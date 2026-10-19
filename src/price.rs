use std::time::Duration;

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use log::{info, warn};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy)]
pub struct PriceTick {
    pub px: f64,
}

pub fn spawn_price_feed(
    ws_url: String,
    info_url: String,
    coin: String,
    stale_ms: u64,
    tx: mpsc::Sender<PriceTick>,
    cancel: CancellationToken,
) {
    let coin = coin.to_uppercase();
    tokio::spawn(async move {
        let (trade_tx, mut trade_rx) = mpsc::channel::<f64>(64);
        let ws_cancel = cancel.clone();
        let ws_coin = coin.clone();
        tokio::spawn(async move {
            run_trades(ws_url, ws_coin, trade_tx, ws_cancel).await;
        });

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("http client");
        let stale = Duration::from_millis(stale_ms);
        let mut last = Instant::now();

        loop {
            let wait = stale.saturating_sub(last.elapsed());
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = trade_rx.recv() => {
                    let Some(px) = msg else { break };
                    last = Instant::now();
                    if tx.send(PriceTick { px }).await.is_err() {
                        break;
                    }
                }
                _ = tokio::time::sleep(wait) => {
                    match poll_mid(&http, &info_url, &coin).await {
                        Ok(px) => {
                            info!("no {coin} trade for {stale_ms}ms, polled price {px}");
                            last = Instant::now();
                            if tx.send(PriceTick { px }).await.is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            warn!("BTC price poll failed: {err:#}");
                            last = Instant::now();
                        }
                    }
                }
            }
        }
    });
}

async fn run_trades(
    ws_url: String,
    coin: String,
    tx: mpsc::Sender<f64>,
    cancel: CancellationToken,
) {
    loop {
        if cancel.is_cancelled() {
            break;
        }
        if let Err(err) = trades_session(&ws_url, &coin, &tx, &cancel).await {
            warn!("hyperliquid trades session ended: {err:#}");
        }
        if cancel.is_cancelled() || tx.is_closed() {
            break;
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn trades_session(
    ws_url: &str,
    coin: &str,
    tx: &mpsc::Sender<f64>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let (ws, _) = connect_async(ws_url).await?;
    let (mut write, mut read) = ws.split();
    let sub = json!({
        "method": "subscribe",
        "subscription": { "type": "trades", "coin": coin }
    });
    write.send(Message::Text(sub.to_string().into())).await?;
    info!("subscribed hyperliquid trades coin={coin}");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            msg = read.next() => {
                let Some(msg) = msg else { break };
                let msg = msg?;
                match msg {
                    Message::Text(text) => {
                        if let Some(px) = last_trade_px(&text, coin) {
                            if tx.send(px).await.is_err() {
                                break;
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        write.send(Message::Pong(payload)).await?;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn last_trade_px(text: &str, coin: &str) -> Option<f64> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("channel").and_then(|v| v.as_str()) != Some("trades") {
        return None;
    }
    let trades = value.get("data")?.as_array()?;
    let mut px = None;
    for trade in trades {
        let trade_coin = trade.get("coin").and_then(|v| v.as_str())?;
        if !trade_coin.eq_ignore_ascii_case(coin) {
            continue;
        }
        px = parse_px(trade.get("px"));
    }
    px
}

async fn poll_mid(http: &reqwest::Client, info_url: &str, coin: &str) -> anyhow::Result<f64> {
    let value: Value = http
        .post(info_url)
        .json(&json!({"type": "allMids"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let mids = value
        .get("mids")
        .and_then(|v| v.as_object())
        .or_else(|| value.as_object())
        .context("allMids payload")?;
    let raw = mids
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(coin))
        .map(|(_, v)| v)
        .context("coin missing from allMids")?;
    parse_px(Some(raw)).context("bad mid")
}

fn parse_px(value: Option<&Value>) -> Option<f64> {
    let px = match value? {
        Value::String(s) => s.parse().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    if px.is_finite() && px > 0.0 {
        Some(px)
    } else {
        None
    }
}
