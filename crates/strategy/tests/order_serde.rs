use alloy_primitives::U256;
use serde_json::json;
use tempo_agentic_domain::{ExecStep, ExecutionPlan, VenueName};
use tempo_agentic_strategy::{Level, Order, OrderState, Side};

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

fn every_state() -> Vec<OrderState> {
    vec![
        OrderState::Withdrawing {
            amount_in: U256::from(1_000_000u64),
            action_id: "a-1".into(),
        },
        OrderState::SwapReady {
            step: ExecStep::Swap,
            amount_in: U256::from(1_000_000u64),
            withdraw_action_id: Some("a-1".into()),
        },
        OrderState::SwapReady {
            step: ExecStep::Approval,
            amount_in: U256::from(1_000_000u64),
            withdraw_action_id: None,
        },
        OrderState::Broadcasting {
            step: ExecStep::Swap,
            amount_in: U256::from(1_000_000u64),
            signed_tx: "0x02f8b0...".into(),
            tx_hash: "0xdeadbeef".into(),
            withdraw_action_id: None,
        },
        OrderState::Submitted {
            step: ExecStep::Cancel,
            amount_in: U256::from(1_000_000u64),
            tx_hash: "0xdeadbeef".into(),
            withdraw_action_id: None,
            submitted_at: 1_700_000_042,
        },
        OrderState::Depositing {
            tx_hash: "0xdeadbeef".into(),
            amount: U256::from(990_000u64),
            action_id: "a-2".into(),
        },
        OrderState::Filled {
            tx_hash: "0xdeadbeef".into(),
        },
        OrderState::Failed {
            tx_hash: Some("0xdeadbeef".into()),
            reason: "transaction reverted on chain".into(),
        },
        OrderState::Failed {
            tx_hash: None,
            reason: "quote expired before it could be signed".into(),
        },
        OrderState::SwapQuarantined {
            amount_in: U256::from(1_000_000u64),
            tx_hash: Some("0xdeadbeef".into()),
            reason: "exhausted broadcast retries".into(),
        },
    ]
}

#[test]
fn every_order_state_variant_round_trips_through_json() {
    for state in every_state() {
        let order = with_state(state);
        let json = serde_json::to_string(&order).unwrap();
        let decoded: Order = serde_json::from_str(&json).unwrap();
        assert_eq!(order, decoded, "round trip changed: {json}");
    }
}

// Old rows without `submitted_at` load as overdue.
#[test]
fn a_submitted_row_written_before_the_deadline_existed_still_loads() {
    let older = json!({
        "phase": "submitted",
        "step": "swap",
        "amount_in": "0xf4240",
        "tx_hash": "0xdeadbeef",
        "withdraw_action_id": null
    });

    let decoded: OrderState = serde_json::from_value(older).unwrap();

    let OrderState::Submitted { submitted_at, .. } = decoded else {
        panic!("expected a submitted state");
    };
    assert_eq!(submitted_at, 0);
}

#[test]
fn every_order_state_variant_round_trips_through_a_json_value() {
    // Exercise storage's JSON-value round trip.
    for state in every_state() {
        let order = with_state(state);
        let value = serde_json::to_value(&order).unwrap();
        let decoded: Order = serde_json::from_value(value).unwrap();
        assert_eq!(order, decoded);
    }
}
