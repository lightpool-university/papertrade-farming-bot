use alloy::primitives::{B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use lightpool_sdk::lightpool_types::{
    eip712_hash_for_tx_digest, AuthScheme, Signature, SignedTransaction, TransactionReceipt,
};
use lightpool_sdk::{
    parse_token_contract, ActionBuilder, Address, PaperClaimParams, PaperCloseParams,
    PaperOpenParams, PaperStakeParams, SetAgentParams, Signer,
    TransactionBuilder,
};
use log::{info, warn};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

use crate::account::{Account, AccountStore, Paper, Position};

const LIGHTPOOL_SCALE: u128 = 1_000_000;

pub struct Papertrade {
    indexer_url: String,
    ws_url: String,
    http: reqwest::Client,
    wallet: PrivateKeySigner,
    wallet_addr: Address,
    agent: Signer,
    user: String,
    agent_addr: String,
    user_addr: Address,
    market: String,
    market_contract: lightpool_sdk::lightpool_types::ContractAddress,
    instrument: u32,
    authorized: bool,
    auth_path: std::path::PathBuf,
    accounts: Option<AccountStore>,
}

#[derive(Deserialize)]
struct SubmitResponse {
    receipt: TransactionReceipt,
}

impl Papertrade {
    pub fn new(
        ws_url: String,
        http: reqwest::Client,
        wallet: PrivateKeySigner,
        agent: PrivateKeySigner,
        market: String,
        instrument: u32,
        auth_path: std::path::PathBuf,
    ) -> Result<Self> {
        let wallet_addr = sdk_address(&wallet)?;
        let agent_signer = sdk_signer(&agent)?;
        let user = wallet_addr.to_hex();
        let agent_addr = agent_signer.address().to_hex();
        let authorized = auth_matches(&auth_path, &user, &agent_addr);
        let market_contract =
            parse_token_contract(market.trim()).map_err(|err| anyhow::anyhow!(err))?;
        Ok(Self {
            indexer_url: indexer_http_from_ws(&ws_url),
            ws_url,
            http,
            wallet,
            wallet_addr,
            agent: agent_signer,
            user,
            agent_addr,
            user_addr: wallet_addr,
            market,
            market_contract,
            instrument,
            authorized,
            auth_path,
            accounts: None,
        })
    }

    pub fn user_address(&self) -> &str {
        &self.user
    }

    pub fn agent_address(&self) -> &str {
        &self.agent_addr
    }

    pub fn authorized(&self) -> bool {
        self.authorized
    }

    pub async fn register_agent(&mut self) -> Result<()> {
        if self.authorized {
            return Ok(());
        }
        info!("setting agent {}", self.agent_addr);
        let action = ActionBuilder::set_agent(SetAgentParams {
            agent: self.agent.address(),
        })
        .context("build set_agent")?;
        let tx = TransactionBuilder::new()
            .sender(self.wallet_addr)
            .expiration(u64::MAX)
            .add_action(action)
            .build()
            .context("build set_agent tx")?;
        let hash = eip712_hash_for_tx_digest(&tx.digest());
        let signature = sign_eip712(&self.wallet, &hash)?;
        let signed = SignedTransaction::new_with_scheme(tx, signature, AuthScheme::Eip712);
        self.submit_tx(signed).await?;
        if let Some(parent) = self.auth_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.auth_path, format!("{}\n{}\n", self.user, self.agent_addr))?;
        self.authorized = true;
        Ok(())
    }

    pub async fn submit_open(&mut self, is_long: bool, size: u128, leverage: u32) -> Result<()> {
        let margin = u64::try_from(size).context("margin does not fit")?;
        let action = ActionBuilder::open_paper(
            self.market_contract,
            PaperOpenParams {
                instrument: self.instrument,
                is_long,
                margin,
                leverage: u64::from(leverage),
                fund_from_queued: false,
            },
        )
        .context("build open_paper")?;
        let submitted = self.submit_tx(self.agent_tx(action)?).await?;
        if let Some(opened) = lightpool_sdk::paper_events::extract_paper_opened(&submitted.receipt)
        {
            self.note_open(is_long, opened.position, U256::from(size)).await;
        }
        Ok(())
    }

    pub async fn submit_close(&mut self, ids: &[String]) -> Result<()> {
        for id in ids {
            let position: u64 = id.parse().context("lightpool position id")?;
            let action = ActionBuilder::close_paper(
                self.market_contract,
                PaperCloseParams { position },
            )
            .context("build close_paper")?;
            self.submit_tx(self.agent_tx(action)?).await?;
            self.note_close(id).await;
        }
        Ok(())
    }

    pub async fn submit_stake(&mut self, amount: U256, _balance: U256) -> Result<()> {
        let raw: u64 = amount.try_into().context("stake amount does not fit")?;
        let action = ActionBuilder::stake_paper(self.market_contract, PaperStakeParams { amount: raw })
            .context("build stake_paper")?;
        self.submit_tx(self.agent_tx(action)?).await?;
        self.note_stake(amount).await;
        Ok(())
    }

    pub async fn submit_claim(&mut self) -> Result<()> {
        let action = ActionBuilder::claim_paper(self.market_contract, PaperClaimParams {})
            .context("build claim_paper")?;
        self.submit_tx(self.agent_tx(action)?).await?;
        self.note_claim().await;
        Ok(())
    }

    fn agent_tx(&self, action: lightpool_sdk::Action) -> Result<SignedTransaction> {
        TransactionBuilder::new()
            .sender(self.agent.address())
            .account(self.user_addr)
            .expiration(u64::MAX)
            .add_action(action)
            .build_and_sign_only(&self.agent)
            .context("sign agent tx")
    }

    async fn submit_tx(&self, tx: SignedTransaction) -> Result<SubmitResponse> {
        let url = format!("{}/tx/submit", self.indexer_url);
        let resp = self.http.post(&url).json(&SubmitBody { tx }).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("submit {status} {text}");
        }
        let submitted: SubmitResponse = serde_json::from_str(&text).context("decode submit")?;
        if !submitted.receipt.is_success() {
            bail!("transaction failed: {:?}", submitted.receipt.status);
        }
        info!("submitted to {}", self.indexer_url);
        Ok(submitted)
    }

    async fn note_open(&self, is_long: bool, id: u64, margin: U256) {
        let Some(store) = &self.accounts else {
            return;
        };
        let mut guard = store.write().await;
        let account = guard.get_or_insert_with(blank_account);
        let id = id.to_string();
        if account.positions.iter().any(|p| p.id == id) {
            return;
        }
        account.available = account.available.saturating_sub(margin);
        account.positions.push(Position {
            id,
            is_long,
            entry: 0.0,
            bust: 0.0,
        });
    }

    async fn note_close(&self, id: &str) {
        let Some(store) = &self.accounts else {
            return;
        };
        let mut guard = store.write().await;
        if let Some(account) = guard.as_mut() {
            account.positions.retain(|p| p.id != id);
        }
    }

    async fn note_stake(&self, amount: U256) {
        let Some(store) = &self.accounts else {
            return;
        };
        let mut guard = store.write().await;
        let account = guard.get_or_insert_with(blank_account);
        account.paper.balance = account.paper.balance.saturating_sub(amount);
        account.paper.staked = account.paper.staked.saturating_add(amount);
    }

    async fn note_claim(&self) {
        let Some(store) = &self.accounts else {
            return;
        };
        let mut guard = store.write().await;
        if let Some(account) = guard.as_mut() {
            account.paper.claimable = U256::ZERO;
        }
    }

    pub fn subscribe_account(&mut self, store: AccountStore, cancel: CancellationToken) {
        self.accounts = Some(store.clone());
        let ws_url = self.ws_url.clone();
        let user = self.user.clone();
        let market = self.market.clone();
        let instrument = self.instrument;
        tokio::spawn(async move {
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                if let Err(err) =
                    read_user_state(&ws_url, &user, &market, instrument, &store, &cancel).await
                {
                    warn!("paper user state ended: {err:#}");
                    store.write().await.take();
                }
                if cancel.is_cancelled() {
                    break;
                }
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                }
            }
        });
    }
}

