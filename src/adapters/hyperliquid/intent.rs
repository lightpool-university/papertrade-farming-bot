use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{address, Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use alloy::sol_types::{eip712_domain, SolStruct};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::nonce::{Lane, Nonces};

const EXCHANGE: Address = address!("0x6cd5661646289fb6e65ea5c032310fded797d0a2");
const STAKING: Address = address!("0xaad6c7b0cc3014fc80ffedae0ed7ce5967b0f016");
const CHAIN_ID: u64 = 999;

alloy::sol! {
    struct RegisterSessionKey {
        address user;
        address key;
        uint64 expiry;
        address feeAddress;
        uint256 nonce;
        uint64 deadline;
    }
    struct OpenPosition {
        address user;
        uint32 instrumentId;
        bool isLong;
        uint128 size;
        uint32 leverage;
        bool useDebt;
        uint256 nonce;
        uint64 deadline;
    }
    struct Close {
        address user;
        uint256[] positionIds;
        uint256 nonce;
        uint64 deadline;
    }
    struct Stake {
        address user;
        uint256 amount;
        uint256 nonce;
        uint64 deadline;
    }
    struct Claim {
        address user;
        uint256 nonce;
        uint64 deadline;
    }
    struct CancelIntent {
        address user;
        bytes32 intentId;
        uint256 deadline;
    }
}

pub struct IntentClient {
    base_url: String,
    http: reqwest::Client,
    pub wallet: PrivateKeySigner,
    pub session: PrivateKeySigner,
    pub nonces: Nonces,
}

impl IntentClient {
    pub fn new(
        base_url: String,
        http: reqwest::Client,
        wallet: PrivateKeySigner,
        session: PrivateKeySigner,
        nonces: Nonces,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            wallet,
            session,
            nonces,
        }
    }

    pub fn wallet_address(&self) -> Address {
        self.wallet.address()
    }

    pub fn session_address(&self) -> Address {
        self.session.address()
    }

    pub async fn register_session(&mut self) -> Result<()> {
        let expiry = now_secs() + 29 * 24 * 3600;
        let deadline = now_secs() + 600;
        let nonce = self.nonces.take(Lane::Session)?;
        let msg = RegisterSessionKey {
            user: self.wallet.address(),
            key: self.session.address(),
            expiry,
            feeAddress: Address::ZERO,
            nonce,
            deadline,
        };
        let signed = sign_exchange(
            &self.wallet,
            &msg,
            json!({
                "user": addr(self.wallet.address()),
                "key": addr(self.session.address()),
                "expiry": expiry.to_string(),
                "feeAddress": addr(Address::ZERO),
                "nonce": nonce.to_string(),
                "deadline": deadline.to_string(),
            }),
        )?;
        self.post_intent("registerSessionKey", signed).await?;
        Ok(())
    }

    pub async fn open(&mut self, is_long: bool, size: u128, leverage: u32) -> Result<()> {
        let deadline = now_secs() + 600;
        let nonce = self.nonces.take(Lane::Session)?;
        let msg = OpenPosition {
            user: self.wallet.address(),
            instrumentId: 0,
            isLong: is_long,
            size,
            leverage,
            useDebt: false,
            nonce,
            deadline,
        };
        let signed = sign_exchange(
            &self.session,
            &msg,
            json!({
                "user": addr(self.wallet.address()),
                "instrumentId": "0",
                "isLong": is_long,
                "size": size.to_string(),
                "leverage": leverage.to_string(),
                "useDebt": false,
                "nonce": nonce.to_string(),
                "deadline": deadline.to_string(),
            }),
        )?;
        self.post_intent("openPosition", signed).await?;
        Ok(())
    }

    pub async fn close(&mut self, position_ids: &[String]) -> Result<()> {
        let deadline = now_secs() + 600;
        let nonce = self.nonces.take(Lane::Session)?;
        let ids = position_ids
            .iter()
            .map(|id| id.parse())
            .collect::<Result<Vec<U256>, _>>()?;
        let id_wire: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        let msg = Close {
            user: self.wallet.address(),
            positionIds: ids,
            nonce,
            deadline,
        };
        let signed = sign_exchange(
            &self.session,
            &msg,
            json!({
                "user": addr(self.wallet.address()),
                "positionIds": id_wire,
                "nonce": nonce.to_string(),
                "deadline": deadline.to_string(),
            }),
        )?;
        self.post_intent("closePositions", signed).await?;
        Ok(())
    }

    pub async fn cancel(&mut self, intent_id: &str) -> Result<()> {
        let deadline = U256::from(now_secs() + 600);
        let parsed = intent_id.parse::<B256>().context("intent id")?;
        let path_id = format!("{parsed:#x}");
        let msg = CancelIntent {
            user: self.wallet.address(),
            intentId: parsed,
            deadline,
        };
        let mut signed = sign_cancel(
            &self.session,
            &msg,
            json!({
                "user": addr(self.wallet.address()),
                "intentId": path_id.clone(),
                "deadline": deadline.to_string(),
            }),
        )?;
        signed.as_object_mut().context("signed object")?.remove("intentId");
        let url = format!("{}/intents/{path_id}/cancel", self.base_url);
        self.post(&url, signed).await?;
        Ok(())
    }

    pub async fn stake(&mut self, amount: U256) -> Result<()> {
        let deadline = now_secs() + 600;
        let nonce = self.nonces.take(Lane::Paper)?;
        let msg = Stake {
            user: self.wallet.address(),
            amount,
            nonce,
            deadline,
        };
        let signed = sign_staking(
            &self.wallet,
            &msg,
            json!({
                "user": addr(self.wallet.address()),
                "amount": amount.to_string(),
                "nonce": nonce.to_string(),
                "deadline": deadline.to_string(),
            }),
        )?;
        self.post_intent("paperStake", signed).await?;
        Ok(())
    }

    pub async fn claim(&mut self) -> Result<()> {
        let deadline = now_secs() + 600;
        let nonce = self.nonces.take(Lane::Paper)?;
        let msg = Claim {
            user: self.wallet.address(),
            nonce,
            deadline,
        };
        let signed = sign_staking(
            &self.wallet,
            &msg,
            json!({
                "user": addr(self.wallet.address()),
                "nonce": nonce.to_string(),
                "deadline": deadline.to_string(),
            }),
        )?;
        self.post_intent("paperClaim", signed).await?;
        Ok(())
    }

    pub async fn acc_reward_per_share(&self) -> Result<U256> {
        let url = format!("{}/state/trading", self.base_url);
        let value: Value = self.http.get(url).send().await?.error_for_status()?.json().await?;
        let raw = value
            .get("accRewardPerShareRaw")
            .and_then(|v| v.as_str())
            .context("accRewardPerShareRaw")?;
        Ok(raw.parse()?)
    }

    async fn post_intent(&self, action: &str, signed: Value) -> Result<()> {
        let url = format!("{}/intents", self.base_url);
        self.post(&url, json!({"action": action, "signed": signed})).await
    }

    async fn post(&self, url: &str, body: Value) -> Result<()> {
        let resp = self.http.post(url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("{status} {text}");
        }
        Ok(())
    }
}

