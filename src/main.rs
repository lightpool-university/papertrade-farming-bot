mod account;
mod adapters;
mod config;
mod price;
mod strategy;
mod ui;
mod venue;

use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use alloy::signers::local::PrivateKeySigner;
use anyhow::{bail, Context, Result};
use clap::Parser;
use log::{info, warn};
use serde_json::{json, Value};
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

use crate::account::AccountStore;
use crate::adapters::{hyperliquid, lightpool};
use crate::config::{
    Config, HYPERLIQUID_DEFAULT_HTTP_URL, LIGHTPOOL_DEFAULT_HTTP_URL, LIGHTPOOL_DEFAULT_WALLET_KEY,
};
use crate::price::{spawn_price_feed, PriceTick};
use crate::strategy::Strategy;
use crate::ui::{AppUi, LogBuffer};
use crate::venue::Venue;

#[tokio::main]
async fn main() -> Result<()> {
    let logs = LogBuffer::new();
    logs.clone().install()?;
    let cfg = Config::parse();
    fs::create_dir_all(&cfg.data_dir)?;
    ensure_hyperliquid_access(&cfg.hyperliquid_info_url, &cfg.data_dir).await?;

    let mut ui = AppUi::enter(logs)?;
    let venue_name = cfg.venue.to_ascii_lowercase();
    let wallet_key = match cfg.wallet_key.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(key) => key.to_string(),
        None if matches!(
            venue_name.as_str(),
            "papertrade_on_lightpool" | "lightpool"
        ) =>
        {
            info!(
                "PAPERTRADE_WALLET_KEY unset; using Anvil account #2 {}",
                "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"
            );
            LIGHTPOOL_DEFAULT_WALLET_KEY.to_string()
        }
        None => bail!("PAPERTRADE_WALLET_KEY is required for {venue_name}"),
    };
    let wallet = signer_from_key(&wallet_key).context("wallet key")?;
    let http_url = match cfg.http_url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(url) => url.to_string(),
        None if matches!(
            venue_name.as_str(),
            "papertrade_on_lightpool" | "lightpool"
        ) =>
        {
            LIGHTPOOL_DEFAULT_HTTP_URL.to_string()
        }
        None => HYPERLIQUID_DEFAULT_HTTP_URL.to_string(),
    };
    let http = reqwest::Client::builder().build()?;
    let accounts: AccountStore = std::sync::Arc::new(RwLock::new(None));
    let cancel = CancellationToken::new();

    let venue: Box<dyn Venue> = match venue_name.as_str() {
        "papertrade_on_hyperliquid" | "exchange" => {
            let session = load_key(&cfg.data_dir.join("session.key"))?;
            let venue = hyperliquid::Papertrade::new(
                http_url.clone(),
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
            venue.subscribe_account(http_url.clone(), accounts.clone(), cancel.clone());
            Box::new(venue)
        }
        "papertrade_on_lightpool" | "lightpool" => {
            if cfg.paper_market.trim().is_empty() {
                bail!("PAPER_MARKET is required for papertrade_on_lightpool");
            }
            let agent = load_key(&cfg.data_dir.join("agent.key"))?;
            let mut venue = lightpool::Papertrade::new(
                http_url,
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

    let mut strategy = Strategy::new(cfg, venue, accounts.clone());
    let mut last_px = None;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            tick = rx.recv() => {
                let Some(tick) = tick else { break };
                let mut latest = tick;
                while let Ok(newer) = rx.try_recv() {
                    latest = newer;
                }
                last_px = Some(latest.px);
                strategy.on_tick(latest).await;
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
        if ui.poll_quit()? {
            break;
        }
        ui.draw(&accounts, last_px)?;
    }
    cancel.cancel();
    info!("stopped");
    Ok(())
}

async fn ensure_hyperliquid_access(info_url: &str, data_dir: &Path) -> Result<()> {
    let proxy_path = data_dir.join("hl_proxy.env");
    load_saved_hl_proxy(&proxy_path);

    info!("check Hyperliquid ({info_url})");
    if hyperliquid_reachable(info_url).await {
        if let Ok(proxy) = std::env::var("HTTPS_PROXY") {
            if !proxy.trim().is_empty() {
                info!("Hyperliquid reachable (proxy={proxy})");
                return Ok(());
            }
        }
        info!("Hyperliquid reachable (direct)");
        return Ok(());
    }

    warn!("Hyperliquid unreachable — price feed needs it");
    if let Ok(proxy) = std::env::var("HTTPS_PROXY") {
        if !proxy.trim().is_empty() {
            warn!("current HTTPS_PROXY={proxy} still cannot reach Hyperliquid");
        }
    } else {
        warn!("no HTTPS_PROXY/HTTP_PROXY/ALL_PROXY/HL_PROXY in the environment");
    }

    let proxy = if let Ok(proxy) = std::env::var("HL_PROXY") {
        let proxy = proxy.trim().to_string();
        if proxy.is_empty() {
            bail!("HL_PROXY is empty");
        }
        proxy
    } else if io::stdin().is_terminal() {
        eprint!("Enter proxy URL (e.g. http://127.0.0.1:7890), empty to abort: ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("read proxy from stdin")?;
        let proxy = line.trim().to_string();
        if proxy.is_empty() {
            bail!("aborted: proxy required for Hyperliquid price feed");
        }
        proxy
    } else {
        bail!("non-interactive shell: export HL_PROXY or HTTPS_PROXY and re-run");
    };

    apply_proxy_url(&proxy);
    info!("retry Hyperliquid via proxy={proxy}");
    if !hyperliquid_reachable(info_url).await {
        bail!("still cannot reach Hyperliquid via {proxy}; check the proxy and re-run");
    }
    save_hl_proxy(&proxy_path, &proxy)?;
    info!("Hyperliquid reachable via proxy (saved {})", proxy_path.display());
    Ok(())
}

async fn hyperliquid_reachable(info_url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
    else {
        return false;
    };
    let Ok(resp) = client
        .post(info_url)
        .json(&json!({"type": "allMids"}))
        .send()
        .await
    else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    let Ok(value) = resp.json::<Value>().await else {
        return false;
    };
    value
        .as_object()
        .or_else(|| value.get("mids").and_then(|v| v.as_object()))
        .is_some_and(|obj| obj.contains_key("BTC") || obj.keys().any(|k| k.eq_ignore_ascii_case("btc")))
}

fn apply_proxy_url(proxy: &str) {
    for key in [
        "HL_PROXY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        std::env::set_var(key, proxy);
    }
}

fn load_saved_hl_proxy(path: &Path) {
    if let Ok(proxy) = std::env::var("HL_PROXY") {
        let proxy = proxy.trim();
        if !proxy.is_empty() {
            apply_proxy_url(proxy);
            return;
        }
    }
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !matches!(
            key.trim(),
            "HL_PROXY"
                | "HTTPS_PROXY"
                | "HTTP_PROXY"
                | "ALL_PROXY"
                | "https_proxy"
                | "http_proxy"
                | "all_proxy"
        ) {
            continue;
        }
        let value = value
            .trim()
            .trim_matches('\'')
            .trim_matches('"')
            .trim();
        if !value.is_empty() {
            apply_proxy_url(value);
            info!("proxy loaded from {}", path.display());
            return;
        }
    }
}

fn save_hl_proxy(path: &PathBuf, proxy: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = format!(
        "# Written by papertrade-farming-bot when Hyperliquid required a proxy.\n\
export HL_PROXY='{proxy}'\n\
export HTTP_PROXY='{proxy}'\n\
export HTTPS_PROXY='{proxy}'\n\
export ALL_PROXY='{proxy}'\n\
export http_proxy='{proxy}'\n\
export https_proxy='{proxy}'\n\
export all_proxy='{proxy}'\n"
    );
    fs::write(path, body).with_context(|| format!("write {}", path.display()))
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
