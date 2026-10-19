use alloy::primitives::{Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use alloy::sol_types::{eip712_domain, SolStruct};
use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use log::{info, warn};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

use crate::account::{Account, AccountStore, Paper, Position};

const LIGHTPOOL_SCALE: u128 = 1_000_000;

alloy::sol! {
    struct LightPoolTx {
        bytes32 digest;
    }
}

pub struct Papertrade {
    app_url: String,
    http: reqwest::Client,
    wallet: PrivateKeySigner,
    agent: PrivateKeySigner,
    user: String,
    agent_addr: String,
    market: String,
    instrument: u32,
    authorized: bool,
    auth_path: std::path::PathBuf,
    accounts: Option<AccountStore>,
}

#[derive(Deserialize)]
struct PreparedTx {
    digest_hex: String,
    unsigned_tx_hex: String,
    auth_scheme: Option<String>,
}

#[derive(Deserialize)]
struct SubmittedTx {
    #[serde(default)]
    position: Option<u64>,
}

impl Papertrade {
    pub fn new(
        app_url: String,
        http: reqwest::Client,
        wallet: PrivateKeySigner,
        agent: PrivateKeySigner,
        market: String,
        instrument: u32,
        auth_path: std::path::PathBuf,
    ) -> Self {
        let user = format!("{:#x}", wallet.address());
        let agent_addr = format!("{:#x}", agent.address());
        let authorized = auth_matches(&auth_path, &user, &agent_addr);
        Self {
            app_url: app_url.trim_end_matches('/').to_string(),
            http,
            wallet,
            agent,
            user,
            agent_addr,
            market,
            instrument,
            authorized,
            auth_path,
            accounts: None,
        }
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
        self.post_prepare(
            "/api/agent/prepare",
            json!({ "user": self.user, "agent": self.agent_addr }),
            true,
        )
        .await?;
        if let Some(parent) = self.auth_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.auth_path, format!("{}\n{}\n", self.user, self.agent_addr))?;
        self.authorized = true;
        Ok(())
    }

    pub async fn submit_open(&mut self, is_long: bool, size: u128, leverage: u32) -> Result<()> {
        let margin = human_amount(U256::from(size), LIGHTPOOL_SCALE);
        let submitted = self.post_prepare(
            "/api/paper/open/prepare",
            json!({
                "user": self.user,
                "agent": self.agent_addr,
                "market": self.market,
                "instrument": self.instrument,
                "is_long": is_long,
                "margin": margin,
                "leverage": leverage,
                "fund_from_queued": false,
            }),
            false,
        )
        .await?;
        if let Some(id) = submitted.position {
            self.note_open(is_long, id, U256::from(size)).await;
        }
        Ok(())
    }

    pub async fn submit_close(&mut self, ids: &[String]) -> Result<()> {
        for id in ids {
            let position: u64 = id.parse().context("lightpool position id")?;
            self.post_prepare(
                "/api/paper/close/prepare",
                json!({
                    "user": self.user,
                    "agent": self.agent_addr,
                    "market": self.market,
                    "position": position,
                }),
                false,
            )
            .await?;
            self.note_close(id).await;
        }
        Ok(())
    }

    pub async fn submit_stake(&mut self, amount: U256, _balance: U256) -> Result<()> {
        let human = human_amount(amount, LIGHTPOOL_SCALE);
        self.post_prepare(
            "/api/paper/stake/prepare",
            json!({
                "user": self.user,
                "agent": self.agent_addr,
                "market": self.market,
                "amount": human,
            }),
            false,
        )
        .await?;
        self.note_stake(amount).await;
        Ok(())
    }

    pub async fn submit_claim(&mut self) -> Result<()> {
        self.post_prepare(
            "/api/paper/claim/prepare",
            json!({
                "user": self.user,
                "agent": self.agent_addr,
                "market": self.market,
            }),
            false,
        )
        .await?;
        self.note_claim().await;
        Ok(())
    }

    async fn post_prepare(&self, path: &str, body: Value, wallet_sign: bool) -> Result<SubmittedTx> {
        let url = format!("{}{path}", self.app_url);
        let resp = self.http.post(&url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("{path} {status} {text}");
        }
        let prepared: PreparedTx = serde_json::from_str(&text).with_context(|| path.to_string())?;
        let signature = if wallet_sign || prepared.auth_scheme.as_deref() == Some("eip712") {
            sign_eip712(&self.wallet, &prepared.digest_hex)?
        } else {
            sign_native(&self.agent, &prepared.digest_hex)?
        };
        let scheme = prepared.auth_scheme.unwrap_or_else(|| {
            if wallet_sign {
                "eip712".into()
            } else {
                "native".into()
            }
        });
        let submit = format!("{}/api/tx/sign/submit", self.app_url);
        let resp = self
            .http
            .post(submit)
            .json(&json!({
                "unsigned_tx_hex": prepared.unsigned_tx_hex,
                "signature": signature,
                "auth_scheme": scheme,
            }))
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("submit {status} {text}");
        }
        info!("{path} submitted");
        Ok(serde_json::from_str(&text).unwrap_or(SubmittedTx { position: None }))
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

    pub fn subscribe_account(
        &mut self,
        ws_url: String,
        store: AccountStore,
        cancel: CancellationToken,
    ) {
        self.accounts = Some(store.clone());
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

fn human_amount(raw: U256, scale: u128) -> f64 {
    let scale = U256::from(scale);
    if scale.is_zero() {
        return 0.0;
    }
    let whole = raw / scale;
    let frac = raw % scale;
    let whole_f: f64 = whole.to_string().parse().unwrap_or(0.0);
    let frac_f: f64 = frac.to_string().parse().unwrap_or(0.0);
    whole_f + frac_f / scale.to_string().parse::<f64>().unwrap_or(1.0)
}

fn sign_eip712(wallet: &PrivateKeySigner, digest_hex: &str) -> Result<String> {
    let digest: B256 = digest_hex.parse().context("tx digest")?;
    let msg = LightPoolTx { digest };
    let domain = eip712_domain! {
        name: "LightPool",
        version: "1",
        chain_id: 1337,
        verifying_contract: Address::ZERO,
    };
    let hash = msg.eip712_signing_hash(&domain);
    let signature = wallet.sign_hash_sync(&hash)?;
    Ok(format!("0x{:064x}{:064x}", signature.r(), signature.s()))
}

fn sign_native(agent: &PrivateKeySigner, digest_hex: &str) -> Result<String> {
    let digest = decode_hex(digest_hex)?;
    let hashed = Sha256::digest(&digest);
    let hash = B256::from_slice(&hashed);
    let signature = agent.sign_hash_sync(&hash)?;
    Ok(format!("0x{:064x}{:064x}", signature.r(), signature.s()))
}

fn decode_hex(raw: &str) -> Result<Vec<u8>> {
    let body = raw
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if body.len() % 2 != 0 {
        bail!("odd hex length");
    }
    (0..body.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&body[i..i + 2], 16).context("hex"))
        .collect()
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
