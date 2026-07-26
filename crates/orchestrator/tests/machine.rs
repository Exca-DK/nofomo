use alloy_primitives::U256;
use tempo_agentic_domain::{ExecStep, ExecutionPlan, VenueName};
use tempo_agentic_strategy::{Level, Order, OrderState, Side, Strategy, StrategyLevel};

use tempo_agentic_orchestrator::{
    Action, Outcome, RECEIPT_DEADLINE_SECS, SWAP_RETRY_CAP, SWAP_RETRY_MAX_BACKOFF_SECS, apply,
    next_action, swap_retry_backoff_secs,
};

const AMOUNT: u64 = 1_000_000;

fn order(state: OrderState) -> Order {
    let level = Level {
        id: "l-1".into(),
        strategy_id: "s-1".into(),
        side: Side::Buy,
        trigger_price_usd: 3_000.0,
        amount: U256::from(AMOUNT),
        amount_decimals: 6,
        slippage_bps: 50,
    };
    let entry = StrategyLevel {
        strategy: Strategy {
            id: "s-1".into(),
            venue: VenueName::Uniswap,
            chain: "base".into(),
            base_token: "WETH".into(),
            quote_token: "USDC".into(),
        },
        level,
    };
    let plan = ExecutionPlan::Uniswap {
        chain_name: "base".into(),
        chain_id: 8453,
        input_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        input_amount: AMOUNT.to_string(),
        quote: serde_json::Value::Null,
    };
    let mut order = Order::new("o-1".into(), &entry, plan, 0);
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
        submitted_at: 0,
    }
}

fn broadcasting() -> OrderState {
    OrderState::Broadcasting {
        step: ExecStep::Swap,
        amount_in: U256::from(AMOUNT),
        signed_tx: "0x02f8".into(),
        tx_hash: "0xdef".into(),
        withdraw_action_id: None,
    }
}

const SIGNED_TX: &str = "0x02f8";
const TX_HASH: &str = "0xdef";

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
        Action::Broadcast {
            signed_tx: SIGNED_TX.into(),
            tx_hash: TX_HASH.into(),
        }
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
            tx_hash: None,
            reason: "nope".into(),
        },
    ] {
        assert_eq!(next_action(&order(state)), Action::Done);
    }
}

// Record the step actually signed.
#[test]
fn signing_records_the_step_the_venue_chose() {
    let order = order(swap_ready(ExecStep::Swap));
    let next = apply(
        &order,
        Outcome::Signed {
            step: ExecStep::Approval,
            signed_tx: SIGNED_TX.into(),
            tx_hash: TX_HASH.into(),
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
            at: 1_700_000_042,
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
            submitted_at: 1_700_000_042,
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

// A confirmed allowance returns to step discovery.
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

// A pre-signing failure frees the level without a hash.
#[test]
fn a_failure_before_signing_ends_the_order_without_a_hash() {
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
}

// Unchanged `Broadcasting` state retries identical bytes.
#[test]
fn a_refused_send_holds_the_state_so_the_same_bytes_go_again() {
    let mut order = order(broadcasting());
    order.swap_attempts = SWAP_RETRY_CAP - 1;
    assert_eq!(
        apply(
            &order,
            Outcome::ExecFailed {
                reason: "rpc down".into()
            }
        )
        .unwrap(),
        None
    );
}

#[test]
fn a_refused_send_past_the_cap_parks_the_order() {
    let mut order = order(broadcasting());
    order.swap_attempts = SWAP_RETRY_CAP;
    assert_eq!(
        apply(
            &order,
            Outcome::ExecFailed {
                reason: "rpc down".into()
            }
        )
        .unwrap()
        .unwrap(),
        OrderState::SwapQuarantined {
            amount_in: U256::from(AMOUNT),
            // Kept so an operator can check whether the bytes landed anyway.
            tx_hash: Some("0xdef".into()),
            reason: "rpc down".into(),
        }
    );
}

// `Failed` is the landing because it is the only status that frees the level.
#[test]
fn resolving_a_quarantine_releases_the_level() {
    let parked = order(OrderState::SwapQuarantined {
        amount_in: U256::from(AMOUNT),
        tx_hash: Some("0xdef".into()),
        reason: "rpc down".into(),
    });
    let released = apply(&parked, Outcome::QuarantineResolved)
        .unwrap()
        .unwrap();
    assert_eq!(
        released,
        OrderState::Failed {
            tx_hash: Some("0xdef".into()),
            reason: "quarantine resolved by operator".into(),
        }
    );
}

#[test]
fn resolving_an_order_that_is_not_parked_is_rejected() {
    let error = apply(
        &order(submitted(ExecStep::Swap)),
        Outcome::QuarantineResolved,
    )
    .unwrap_err();
    assert_eq!(error.state, "Submitted");
    assert_eq!(error.outcome, "QuarantineResolved");
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
                tx_hash: "0xdef".into(),
                at: 1_700_000_042,
            }
        )
        .is_err()
    );
}

