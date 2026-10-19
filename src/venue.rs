use alloy::primitives::U256;
use anyhow::Result;

use crate::account::Account;

#[async_trait::async_trait]
pub trait Venue: Send {
    fn is_session_authed(&self, account: &Account) -> bool;
    async fn ensure_auth(&mut self) -> Result<()>;
    fn venue_signer_label(&self) -> String;
    async fn open_hedge_position(&mut self, size: u128, leverage: u32) -> Result<()>;
    async fn close_open_positions(&mut self, ids: &[String]) -> Result<()>;
    async fn cancel_inflight_intent(&mut self, intent_id: &str) -> Result<()>;
    async fn stake_paper_balance(&mut self, amount: U256, balance: U256) -> Result<()>;
    async fn claim_paper_rewards(&mut self) -> Result<()>;
    async fn acc_reward_per_share(&mut self) -> Result<Option<U256>> {
        Ok(None)
    }
}
