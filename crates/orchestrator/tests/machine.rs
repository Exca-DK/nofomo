use alloy_primitives::U256;
use tempo_agentic_domain::{ExecStep, ExecutionPlan, SignedTx, VenueName};
use tempo_agentic_strategy::{Level, Order, OrderState, Side};

use tempo_agentic_orchestrator::{Action, Outcome, apply, next_action};

const AMOUNT: u64 = 1_000_000;

fn order(state: OrderState) -> Order {
    let level = Level {
        id: "l-1".into(),
        venue: VenueName::Uniswap,
        chain: "base".into(),
        token_in: "USDC".into(),
        token_out: "WETH".into(),
        side: Side::Buy,
        trigger_price_usd: 3_000.0,
        amount: U256::from(AMOUNT),
        amount_decimals: 6,
        slippage_bps: 50,
    };
    let plan = ExecutionPlan::Uniswap {
        chain_name: "base".into(),
        chain_id: 8453,
        input_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        input_amount: AMOUNT.to_string(),
        quote: serde_json::Value::Null,
    };
    let mut order = Order::new("o-1".into(), &level, plan, 0);
    order.state = state;
    order
}

fn swap_ready(step: ExecStep) -> OrderState {
    OrderState::SwapReady {
        step,
        amount_in: U256::from(AMOUNT),
        withdraw_action_id: None,
    }
}

fn submitted(step: ExecStep) -> OrderState {
    OrderState::Submitted {
        step,
        amount_in: U256::from(AMOUNT),
        tx_hash: "0xabc".into(),
        withdraw_action_id: None,
    }
}

fn signed() -> SignedTx {
    SignedTx {
        raw: "0x02f8".into(),
        hash: "0xdef".into(),
    }
}

#[test]
fn every_state_asks_for_its_own_action() {
    assert_eq!(
        next_action(&order(swap_ready(ExecStep::Swap))),
        Action::Sign
    );
    assert_eq!(
        next_action(&order(OrderState::Broadcasting {
            step: ExecStep::Swap,
            amount_in: U256::from(AMOUNT),
            signed_tx: "0x02f8".into(),
            tx_hash: "0xdef".into(),
            withdraw_action_id: None,
        })),
        Action::Broadcast { signed: signed() }
    );
    assert_eq!(
        next_action(&order(submitted(ExecStep::Swap))),
        Action::CheckReceipt {
            tx_hash: "0xabc".into()
        }
    );
    for state in [
        OrderState::Filled {
            tx_hash: "0xabc".into(),
        },
        OrderState::Failed {
            tx_hash: None,
            reason: "nope".into(),
        },
        OrderState::SwapQuarantined {
            amount_in: U256::from(AMOUNT),
            withdraw_action_id: "w-1".into(),
            reason: "nope".into(),
        },
    ] {
        assert_eq!(next_action(&order(state)), Action::Done);
    }
}

// The step the venue actually signed is what gets recorded, not the hint the
// order was carrying.
#[test]
fn signing_records_the_step_the_venue_chose() {
    let order = order(swap_ready(ExecStep::Swap));
    let next = apply(
        &order,
        Outcome::Signed {
            step: ExecStep::Approval,
            signed: signed(),
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        next,
        OrderState::Broadcasting {
            step: ExecStep::Approval,
            amount_in: U256::from(AMOUNT),
            signed_tx: "0x02f8".into(),
            tx_hash: "0xdef".into(),
            withdraw_action_id: None,
        }
    );
}

#[test]
fn broadcasting_moves_to_submitted_keeping_the_step() {
    let order = order(OrderState::Broadcasting {
        step: ExecStep::Approval,
        amount_in: U256::from(AMOUNT),
        signed_tx: "0x02f8".into(),
        tx_hash: "0xdef".into(),
        withdraw_action_id: None,
    });
    let next = apply(
        &order,
        Outcome::Broadcast {
            tx_hash: "0xdef".into(),
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        next,
        OrderState::Submitted {
            step: ExecStep::Approval,
            amount_in: U256::from(AMOUNT),
            tx_hash: "0xdef".into(),
            withdraw_action_id: None,
        }
    );
}

#[test]
fn a_confirmed_swap_fills_the_order() {
    let order = order(submitted(ExecStep::Swap));
    assert_eq!(
        apply(&order, Outcome::Confirmed).unwrap().unwrap(),
        OrderState::Filled {
            tx_hash: "0xabc".into()
        }
    );
}

// An allowance is not the trade: confirming one only returns the order to
// signing, where the venue names whatever is left.
#[test]
fn a_confirmed_allowance_goes_back_to_signing() {
    for step in [ExecStep::Cancel, ExecStep::Approval] {
        let order = order(submitted(step));
        assert_eq!(
            apply(&order, Outcome::Confirmed).unwrap().unwrap(),
            swap_ready(ExecStep::Swap)
        );
    }
}

#[test]
fn a_revert_fails_the_order_and_keeps_the_hash() {
    let order = order(submitted(ExecStep::Swap));
    assert_eq!(
        apply(&order, Outcome::Reverted).unwrap().unwrap(),
        OrderState::Failed {
            tx_hash: Some("0xabc".into()),
            reason: "reverted on-chain".into(),
        }
    );
}

// Nothing was signed yet, so there is no hash to record; once there is one it
// stays with the failure.
#[test]
fn a_failure_carries_a_hash_only_once_one_exists() {
    let before = order(swap_ready(ExecStep::Swap));
    assert_eq!(
        apply(
            &before,
            Outcome::ExecFailed {
                reason: "build failed".into()
            }
        )
        .unwrap()
        .unwrap(),
        OrderState::Failed {
            tx_hash: None,
            reason: "build failed".into(),
        }
    );

    let after = order(OrderState::Broadcasting {
        step: ExecStep::Swap,
        amount_in: U256::from(AMOUNT),
        signed_tx: "0x02f8".into(),
        tx_hash: "0xdef".into(),
        withdraw_action_id: None,
    });
    assert_eq!(
        apply(
            &after,
            Outcome::ExecFailed {
                reason: "rpc down".into()
            }
        )
        .unwrap()
        .unwrap(),
        OrderState::Failed {
            tx_hash: Some("0xdef".into()),
            reason: "rpc down".into(),
        }
    );
}

#[test]
fn a_pending_outcome_changes_nothing_from_any_state() {
    for state in [
        swap_ready(ExecStep::Swap),
        submitted(ExecStep::Swap),
        OrderState::Filled {
            tx_hash: "0xabc".into(),
        },
    ] {
        assert_eq!(apply(&order(state), Outcome::StillPending).unwrap(), None);
    }
}

#[test]
fn an_outcome_that_cannot_follow_the_state_is_rejected() {
    let error = apply(&order(swap_ready(ExecStep::Swap)), Outcome::Confirmed).unwrap_err();
    assert_eq!(error.state, "SwapReady");
    assert_eq!(error.outcome, "Confirmed");

    let filled = order(OrderState::Filled {
        tx_hash: "0xabc".into(),
    });
    assert!(
        apply(
            &filled,
            Outcome::Broadcast {
                tx_hash: "0xdef".into()
            }
        )
        .is_err()
    );
}
