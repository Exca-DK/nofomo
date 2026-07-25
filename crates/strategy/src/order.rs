use alloy_primitives::U256;
use serde::{Deserialize, Serialize};
use tempo_agentic_domain::VenueName;

use crate::level::Level;

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

/// Execution progress of an [`Order`]. Each variant carries exactly what a
/// restarted daemon needs to resume from that point, so a crash never leaves
/// funds somewhere the next run cannot find them.
///
/// The lending variants (`Withdrawing`, `Depositing`) and `SwapQuarantined`
/// exist ahead of the lending work so adding it later needs no migration.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum OrderState {
    /// Pulling the spend amount out of a lending position before the swap can
    /// be signed.
    Withdrawing {
        amount_in: U256,
        action_id: String,
    },
    /// Funds are in hand and the swap can be signed. `withdraw_action_id` is
    /// `None` when no lending withdraw was needed.
    SwapReady {
        amount_in: U256,
        withdraw_action_id: Option<String>,
    },
    /// Signed and persisted before broadcast. A crash here re-broadcasts the
    /// same bytes with the same nonce, so at most one transaction lands.
    Broadcasting {
        amount_in: U256,
        signed_tx: String,
        tx_hash: String,
        withdraw_action_id: Option<String>,
    },
    /// Broadcast, waiting for the receipt.
    Submitted {
        amount_in: U256,
        tx_hash: String,
        withdraw_action_id: Option<String>,
    },
    /// The swap filled and the proceeds are being deposited into a lending
    /// position.
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
    /// The swap exhausted its retries. Funds are parked as `token_in` and the
    /// capital stays reserved until an operator intervenes.
    SwapQuarantined {
        amount_in: U256,
        withdraw_action_id: String,
        reason: String,
    },
}

/// One execution attempt for a [`Level`] that fired.
///
/// The venue, chain, and token pair are snapshotted at creation so editing or
/// deleting the level later cannot change what this order already committed.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Order {
    pub id: String,
    pub level_id: String,
    pub venue: VenueName,
    pub chain: String,
    pub token_in: String,
    pub token_out: String,
    /// Base units of `token_in` this order committed, snapshotted at creation.
    pub reserved_amount: U256,
    pub state: OrderState,
    /// Swap attempts the order has burned. Inert until the retry transitions land.
    pub swap_attempts: u32,
    /// Earliest unix second a swap retry may run. Inert until the retry
    /// transitions land.
    pub swap_retry_after_ts: Option<i64>,
    pub created_at: i64,
}

impl Order {
    /// Creates a fresh order from a fired level, ready for its swap to be signed.
    pub fn new(id: String, level: &Level, created_at: i64) -> Self {
        Self {
            id,
            level_id: level.id.clone(),
            venue: level.venue,
            chain: level.chain.clone(),
            token_in: level.token_in.clone(),
            token_out: level.token_out.clone(),
            reserved_amount: level.amount,
            state: OrderState::SwapReady {
                amount_in: level.amount,
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
            OrderState::Failed { tx_hash, .. } => tx_hash.as_deref(),
            OrderState::Withdrawing { .. }
            | OrderState::SwapReady { .. }
            | OrderState::SwapQuarantined { .. } => None,
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