#[async_trait::async_trait]
impl crate::venue::Venue for Papertrade {
    fn is_session_authed(&self, _account: &Account) -> bool {
        self.authorized()
    }

    async fn ensure_auth(&mut self) -> Result<()> {
        self.register_agent().await
    }

    fn venue_signer_label(&self) -> String {
        format!("agent {}", self.agent_address())
    }

    async fn open_hedge_position(&mut self, size: u128, leverage: u32) -> Result<()> {
        self.submit_open(true, size, leverage).await?;
        self.submit_open(false, size, leverage).await
    }

    async fn close_open_positions(&mut self, ids: &[String]) -> Result<()> {
        self.submit_close(ids).await
    }

    async fn cancel_inflight_intent(&mut self, _intent_id: &str) -> Result<()> {
        Ok(())
    }

    async fn stake_paper_balance(&mut self, amount: U256, balance: U256) -> Result<()> {
        self.submit_stake(amount, balance).await
    }

    async fn claim_paper_rewards(&mut self) -> Result<()> {
        self.submit_claim().await
    }
}

async fn read_user_state(
    ws_url: &str,
    user: &str,
    market: &str,
    instrument: u32,
    store: &AccountStore,
    cancel: &CancellationToken,
) -> Result<()> {
    let (ws, _) = connect_async(ws_url).await?;
    let (mut write, mut read) = ws.split();
    let sub = json!({
        "op": "subscribe",
        "channel": "paper.user.state",
        "user_address": user,
        "market": market,
    });
    write.send(Message::Text(sub.to_string().into())).await?;
    info!("subscribed paper.user.state user={user} market={market}");
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            msg = read.next() => {
                let Some(msg) = msg else { return Ok(()) };
                let msg = msg?;
                let Message::Text(text) = msg else { continue };
                apply_state(store, &text, instrument).await?;
            }
        }
    }
}

