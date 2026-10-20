use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "papertrade-farming-bot")]
pub struct Config {
    #[arg(long, env = "PAPERTRADE_WALLET_KEY")]
    pub wallet_key: Option<String>,

    #[arg(long, env = "PAPERTRADE_DATA_DIR", default_value = "data")]
    pub data_dir: PathBuf,

    #[arg(long, env = "PAPERTRADE_HTTP_URL")]
    pub http_url: Option<String>,

    #[arg(long, env = "PAPERTRADE_WS_URL", default_value = "ws://127.0.0.1:3002/api/ws")]
    pub ws_url: String,

    #[arg(
        long,
        env = "HYPERLIQUID_WS_URL",
        default_value = "wss://api.hyperliquid.xyz/ws"
    )]
    pub hyperliquid_ws_url: String,

    #[arg(
        long,
        env = "HYPERLIQUID_INFO_URL",
        default_value = "https://api.hyperliquid.xyz/info"
    )]
    pub hyperliquid_info_url: String,

    #[arg(long, env = "PAPERTRADE_COIN", default_value = "BTC")]
    pub coin: String,

    #[arg(long, env = "PAPERTRADE_LEVERAGE", default_value_t = 50)]
    pub leverage: u32,

    #[arg(long, env = "PAPERTRADE_TARGET_MOVE_BPS", default_value_t = 60)]
    pub target_move_bps: u64,

    #[arg(long, env = "PAPERTRADE_LIQ_BUFFER_BPS", default_value_t = 40)]
    pub liq_buffer_bps: u64,

    #[arg(long, env = "PAPERTRADE_PRICE_STALE_MS", default_value_t = 1000)]
    pub price_stale_ms: u64,

    #[arg(long, env = "PAPERTRADE_MIN_STAKE", default_value_t = 1000)]
    pub min_stake_paper: u64,

    #[arg(long, env = "PAPERTRADE_VENUE", default_value = "papertrade_on_hyperliquid")]
    pub venue: String,

    #[arg(long, env = "PAPER_MARKET", default_value = "0x0c01000000000001")]
    pub paper_market: String,

    #[arg(long, env = "PAPER_INSTRUMENT", default_value_t = 0)]
    pub instrument: u32,
}

pub const REWARD_SCALE: u128 = 1_000_000_000_000_000_000_000_000_000;

/// Anvil account #2. Used on LightPool when PAPERTRADE_WALLET_KEY is unset.
/// Address: 0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC
pub const LIGHTPOOL_DEFAULT_WALLET_KEY: &str =
    "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";

pub const HYPERLIQUID_DEFAULT_HTTP_URL: &str = "https://exchange.papertrade.xyz";
pub const LIGHTPOOL_DEFAULT_HTTP_URL: &str = "http://127.0.0.1:3002";
