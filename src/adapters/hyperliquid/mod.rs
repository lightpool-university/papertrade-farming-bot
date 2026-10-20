use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, U256};
use alloy::signers::local::PrivateKeySigner;
use anyhow::Result;
use futures_util::StreamExt;
use log::warn;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::account::{
    Account, AccountStore, Inflight, Paper, Position, DEFAULT_MINT_RATE_RAW,
};
use crate::venue::Venue;

use self::intent::IntentClient;
use self::nonce::Nonces;

mod intent;
mod nonce;

const BTC_INSTRUMENT_ID: u32 = 0;
const BTC_PRICE_SCALE: f64 = 10.0;
const WAD: u128 = 1_000_000_000_000_000_000;

pub struct Papertrade {
    intents: IntentClient,
}

impl Papertrade {
    pub fn new(
        base_url: String,
        http: reqwest::Client,
        wallet: PrivateKeySigner,
        session: PrivateKeySigner,
        nonces_path: PathBuf,
    ) -> Result<Self> {
        Ok(Self {
            intents: IntentClient::new(base_url, http, wallet, session, Nonces::load(nonces_path)?),
        })
    }

    pub fn wallet_address(&self) -> Address {
        self.intents.wallet_address()
    }

    pub fn session_address(&self) -> Address {
        self.intents.session_address()
    }