async fn apply_state(store: &AccountStore, text: &str, instrument: u32) -> Result<()> {
    let value: Value = serde_json::from_str(text)?;
    if value.get("type").and_then(|v| v.as_str()) != Some("paper.user.state") {
        return Ok(());
    }
    let account_v = value.get("account").cloned().unwrap_or(Value::Null);
    let paper_v = value.get("paper").cloned().unwrap_or(Value::Null);
    let positions = value
        .get("positions")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| parse_position(row, instrument))
                .collect()
        })
        .unwrap_or_default();
    let account = Account {
        revision: 0,
        available: json_u256(account_v.get("available_raw")),
        paper: Paper {
            balance: json_u256(paper_v.get("paper_raw")),
            staked: json_u256(paper_v.get("staked_raw")),
            pending_base: U256::ZERO,
            reward_debt: U256::ZERO,
            claimable: json_u256(paper_v.get("claimable_raw")),
        },
        session_keys: Vec::new(),
        positions,
        inflight: Vec::new(),
        token_scale: LIGHTPOOL_SCALE,
        indexed_claim: true,
    };
    info!(
        "paper user state positions={} available={} paper={}",
        account.positions.len(),
        account.available,
        account.paper.balance
    );
    *store.write().await = Some(account);
    Ok(())
}

fn parse_position(value: &Value, instrument: u32) -> Option<Position> {
    if value.get("instrument").and_then(|v| v.as_u64())? as u32 != instrument {
        return None;
    }
    let id = match value.get("position")? {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return None,
    };
    Some(Position {
        id,
        is_long: value.get("is_long").and_then(|v| v.as_bool()).unwrap_or(false),
        entry: value
            .get("entry_price")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
        bust: value
            .get("bust_price")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
    })
}

fn blank_account() -> Account {
    Account {
        revision: 0,
        available: U256::ZERO,
        paper: Paper {
            balance: U256::ZERO,
            staked: U256::ZERO,
            pending_base: U256::ZERO,
            reward_debt: U256::ZERO,
            claimable: U256::ZERO,
        },
        session_keys: Vec::new(),
        positions: Vec::new(),
        inflight: Vec::new(),
        token_scale: LIGHTPOOL_SCALE,
        indexed_claim: true,
    }
}

fn auth_matches(path: &std::path::Path, user: &str, agent: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut lines = raw.lines();
    lines.next() == Some(user) && lines.next() == Some(agent)
}

#[derive(serde::Serialize)]
struct SubmitBody {
    tx: SignedTransaction,
}

fn indexer_http_from_ws(ws_url: &str) -> String {
    let trimmed = ws_url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/ws").unwrap_or(trimmed);
    if let Some(rest) = trimmed.strip_prefix("ws://") {
        format!("http://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        format!("https://{rest}")
    } else {
        trimmed.to_string()
    }
}

fn sdk_signer(key: &PrivateKeySigner) -> Result<Signer> {
    let bytes: [u8; 32] = key.to_bytes().into();
    Signer::from_secret_key_bytes(&bytes).context("lightpool signer")
}

fn sdk_address(key: &PrivateKeySigner) -> Result<Address> {
    Ok(sdk_signer(key)?.address())
}

fn sign_eip712(wallet: &PrivateKeySigner, hash: &[u8; 32]) -> Result<Signature> {
    let signature = wallet.sign_hash_sync(&B256::from(*hash))?;
    let mut raw = [0u8; 64];
    raw[..32].copy_from_slice(&signature.r().to_be_bytes::<32>());
    raw[32..].copy_from_slice(&signature.s().to_be_bytes::<32>());
    bincode::deserialize(&raw).context("eip712 signature")
}

fn json_u256(value: Option<&Value>) -> U256 {
    let Some(value) = value else {
        return U256::ZERO;
    };
    match value {
        Value::String(s) => s.parse().unwrap_or(U256::ZERO),
        Value::Number(n) => n.to_string().parse().unwrap_or(U256::ZERO),
        _ => U256::ZERO,
    }
}
