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
    },
    Confirmed,
    /// Nothing moved. The order keeps its state and is retried on a later pass.
    StillPending,
    Reverted,
    ExecFailed {
        reason: String,
    },
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
            O::Broadcast { tx_hash },
        ) => S::Submitted {
            step: *step,
            amount_in: *amount_in,
            tx_hash,
            withdraw_action_id: withdraw_action_id.clone(),
        },
        // The bytes may still be in a mempool somewhere, so the hash is worth
        // keeping even though the send reported an error.
        (S::Broadcasting { tx_hash, .. }, O::ExecFailed { reason }) => S::Failed {
            tx_hash: Some(tx_hash.clone()),
            reason,
        },

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
    }
}
