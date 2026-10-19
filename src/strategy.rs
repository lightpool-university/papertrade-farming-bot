use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::U256;
use anyhow::{Context, Result};
use log::{info, warn};
use tokio::time::{Duration, Instant};

use crate::account::{Account, AccountStore, Inflight, Position};
use crate::config::Config;
use crate::price::PriceTick;
use crate::venue::Venue;

pub struct Strategy {
    cfg: Config,
    venue: Box<dyn Venue>,
    accounts: AccountStore,
    session_pending: bool,
    quiet_until: Option<Instant>,
    acc_reward: U256,
    acc_reward_at: Option<Instant>,
}

impl Strategy {
    pub fn new(cfg: Config, venue: Box<dyn Venue>, accounts: AccountStore) -> Self {
        Self {
            cfg,
            venue,
            accounts,
            session_pending: false,
            quiet_until: None,
            acc_reward: U256::ZERO,
            acc_reward_at: None,
        }
    }

    pub async fn on_tick(&mut self, tick: PriceTick) {
        if let Err(err) = self.step(tick).await {
            warn!("hedge step failed: {err:#}");
            self.quiet(Duration::from_secs(2));
        }
    }

    async fn step(&mut self, tick: PriceTick) -> Result<()> {
        // Skip ticks during the quiet window after a submit or a failed step.
        if self.quiet_until.is_some_and(|until| Instant::now() < until) {
            return Ok(());
        }
        // Account stream has not delivered a snapshot yet.
        let Some(account) = self.accounts.read().await.clone() else {
            return Ok(());
        };
        // Register the session or agent once, then wait for the account stream to show it.
        if !self.venue.is_session_authed(&account) {
            if self.session_pending {
                return Ok(());
            }
            info!("registering {}", self.venue.venue_signer_label());
            self.venue.ensure_auth().await?;
            self.session_pending = true;
            self.quiet(Duration::from_secs(5));
            return Ok(());
        }
        self.session_pending = false;

        let positions = &account.positions;
        let opens = self.inflight_opens(&account);
        // A close is already in flight. Wait for it to leave inflight.
        let closing = account.inflight.iter().any(|op| op.action == "closePositions");
        if closing {
            return Ok(());
        }

        // Both legs are open. Close the pair on the target move or near liquidation.
        if positions.len() >= 2 {
            if self.should_close(tick.px, positions) {
                info!("closing hedge at {}", tick.px);
                self.close_positions(positions).await?;
                self.quiet(Duration::from_secs(2));
            }
            return Ok(());
        }

        // One leg is open. Cancel a stuck open older than 3s and close the filled leg.
        if positions.len() == 1 {
            if opens.is_empty() || opens.iter().all(|op| age_ms(op) > 3_000) {
                for op in &opens {
                    info!("cancelling stuck open {}", op.intent_id);
                    self.venue.cancel_inflight_intent(&op.intent_id).await?;
                }
                info!("closing unhedged position {}", positions[0].id);
                self.close_positions(positions).await?;
                self.quiet(Duration::from_secs(2));
            }
            return Ok(());
        }

        // Flat, with opens still in flight. Cancel them after 30s.
        if !opens.is_empty() {
            if opens.iter().all(|op| age_ms(op) > 30_000) {
                for op in &opens {
                    info!("cancelling stale open {}", op.intent_id);
                    self.venue.cancel_inflight_intent(&op.intent_id).await?;
                }
                self.quiet(Duration::from_secs(2));
            }
            return Ok(());
        }

        // Stake or claim is already in flight.
        if account
            .inflight
            .iter()
            .any(|op| op.action == "paperStake" || op.action == "paperClaim")
        {
            return Ok(());
        }
        // Stake when the PAPER balance reaches the minimum.
        if self.stake_if_needed(&account).await? {
            return Ok(());
        }
        // Claim when indexed claimable is above zero, or when the exchange reward formula is positive.
        if self.claim_if_needed(&account).await? {
            return Ok(());
        }
        // Open an equal long and short. Each leg takes half of the available balance.
        let half = account.available / U256::from(2);
        let size: u128 = half.try_into().context("margin does not fit")?;
        if size == 0 {
            return Ok(());
        }
        let margin_usd = size as f64 / account.token_scale as f64;
        info!(
            "opening BTC hedge margin={margin_usd}usd leverage={} px={}",
            self.cfg.leverage, tick.px
        );
        self.venue.open_hedge_position(size, self.cfg.leverage).await?;
        self.quiet(Duration::from_secs(5));
        Ok(())
    }

    fn should_close(&self, px: f64, positions: &[Position]) -> bool {
        positions.iter().any(|position| {
            let entry = position.entry;
            let bust = position.bust;
            if entry <= 0.0 {
                return false;
            }
            let adverse = if position.is_long { entry - px } else { px - entry };
            let adverse_bps = if adverse > 0.0 {
                adverse / entry * 10_000.0
            } else {
                0.0
            };
            let room = if position.is_long { px - bust } else { bust - px };
            let room_bps = room / entry * 10_000.0;
            adverse_bps >= self.cfg.target_move_bps as f64
                || room_bps <= self.cfg.liq_buffer_bps as f64
        })
    }

    async fn close_positions(&mut self, positions: &[Position]) -> Result<()> {
        let ids: Vec<String> = positions.iter().map(|p| p.id.clone()).collect();
        self.venue.close_open_positions(&ids).await
    }

    async fn stake_if_needed(&mut self, account: &Account) -> Result<bool> {
        let min = U256::from(self.cfg.min_stake_paper) * U256::from(account.token_scale);
        if account.paper.balance < min {
            return Ok(false);
        }
        info!("staking PAPER {}", account.paper.balance);
        self.venue.stake_paper_balance(account.paper.balance, account.paper.balance).await?;
        self.quiet(Duration::from_secs(2));
        Ok(true)
    }

    async fn claim_if_needed(&mut self, account: &Account) -> Result<bool> {
        if account.indexed_claim {
            if account.paper.claimable.is_zero() {
                return Ok(false);
            }
            info!("claiming PAPER rewards");
            self.venue.claim_paper_rewards().await?;
            self.quiet(Duration::from_secs(2));
            return Ok(true);
        }
        if self
            .acc_reward_at
            .is_none_or(|at| at.elapsed() > Duration::from_secs(60))
        {
            match self.venue.acc_reward_per_share().await {
                Ok(Some(value)) => {
                    self.acc_reward = value;
                    self.acc_reward_at = Some(Instant::now());
                }
                Ok(None) => {}
                Err(err) => warn!("reward index fetch failed: {err:#}"),
            }
        }
        let accrued = account.paper.staked * self.acc_reward / U256::from(crate::config::REWARD_SCALE);
        let pending = account.paper.pending_base.saturating_add(accrued);
        if pending <= account.paper.reward_debt {
            return Ok(false);
        }
        info!("claiming PAPER rewards");
        self.venue.claim_paper_rewards().await?;
        self.quiet(Duration::from_secs(2));
        Ok(true)
    }

    fn quiet(&mut self, dur: Duration) {
        self.quiet_until = Some(Instant::now() + dur);
    }

    fn inflight_opens<'a>(&self, account: &'a Account) -> Vec<&'a Inflight> {
        let instrument = self.cfg.instrument;
        account
            .inflight
            .iter()
            .filter(|op| op.action == "openPosition" && op.instrument_id == Some(instrument))
            .collect()
    }
}

fn age_ms(op: &Inflight) -> u64 {
    now_millis().saturating_sub(op.accepted_at_ms)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
