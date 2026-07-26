use tempo_agentic_domain::ExecStep;
use tempo_agentic_strategy::{Order, OrderState};
use thiserror::Error;

/// The single thing an order needs done next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Build and sign the venue's next transaction.
    Sign,
    Broadcast {
        signed_tx: String,
        tx_hash: String,
    },
    CheckReceipt {
        tx_hash: String,
    },
    Done,
}

/// What running an [`Action`] actually produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Signed {
        step: ExecStep,
        signed_tx: String,
        tx_hash: String,
    },
    Broadcast {
        tx_hash: String,
        /// Broadcast time used for the receipt deadline.
        at: i64,
    },
    Confirmed,
    /// Nothing moved. The order keeps its state and is retried on a later pass.
    StillPending,
    Reverted,
    ExecFailed {
        reason: String,
    },
    /// An operator manually released a quarantined order.
    QuarantineResolved,
    /// The transaction was ready but sending it is not allowed.
    BroadcastBlocked {
        /// What a dry run said about it, since nothing was sent.
        note: String,
    },
    /// No receipt turned up before the deadline.
    ReceiptTimedOut,
}

/// Receipt deadline, kept long to avoid a duplicate late fill.
pub const RECEIPT_DEADLINE_SECS: i64 = 1_800;

/// Broadcast attempts allowed before parking an order.
pub const SWAP_RETRY_CAP: u32 = 8;

/// Longest a retry is ever put off.
pub const SWAP_RETRY_MAX_BACKOFF_SECS: i64 = 600;

const SWAP_RETRY_INITIAL_BACKOFF_SECS: i64 = 2;

/// Exponential retry delay capped by [`SWAP_RETRY_MAX_BACKOFF_SECS`].
pub fn swap_retry_backoff_secs(attempts: u32) -> i64 {
    let shift = attempts.saturating_sub(1).min(20);
    SWAP_RETRY_INITIAL_BACKOFF_SECS
        .checked_shl(shift)
        .unwrap_or(SWAP_RETRY_MAX_BACKOFF_SECS)
        .min(SWAP_RETRY_MAX_BACKOFF_SECS)
}

#[derive(Debug, PartialEq, Eq, Error)]
#[error("order {order_id}: outcome {outcome} is invalid in state {state}")]
pub struct TransitionError {
    pub order_id: String,
    pub state: &'static str,
    pub outcome: &'static str,
}

pub fn next_action(order: &Order) -> Action {
    match &order.state {
        OrderState::SwapReady { .. } => Action::Sign,
        OrderState::Broadcasting {
            signed_tx, tx_hash, ..
        } => Action::Broadcast {
            signed_tx: signed_tx.clone(),
            tx_hash: tx_hash.clone(),
        },
        OrderState::Submitted { tx_hash, .. } => Action::CheckReceipt {
            tx_hash: tx_hash.clone(),
        },
        OrderState::Filled { .. }
        | OrderState::Failed { .. }
        | OrderState::SwapQuarantined { .. } => Action::Done,
        // Lending states are not executable yet.
        OrderState::Withdrawing { .. } | OrderState::Depositing { .. } => Action::Done,
    }
}

/// Applies an outcome, rejecting impossible state transitions.
pub fn apply(order: &Order, outcome: Outcome) -> Result<Option<OrderState>, TransitionError> {
    use OrderState as S;
    use Outcome as O;

    let next = match (&order.state, outcome) {
        (_, O::StillPending) => return Ok(None),

        (
            S::SwapReady {
                amount_in,
                withdraw_action_id,
                ..
            },
            O::Signed {
                step,
                signed_tx,
                tx_hash,
            },
        ) => S::Broadcasting {
            step,
            amount_in: *amount_in,
            signed_tx,
            tx_hash,
            withdraw_action_id: withdraw_action_id.clone(),
        },
        (S::SwapReady { .. }, O::ExecFailed { reason }) => S::Failed {
            tx_hash: None,
            reason,
        },

        (
            S::Broadcasting {
                step,
                amount_in,
                withdraw_action_id,
                ..
            },
            O::Broadcast { tx_hash, at },
        ) => S::Submitted {
            step: *step,
            amount_in: *amount_in,
            tx_hash,
            withdraw_action_id: withdraw_action_id.clone(),
            submitted_at: at,
        },
        // Keep signed bytes after an uncertain send so retries are identical.
        (
            S::Broadcasting {
                amount_in, tx_hash, ..
            },
            O::ExecFailed { reason },
        ) => {
            if order.swap_attempts < SWAP_RETRY_CAP {
                return Ok(None);
            }
            S::SwapQuarantined {
                amount_in: *amount_in,
                tx_hash: Some(tx_hash.clone()),
                reason,
            }
        }

        // A confirmed swap finishes; allowances return to step discovery.
        (
            S::Submitted {
                step: ExecStep::Swap,
                tx_hash,
                ..
            },
            O::Confirmed,
        ) => S::Filled {
            tx_hash: tx_hash.clone(),
        },
        (
            S::Submitted {
                amount_in,
                withdraw_action_id,
                ..
            },
            O::Confirmed,
        ) => S::SwapReady {
            step: ExecStep::Swap,
            amount_in: *amount_in,
            withdraw_action_id: withdraw_action_id.clone(),
        },
        (S::Submitted { tx_hash, .. }, O::Reverted) => S::Failed {
            tx_hash: Some(tx_hash.clone()),
            reason: "reverted on-chain".to_string(),
        },
        // Keep the hash because a timed-out transaction may still land.
        (S::Submitted { tx_hash, .. }, O::ReceiptTimedOut) => S::Failed {
            tx_hash: Some(tx_hash.clone()),
            reason: format!(
                "no receipt within {} minutes; the transaction may still land",
                RECEIPT_DEADLINE_SECS / 60
            ),
        },

        // A blocked send leaves nothing to retry and frees the level.
        (S::Broadcasting { .. }, O::BroadcastBlocked { note }) => S::Failed {
            tx_hash: None,
            reason: format!("broadcast blocked; set MAINNET_SWAP=1 to allow ({note})"),
        },

        // Manual release uses `Failed` to free the level.
        (S::SwapQuarantined { tx_hash, .. }, O::QuarantineResolved) => S::Failed {
            tx_hash: tx_hash.clone(),
            reason: "quarantine resolved by operator".to_string(),
        },

        (state, outcome) => {
            return Err(TransitionError {
                order_id: order.id.clone(),
                state: state_name(state),
                outcome: outcome_name(&outcome),
            });
        }
    };
    Ok(Some(next))
}

fn state_name(state: &OrderState) -> &'static str {
    match state {
        OrderState::Withdrawing { .. } => "Withdrawing",
        OrderState::SwapReady { .. } => "SwapReady",
        OrderState::Broadcasting { .. } => "Broadcasting",
        OrderState::Submitted { .. } => "Submitted",
        OrderState::Depositing { .. } => "Depositing",
        OrderState::Filled { .. } => "Filled",
        OrderState::Failed { .. } => "Failed",
        OrderState::SwapQuarantined { .. } => "SwapQuarantined",
    }
}

fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Signed { .. } => "Signed",
        Outcome::Broadcast { .. } => "Broadcast",
        Outcome::Confirmed => "Confirmed",
        Outcome::StillPending => "StillPending",
        Outcome::Reverted => "Reverted",
        Outcome::ExecFailed { .. } => "ExecFailed",
        Outcome::QuarantineResolved => "QuarantineResolved",
        Outcome::BroadcastBlocked { .. } => "BroadcastBlocked",
        Outcome::ReceiptTimedOut => "ReceiptTimedOut",
    }
}