#[test]
fn the_backoff_doubles_and_then_holds() {
    assert_eq!(swap_retry_backoff_secs(1), 2);
    assert_eq!(swap_retry_backoff_secs(2), 4);
    assert_eq!(swap_retry_backoff_secs(3), 8);
    assert_eq!(swap_retry_backoff_secs(8), 256);
    assert_eq!(
        swap_retry_backoff_secs(100),
        SWAP_RETRY_MAX_BACKOFF_SECS,
        "a huge attempt count must not overflow the shift"
    );
}

// The first post-failure attempt still gets a delay.
#[test]
fn a_zero_attempt_count_still_pauses() {
    assert_eq!(swap_retry_backoff_secs(0), 2);
}

// A blocked send has no hash to retry and frees the level.
#[test]
fn a_blocked_broadcast_ends_the_order_without_a_hash() {
    assert_eq!(
        apply(&order(broadcasting()), Outcome::BroadcastBlocked)
            .unwrap()
            .unwrap(),
        OrderState::Failed {
            tx_hash: None,
            reason: "broadcast blocked; set MAINNET_SWAP=1 to allow".into(),
        }
    );
}

#[test]
fn blocking_something_that_was_not_being_sent_is_rejected() {
    let error = apply(
        &order(swap_ready(ExecStep::Swap)),
        Outcome::BroadcastBlocked,
    )
    .unwrap_err();
    assert_eq!(error.state, "SwapReady");
    assert_eq!(error.outcome, "BroadcastBlocked");
}

// Receipt timeout frees the level but keeps the uncertain hash.
#[test]
fn a_receipt_that_never_arrives_ends_the_order() {
    let released = apply(&order(submitted(ExecStep::Swap)), Outcome::ReceiptTimedOut)
        .unwrap()
        .unwrap();
    let OrderState::Failed { tx_hash, reason } = &released else {
        panic!("expected a failed order, got {released:?}");
    };
    assert_eq!(tx_hash.as_deref(), Some("0xabc"));
    assert!(
        reason.contains("may still land"),
        "the reason has to warn that this is not proof of death: {reason}"
    );
    assert!(
        reason.contains(&(RECEIPT_DEADLINE_SECS / 60).to_string()),
        "and say how long we waited: {reason}"
    );
}

#[test]
fn a_timeout_makes_no_sense_before_anything_was_sent() {
    let error = apply(&order(swap_ready(ExecStep::Swap)), Outcome::ReceiptTimedOut).unwrap_err();
    assert_eq!(error.state, "SwapReady");
    assert_eq!(error.outcome, "ReceiptTimedOut");
}

// Preserve the broadcast time used by the deadline.
#[test]
fn broadcasting_records_when_the_bytes_went_out() {
    let order = order(broadcasting());
    let next = apply(
        &order,
        Outcome::Broadcast {
            tx_hash: "0xdef".into(),
            at: 1_700_000_042,
        },
    )
    .unwrap()
    .unwrap();
    let OrderState::Submitted { submitted_at, .. } = next else {
        panic!("expected a submitted order");
    };
    assert_eq!(submitted_at, 1_700_000_042);
}
