mod account;
mod adapters;
mod config;
mod price;
mod strategy;
mod venue;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::str::FromStr;

use alloy::signers::local::PrivateKeySigner;
use anyhow::{bail, Context, Result};
use clap::Parser;
use log::info;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

use crate::account::AccountStore;
use crate::adapters::{hyperliquid, lightpool};
use crate::config::Config;
use crate::price::{spawn_price_feed, PriceTick};
use crate::strategy::Strategy;
use crate::venue::Venue;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cfg = Config::parse();
    fs::create_dir_all(&cfg.data_dir)?;

    let wallet = signer_from_key(&cfg.wallet_key).context("wallet key")?;
    let http = reqwest::Client::builder().build()?;
    let accounts: AccountStore = std::sync::Arc::new(RwLock::new(None));
    let cancel = CancellationToken::new();

    let venue: Box<dyn Venue> = match cfg.venue.to_ascii_lowercase().as_str() {
        "papertrade_on_hyperliquid" | "exchange" => {
            let session = load_key(&cfg.data_dir.join("session.key"))?;
            let venue = hyperliquid::Papertrade::new(
                cfg.http_url.clone(),
                http,
                wallet,
                session,
                cfg.data_dir.join("nonces.json"),
            )?;
            info!(
                "venue=papertrade_on_hyperliquid wallet {} session {}",
                venue.wallet_address(),
                venue.session_address()
            );
            venue.subscribe_account(cfg.http_url.clone(), accounts.clone(), cancel.clone());
            Box::new(venue)
        }
        "papertrade_on_lightpool" | "lightpool" => {
            if cfg.paper_market.trim().is_empty() {
                bail!("PAPER_MARKET is required for papertrade_on_lightpool");
            }
            let agent = load_key(&cfg.data_dir.join("agent.key"))?;
            let mut venue = lightpool::Papertrade::new(
                cfg.ws_url.clone(),
                http,
                wallet,
                agent,
                cfg.paper_market.clone(),
                cfg.instrument,
                cfg.data_dir.join("agent.auth"),
            )?;
            info!(
                "venue=papertrade_on_lightpool user {} agent {}",
                venue.user_address(),
                venue.agent_address()
            );
            venue.subscribe_account(accounts.clone(), cancel.clone());
            Box::new(venue)
        }
        other => bail!("unknown venue {other}"),
    };

    let (tx, mut rx) = mpsc::channel::<PriceTick>(32);
    spawn_price_feed(
        cfg.hyperliquid_ws_url.clone(),
        cfg.hyperliquid_info_url.clone(),
        cfg.coin.clone(),
        cfg.price_stale_ms,
        tx,
        cancel.clone(),
    );

    let mut strategy = Strategy::new(cfg, venue, accounts);
    loop {
        let tick = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            tick = rx.recv() => {
                let Some(tick) = tick else { break };
                tick
            }
        };
        let mut latest = tick;
        while let Ok(newer) = rx.try_recv() {
            latest = newer;
        }
        strategy.on_tick(latest).await;
    }
    cancel.cancel();
    info!("stopped");
    Ok(())
}

fn signer_from_key(raw: &str) -> Result<PrivateKeySigner> {
    let key = raw.trim();
    let key = if key.starts_with("0x") || key.starts_with("0X") {
        key.to_string()
    } else {
        format!("0x{key}")
    };
    Ok(PrivateKeySigner::from_str(&key)?)
}

fn load_key(path: &Path) -> Result<PrivateKeySigner> {
    if path.exists() {
        let raw = fs::read_to_string(path)?;
        return signer_from_key(&raw);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let signer = PrivateKeySigner::random();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{:#x}", signer.to_bytes())?;
    Ok(signer)
}
