use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "papertrade-farming-bot")]
pub struct Config {
    #[arg(long, env = "PAPERTRADE_WALLET_KEY")]
    pub wallet_key: String,

    #[arg(long, env = "PAPERTRADE_DATA_DIR", default_value = "data")]
    pub data_dir: PathBuf,

    #[arg(long, env = "PAPERTRADE_HTTP_URL", default_value = "https://exchange.papertrade.xyz")]
    pub http_url: String,

    #[arg(long, env = "PAPERTRADE_WS_URL", default_value = "ws://127.0.0.1:3002/ws")]
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

    #[arg(long, env = "PAPER_MARKET", default_value = "")]
    pub paper_market: String,

    #[arg(long, env = "PAPER_INSTRUMENT", default_value_t = 0)]
    pub instrument: u32,
}

pub const REWARD_SCALE: u128 = 1_000_000_000_000_000_000_000_000_000;