    pub fn subscribe_account(&self, base_url: String, store: AccountStore, cancel: CancellationToken) {
        let wallet = format!("{:#x}", self.wallet_address());
        tokio::spawn(async move {
            let http = reqwest::Client::builder()
                .build()
                .expect("http client");
            let url = format!(
                "{}/state/user/live?wallet={}",
                base_url.trim_end_matches('/'),
                wallet
            );
            loop {
                if cancel.is_cancelled() {
                    break;
                }
                if let Err(err) = read_stream(&http, &url, &store, &cancel).await {
                    warn!("account stream ended: {err:#}");
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
impl Venue for Papertrade {
    fn is_session_authed(&self, account: &Account) -> bool {
        let addr = format!("{:#x}", self.intents.session_address());
        let now = now_secs();
        account
            .session_keys
            .iter()
            .any(|(key, expiry)| key == &addr && *expiry > now + 86_400)
    }

    async fn ensure_auth(&mut self) -> Result<()> {
        self.intents.register_session().await
    }

    fn venue_signer_label(&self) -> String {
        format!("session {}", self.intents.session_address())
    }

    async fn open_hedge_position(&mut self, size: u128, leverage: u32) -> Result<()> {
        self.intents.open(true, size, leverage).await?;
        self.intents.open(false, size, leverage).await
    }

    async fn close_open_positions(&mut self, ids: &[String]) -> Result<()> {
        self.intents.close(ids).await
    }

    async fn cancel_inflight_intent(&mut self, intent_id: &str) -> Result<()> {
        self.intents.cancel(intent_id).await
    }

    async fn stake_paper_balance(&mut self, amount: U256, _balance: U256) -> Result<()> {
        self.intents.stake(amount).await
    }

    async fn claim_paper_rewards(&mut self) -> Result<()> {
        self.intents.claim().await
    }

    async fn acc_reward_per_share(&mut self) -> Result<Option<U256>> {
        Ok(Some(self.intents.acc_reward_per_share().await?))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn read_stream(
    http: &reqwest::Client,
    url: &str,
    store: &AccountStore,
    cancel: &CancellationToken,
) -> Result<()> {
    let resp = http
        .get(url)
        .header("accept", "text/event-stream")
        .send()
        .await?
        .error_for_status()?;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut event = String::new();
    let mut data = String::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            chunk = stream.next() => {
                let Some(chunk) = chunk else { return Ok(()) };
                buf.push_str(&String::from_utf8_lossy(&chunk?));
                while let Some(idx) = buf.find('\n') {
                    let mut line = buf.drain(..=idx).collect::<String>();
                    if line.ends_with('\n') {
                        line.pop();
                    }
                    if line.ends_with('\r') {
                        line.pop();
                    }
                    if line.is_empty() {
                        if !data.is_empty() && event == "wallet" {
                            apply_wallet(store, &data).await?;
                        }
                        event.clear();
                        data.clear();
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix("event:") {
                        event = rest.trim().to_string();
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(rest.trim_start());
                    }
                }
            }
        }
    }
}

async fn apply_wallet(store: &AccountStore, data: &str) -> Result<()> {
    let value: Value = serde_json::from_str(data)?;
    let revision = json_u64(value.get("accountRevision")).unwrap_or(0);
    let mut guard = store.write().await;
    if value.get("baseAccountRevision").is_some() {
        let base = json_u64(value.get("baseAccountRevision")).unwrap_or(0);
        let Some(prev) = guard.as_ref() else {
            return Ok(());
        };
        if base != prev.revision || revision <= prev.revision {
            anyhow::bail!("account revision gap base={base} prev={} next={revision}", prev.revision);
        }
        let mut next = prev.clone();
        next.revision = revision;
        merge_state(&mut next, value.get("state"));
        if let Some(paper) = value.pointer("/state/paper") {
            next.paper = parse_paper(paper);
        }
        let removed: Vec<String> = value
            .get("removedPositionIds")
            .and_then(|v| v.as_array())
            .map(|ids| ids.iter().filter_map(json_string).collect())
            .unwrap_or_default();
        next.positions.retain(|p| !removed.iter().any(|id| id == &p.id));
        if let Some(added) = value.get("addedPositions").and_then(|v| v.as_array()) {
            for item in added {
                if let Some(pos) = parse_position(item) {
                    next.positions.push(pos);
                }
            }
        }
        let removed_ops: Vec<String> = value
            .get("removedOperationIds")
            .and_then(|v| v.as_array())
            .map(|ids| ids.iter().filter_map(json_string).collect())
            .unwrap_or_default();
        next.inflight
            .retain(|op| !removed_ops.iter().any(|id| id == &op.intent_id));
        if let Some(added) = value.get("addedOperations").and_then(|v| v.as_array()) {
            for item in added {
                if let Some(op) = parse_inflight(item) {
                    next.inflight.push(op);
                }
            }
        }
        *guard = Some(next);
        return Ok(());
    }

    let account = parse_snapshot(&value, revision);
    *guard = Some(account);
    Ok(())
}

fn parse_snapshot(value: &Value, revision: u64) -> Account {
    let mut account = Account {
        revision,
        available: json_u256(value.pointer("/funds/availableBalance")),
        paper: parse_paper(value.get("paper").unwrap_or(&Value::Null)),
        session_keys: Vec::new(),
        positions: Vec::new(),
        inflight: Vec::new(),
        token_scale: WAD,
        indexed_claim: false,
        mint_rate_raw: DEFAULT_MINT_RATE_RAW,
    };
    if let Some(keys) = value.get("sessionKeys").and_then(|v| v.as_array()) {
        account.session_keys = parse_keys(keys);
    }
    if let Some(rows) = value.get("openPositions").and_then(|v| v.as_array()) {
        account.positions = rows.iter().filter_map(parse_position).collect();
    }
    if let Some(rows) = value.get("operations").and_then(|v| v.as_array()) {
        account.inflight = rows.iter().filter_map(parse_inflight).collect();
    }
    account
}

fn merge_state(account: &mut Account, state: Option<&Value>) {
    let Some(state) = state else { return };
    if let Some(v) = state.pointer("/funds/availableBalance") {
        account.available = json_u256(Some(v));
    }
    if let Some(keys) = state.get("sessionKeys").and_then(|v| v.as_array()) {
        account.session_keys = parse_keys(keys);
    }
}

fn parse_keys(keys: &[Value]) -> Vec<(String, u64)> {
    keys.iter()
        .filter_map(|row| {
            let addr = json_string(row.get(0)?)?.to_lowercase();
            let expiry = json_u64(row.get(1))?;
            Some((addr, expiry))
        })
        .collect()
}

fn parse_paper(value: &Value) -> Paper {
    Paper {
        balance: json_u256(value.get("balance")),
        staked: json_u256(value.get("staked")),
        pending_base: json_u256(value.get("pendingBase")),
        reward_debt: json_u256(value.get("rewardDebt")),
        claimable: U256::ZERO,
    }
}

fn parse_position(value: &Value) -> Option<Position> {
    let row = value.as_array()?;
    let instrument_id = json_u64(row.get(1))? as u32;
    if instrument_id != BTC_INSTRUMENT_ID {
        return None;
    }
    let entry = json_u128(row.get(4))? as f64 / BTC_PRICE_SCALE;
    let margin = json_u128(row.get(5))? as f64 / WAD as f64;
    let leverage = json_u64(row.get(6)).unwrap_or(1).max(1) as f64;
    let notional = margin * leverage;
    let size = if entry > 0.0 { notional / entry } else { 0.0 };
    Some(Position {
        id: json_string(row.first()?)?,
        is_long: row.get(2).and_then(|v| v.as_bool()).unwrap_or(false),
        entry,
        bust: json_u128(row.get(7))? as f64 / BTC_PRICE_SCALE,
        margin,
        size,
        notional,
    })
}

fn parse_inflight(value: &Value) -> Option<Inflight> {
    let row = value.as_array()?;
    let action = row.get(1).and_then(|v| v.as_str())?.to_string();
    let instrument_id = match action.as_str() {
        "openPosition" => Some(json_u64(row.get(2)).unwrap_or(0) as u32),
        _ => None,
    };
    Some(Inflight {
        intent_id: json_string(row.first()?)?,
        action,
        instrument_id,
        accepted_at_ms: json_u64(row.get(7)).unwrap_or(0),
    })
}

fn json_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn json_u64(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn json_u128(value: Option<&Value>) -> Option<u128> {
    match value? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_u64().map(u128::from),
        _ => None,
    }
}

fn json_u256(value: Option<&Value>) -> U256 {
    let Some(value) = value else {
        return U256::ZERO;
    };
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return U256::ZERO,
    };
    raw.parse().unwrap_or(U256::ZERO)
}