fn sign_exchange<T: SolStruct>(signer: &PrivateKeySigner, msg: &T, wire: Value) -> Result<Value> {
    let domain = eip712_domain! {
        name: "String",
        version: "1",
        chain_id: CHAIN_ID,
        verifying_contract: EXCHANGE,
    };
    sign_typed(signer, msg, &domain, wire)
}

fn sign_staking<T: SolStruct>(signer: &PrivateKeySigner, msg: &T, wire: Value) -> Result<Value> {
    let domain = eip712_domain! {
        name: "PaperStaking",
        version: "1",
        chain_id: CHAIN_ID,
        verifying_contract: STAKING,
    };
    sign_typed(signer, msg, &domain, wire)
}

fn sign_cancel(signer: &PrivateKeySigner, msg: &CancelIntent, wire: Value) -> Result<Value> {
    let domain = eip712_domain! {
        name: "PaperTrade Intent Cancel",
        version: "2",
        chain_id: CHAIN_ID,
        verifying_contract: EXCHANGE,
    };
    sign_typed(signer, msg, &domain, wire)
}

fn sign_typed<T: SolStruct>(
    signer: &PrivateKeySigner,
    msg: &T,
    domain: &alloy::sol_types::Eip712Domain,
    mut wire: Value,
) -> Result<Value> {
    let hash = msg.eip712_signing_hash(domain);
    let signature = signer.sign_hash_sync(&hash)?;
    let v = if signature.v() { 28u8 } else { 27u8 };
    let obj = wire.as_object_mut().context("message json")?;
    obj.insert("v".to_string(), json!(v.to_string()));
    obj.insert("r".to_string(), json!(format!("0x{:064x}", signature.r())));
    obj.insert("s".to_string(), json!(format!("0x{:064x}", signature.s())));
    Ok(wire)
}

fn addr(address: Address) -> String {
    format!("{address:#x}")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
