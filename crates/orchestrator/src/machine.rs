use tempo_agentic_domain::{ExecStep, SignedTx};
use tempo_agentic_strategy::{Order, OrderState};
use thiserror::Error;

/// The single thing an order needs done next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Build and sign the next transaction. Which step that is comes from the
    /// venue, not from here.
    Sign,
    Broadcast {
        signed: SignedTx,
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
        signed: SignedTx,
    },
    Broadcast {
        tx_hash: String,
        /// Unix second the bytes went out, kept so the wait for a receipt can be
        /// given a deadline.
        at: i64,
    },
    Confirmed,
    /// Nothing moved. The order keeps its state and is retried on a later pass.
    StillPending,
    Reverted,
    ExecFailed {
        reason: String,
    },
    /// An operator released a quarantined order by hand. The only outcome no
    /// execution ever produces.
    QuarantineResolved,
    /// The transaction was ready but sending it is not allowed.
    BroadcastBlocked,
    /// No receipt turned up before the deadline.
    ReceiptTimedOut,
}

/// How long a broadcast transaction is given to produce a receipt.
///
/// Generous on purpose. Giving up frees the level to fire again, so a
/// transaction that landed late would become a second fill; the wait has to be
/// long enough that a live transaction has almost certainly been dropped first.
pub const RECEIPT_DEADLINE_SECS: i64 = 1_800;

/// Broadcast attempts an order may burn before it is parked. The count is raised
/// after this decision, so the cap lands on the following attempt.
pub const SWAP_RETRY_CAP: u32 = 8;

/// Longest a retry is ever put off.
pub const SWAP_RETRY_MAX_BACKOFF_SECS: i64 = 600;

const SWAP_RETRY_INITIAL_BACKOFF_SECS: i64 = 2;

/// How long to wait before the given attempt.
///
/// Doubles per attempt up to [`SWAP_RETRY_MAX_BACKOFF_SECS`], so a node that
/// stays down is asked less and less often instead of on every sweep.
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
            signed: SignedTx {
                raw: signed_tx.clone(),
                hash: tx_hash.clone(),
            },
        },
        OrderState::Submitted { tx_hash, .. } => Action::CheckReceipt {
            tx_hash: tx_hash.clone(),
        },
        OrderState::Filled { .. }
        | OrderState::Failed { .. }
        | OrderState::SwapQuarantined { .. } => Action::Done,
        // The lending states exist ahead of the lending work and nothing creates
        // them yet, so there is nothing to do. A sweep passes over such a row
        // without touching the network.
        OrderState::Withdrawing { .. } | OrderState::Depositing { .. } => Action::Done,
    }
}

/// The state an outcome moves an order into, or `None` when it stays put.
///
/// Returns an error when the outcome cannot follow the state at all, which means
/// the row or the caller is wrong rather than the trade.
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
            O::Signed { step, signed },
        ) => S::Broadcasting {
            step,
            amount_in: *amount_in,
            signed_tx: signed.raw,
            tx_hash: signed.hash,
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
        // A refused send says nothing about the transaction: the bytes may sit in
        // a mempool already. Retrying resends exactly the same bytes, which only
        // works because a node reports an already-known transaction as accepted.
        // Staying put is what makes that happen; the caller schedules the retry.
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

        // The swap is always the plan's last step, so confirming it finishes the
        // order. Confirming an allowance step only clears the way for the next
        // one, which the venue names when the signing comes round again.
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
        // The only way out of `Submitted` when a nonce was taken by somebody
        // else: no receipt will ever come. The hash is kept and the reason says
        // outright that the transaction is not provably dead, because ending
        // here frees the level and a late fill would be a second one.
        (S::Submitted { tx_hash, .. }, O::ReceiptTimedOut) => S::Failed {
            tx_hash: Some(tx_hash.clone()),
            reason: format!(
                "no receipt within {} minutes; the transaction may still land",
                RECEIPT_DEADLINE_SECS / 60
            ),
        },

        // Nothing left the process, so there is no hash and nothing to retry:
        // resending would be blocked just the same. Ending here frees the level,
        // which then rests and tries the whole path again a minute later.
        (S::Broadcasting { .. }, O::BroadcastBlocked) => S::Failed {
            tx_hash: None,
            reason: "broadcast blocked; set MAINNET_SWAP=1 to allow".to_string(),
        },

        // The one transition a person makes by hand. `Failed` is the landing
        // because it is the only status that leaves the level free to fire again.
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
        Outcome::BroadcastBlocked => "BroadcastBlocked",
        Outcome::ReceiptTimedOut => "ReceiptTimedOut",
    }
}
