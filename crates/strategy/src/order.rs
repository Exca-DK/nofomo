use alloy_primitives::U256;
use serde::{Deserialize, Serialize};
use tempo_agentic_domain::{ExecStep, ExecutionPlan, VenueName};

use crate::level::{StrategyLevel, trade_direction};

/// Coarse lifecycle of an [`Order`], derived from its [`OrderState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    /// Created from a fired level; nothing is on chain yet.
    Pending,
    /// On chain, waiting for a receipt or an earn action to confirm.
    Submitted,
    Filled,
    Failed,
    /// The swap exhausted its retries and funds are parked. Terminal.
    Quarantined,
}

impl OrderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Submitted => "submitted",
            Self::Filled => "filled",
            Self::Failed => "failed",
            Self::Quarantined => "quarantined",
        }
    }
}

/// Restart-safe execution progress for an [`Order`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum OrderState {
    /// Withdrawing the spend amount before the swap.
    Withdrawing {
        amount_in: U256,
        action_id: String,
    },
    /// Funds are ready and `step` can be signed.
    SwapReady {
        step: ExecStep,
        amount_in: U256,
        withdraw_action_id: Option<String>,
    },
    /// Signed and persisted before broadcast for safe replay.
    Broadcasting {
        step: ExecStep,
        amount_in: U256,
        signed_tx: String,
        tx_hash: String,
        withdraw_action_id: Option<String>,
    },
    /// Broadcast, waiting for the receipt.
    Submitted {
        step: ExecStep,
        amount_in: U256,
        tx_hash: String,
        withdraw_action_id: Option<String>,
        /// Broadcast time; old rows default to overdue.
        #[serde(default)]
        submitted_at: i64,
    },
    /// Depositing swap proceeds.
    Depositing {
        tx_hash: String,
        amount: U256,
        action_id: String,
    },
    Filled {
        tx_hash: String,
    },
    Failed {
        tx_hash: Option<String>,
        reason: String,
    },
    /// Retries exhausted; blocks the level until an operator resolves it.
    SwapQuarantined {
        amount_in: U256,
        /// Transaction whose broadcast was never confirmed.
        tx_hash: Option<String>,
        reason: String,
    },
}

/// A snapshotted execution attempt for a fired level.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Order {
    pub id: String,
    pub level_id: String,
    pub venue: VenueName,
    pub chain: String,
    pub token_in: String,
    pub token_out: String,
    /// Base units of `token_in` this order committed, snapshotted at creation.
    pub reserved_amount: U256,
    /// Persisted plan used to rebuild transactions after restart.
    pub plan: ExecutionPlan,
    pub state: OrderState,
    /// Swap attempts the order has burned. Inert until the retry transitions land.
    pub swap_attempts: u32,
    /// Earliest Unix second a swap retry may run.
    pub swap_retry_after_ts: Option<i64>,
    pub created_at: i64,
}

impl Order {
    /// Creates an order; the venue may later prepend allowance steps.
    pub fn new(id: String, entry: &StrategyLevel, plan: ExecutionPlan, created_at: i64) -> Self {
        let direction = trade_direction(&entry.strategy, entry.level.side);
        Self {
            id,
            level_id: entry.level.id.clone(),
            venue: entry.strategy.venue,
            chain: entry.strategy.chain.clone(),
            token_in: direction.token_in.to_owned(),
            token_out: direction.token_out.to_owned(),
            reserved_amount: entry.level.amount,
            plan,
            state: OrderState::SwapReady {
                step: ExecStep::Swap,
                amount_in: entry.level.amount,
                withdraw_action_id: None,
            },
            swap_attempts: 0,
            swap_retry_after_ts: None,
            created_at,
        }
    }

    pub fn status(&self) -> OrderStatus {
        match &self.state {
            OrderState::Withdrawing { .. }
            | OrderState::SwapReady { .. }
            | OrderState::Broadcasting { .. } => OrderStatus::Pending,
            OrderState::Submitted { .. } | OrderState::Depositing { .. } => OrderStatus::Submitted,
            OrderState::Filled { .. } => OrderStatus::Filled,
            OrderState::Failed { .. } => OrderStatus::Failed,
            OrderState::SwapQuarantined { .. } => OrderStatus::Quarantined,
        }
    }

    pub fn tx_hash(&self) -> Option<&str> {
        match &self.state {
            OrderState::Broadcasting { tx_hash, .. }
            | OrderState::Submitted { tx_hash, .. }
            | OrderState::Depositing { tx_hash, .. }
            | OrderState::Filled { tx_hash } => Some(tx_hash),
            OrderState::Failed { tx_hash, .. } | OrderState::SwapQuarantined { tx_hash, .. } => {
                tx_hash.as_deref()
            }
            OrderState::Withdrawing { .. } | OrderState::SwapReady { .. } => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            OrderState::Filled { .. }
                | OrderState::Failed { .. }
                | OrderState::SwapQuarantined { .. }
        )
    }
}
