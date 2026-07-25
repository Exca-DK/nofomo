use alloy_primitives::U256;
use serde_json::json;
use tempo_agentic_domain::{ExecStep, ExecutionPlan, VenueName};
use tempo_agentic_strategy::{Level, Order, OrderState, OrderStatus, Side};

fn level() -> Level {
    Level {
        id: "l-1".into(),
        venue: VenueName::Uniswap,
        chain: "base".into(),
        token_in: "USDC".into(),
        token_out: "WETH".into(),
        side: Side::Buy,
        trigger_price_usd: 3_000.0,
        amount: U256::from(1_000_000u64),
        amount_decimals: 6,
        slippage_bps: 50,
    }
}

fn plan() -> ExecutionPlan {
    ExecutionPlan::Uniswap {
        chain_name: "base".into(),
        chain_id: 8453,
        input_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
        input_amount: "1000000".into(),
        quote: json!({"tradeType": "EXACT_INPUT"}),
    }
}

fn with_state(state: OrderState) -> Order {
    Order {
        state,
        ..Order::new("o-1".into(), &level(), plan(), 1)
    }
}

#[test]
fn new_order_snapshots_the_level_and_is_ready_to_sign() {
    let order = Order::new("o-1".into(), &level(), plan(), 42);
    assert_eq!(order.level_id, "l-1");
    assert_eq!(order.venue, VenueName::Uniswap);
    assert_eq!(order.chain, "base");
    assert_eq!(order.token_in, "USDC");
    assert_eq!(order.token_out, "WETH");
    assert_eq!(order.reserved_amount, U256::from(1_000_000u64));
    assert_eq!(order.created_at, 42);
    assert_eq!(order.status(), OrderStatus::Pending);
    assert!(!order.is_terminal());
}

#[test]
fn status_projects_every_state() {
    let amount_in = U256::from(7u64);
    let cases = [
        (
            OrderState::Withdrawing {
                amount_in,
                action_id: "a".into(),
            },
            OrderStatus::Pending,
        ),
        (
            // Each step carries a different value: the status projection must
            // depend only on the phase, never on which transaction it is.
            OrderState::SwapReady {
                step: ExecStep::Cancel,
                amount_in,
                withdraw_action_id: None,
            },
            OrderStatus::Pending,
        ),
        (
            OrderState::Broadcasting {
                step: ExecStep::Approval,
                amount_in,
                signed_tx: "0xdead".into(),
                tx_hash: "0xbeef".into(),
                withdraw_action_id: None,
            },
            OrderStatus::Pending,
        ),
        (
            OrderState::Submitted {
                step: ExecStep::Swap,
                amount_in,
                tx_hash: "0xbeef".into(),
                withdraw_action_id: None,
            },
            OrderStatus::Submitted,
        ),
        (
            OrderState::Depositing {
                tx_hash: "0xbeef".into(),
                amount: amount_in,
                action_id: "a".into(),
            },
            OrderStatus::Submitted,
        ),
        (
            OrderState::Filled {
                tx_hash: "0xbeef".into(),
            },
            OrderStatus::Filled,
        ),
        (
            OrderState::Failed {
                tx_hash: None,
                reason: "boom".into(),
            },
            OrderStatus::Failed,
        ),
        (
            OrderState::SwapQuarantined {
                amount_in,
                withdraw_action_id: "a".into(),
                reason: "boom".into(),
            },
            OrderStatus::Quarantined,
        ),
    ];
    for (state, expected) in cases {
        assert_eq!(with_state(state).status(), expected);
    }
}

#[test]
fn only_settled_states_are_terminal() {
    let amount_in = U256::from(7u64);
    assert!(
        with_state(OrderState::Filled {
            tx_hash: "0xbeef".into()
        })
        .is_terminal()
    );
    assert!(
        with_state(OrderState::Failed {
            tx_hash: None,
            reason: "boom".into()
        })
        .is_terminal()
    );
    assert!(
        with_state(OrderState::SwapQuarantined {
            amount_in,
            withdraw_action_id: "a".into(),
            reason: "boom".into()
        })
        .is_terminal()
    );
    assert!(
        !with_state(OrderState::Submitted {
            step: ExecStep::Swap,
            amount_in,
            tx_hash: "0xbeef".into(),
            withdraw_action_id: None
        })
        .is_terminal()
    );
}

#[test]
fn tx_hash_is_exposed_only_once_something_is_on_chain() {
    let amount_in = U256::from(7u64);
    assert_eq!(
        with_state(OrderState::Filled {
            tx_hash: "0xbeef".into()
        })
        .tx_hash(),
        Some("0xbeef")
    );
    assert_eq!(
        with_state(OrderState::SwapReady {
            step: ExecStep::Swap,
            amount_in,
            withdraw_action_id: None
        })
        .tx_hash(),
        None
    );
    assert_eq!(
        with_state(OrderState::Failed {
            tx_hash: None,
            reason: "boom".into()
        })
        .tx_hash(),
        None
    );
}
