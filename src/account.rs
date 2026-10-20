use alloy::primitives::U256;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct Position {
    pub id: String,
    pub is_long: bool,
    pub entry: f64,
    pub bust: f64,
    pub margin: f64,
    pub size: f64,
    pub notional: f64,
}

#[derive(Debug, Clone)]
pub struct Inflight {
    pub intent_id: String,
    pub action: String,
    pub instrument_id: Option<u32>,
    pub accepted_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Paper {
    pub balance: U256,
    pub staked: U256,
    pub pending_base: U256,
    pub reward_debt: U256,
    pub claimable: U256,
}

#[derive(Debug, Clone)]
pub struct Account {
    pub revision: u64,
    pub available: U256,
    pub paper: Paper,
    pub session_keys: Vec<(String, u64)>,
    pub positions: Vec<Position>,
    pub inflight: Vec<Inflight>,
    pub token_scale: u128,
    pub indexed_claim: bool,
    /// PAPER per $1 loss (`rate_ppm / 1e6`). Bootstrap flat default is 100.
    pub mint_rate_raw: u64,
}

/// Bootstrap `flat_rate_ppm` (100 PAPER per $1 loss).
pub const DEFAULT_MINT_RATE_RAW: u64 = 100_000_000;

pub type AccountStore = std::sync::Arc<RwLock<Option<Account>>>;
