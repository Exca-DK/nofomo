//! Runtime state the feed, trigger and dashboard all read.

use alloy_primitives::U256;
use serde_json::json;
use tempo_agentic_domain::{ExecStep, ExecutionPlan, VenueName};
use tempo_agentic_price::{PricePair, PriceTick};
use tempo_agentic_strategy::{Order, OrderState, Side, Strategy, StrategyLevel};
use tempo_agentic_trigger::{FeedHealth, RuntimeLevelState, RuntimeStatus};

fn pair() -> PricePair {
    PricePair::new(8453, "0xfeed")
}

fn tick(at: i64) -> PriceTick {
    PriceTick {
        pair: pair(),
        price_usd: 3_000.0,
        published_at: at,
    }
}

#[test]
fn feed_lifecycle_is_time_based_and_recovers() {
    let runtime = RuntimeStatus::new_at(10, false, 5);
    runtime.feed_connecting(pair(), 10);
    assert_eq!(
        runtime.snapshot_at(10).feeds[0].health,
        FeedHealth::Connecting
    );

    runtime.feed_tick(&tick(11), 11);
    assert_eq!(runtime.snapshot_at(15).feeds[0].health, FeedHealth::Live);
    assert_eq!(runtime.snapshot_at(16).feeds[0].health, FeedHealth::Stale);

    runtime.feed_error(&pair(), 17);
    let degraded = runtime.snapshot_at(17).feeds.remove(0);
    assert_eq!(degraded.health, FeedHealth::Degraded);
    assert_eq!(degraded.last_error.unwrap().category, "source_error");

    runtime.feed_tick(&tick(18), 18);
    assert_eq!(runtime.snapshot_at(18).feeds[0].health, FeedHealth::Live);
    runtime.feed_ended(&pair(), 19);
    assert_eq!(runtime.snapshot_at(19).feeds[0].health, FeedHealth::Ended);
}

#[test]
fn runtime_json_never_contains_provider_error_details() {
    let runtime = RuntimeStatus::new_at(10, true, 5);
    runtime.feed_error(&pair(), 11);
    let json = serde_json::to_string(&runtime.snapshot_at(11)).unwrap();
    assert!(!json.contains("http"));
    assert!(!json.contains("body"));
    assert!(json.contains("source_error"));
}

#[test]
fn quiet_until_and_level_priority_use_the_same_clock_boundary() {
    let runtime = RuntimeStatus::new_at(10, false, 5);
    runtime.set_quiet_until("l-1", 20);
    assert!(runtime.is_quiet("l-1", 19));
    assert!(!runtime.is_quiet("l-1", 20));
    assert!(runtime.snapshot_at(19).quiet_until.contains_key("l-1"));
    assert!(!runtime.snapshot_at(20).quiet_until.contains_key("l-1"));

    let failed = order(
        OrderState::Failed {
            tx_hash: None,
            reason: "failed".into(),
        },
        0,
    );
    let filled = order(
        OrderState::Filled {
            tx_hash: "0x1".into(),
        },
        0,
    );
    let executing = order(
        OrderState::Submitted {
            step: ExecStep::Swap,
            amount_in: U256::ONE,
            tx_hash: "0x2".into(),
            withdraw_action_id: None,
            submitted_at: 0,
        },
        0,
    );
    let quarantined = order(
        OrderState::SwapQuarantined {
            amount_in: U256::ONE,
            tx_hash: None,
            reason: "operator needed".into(),
        },
        0,
    );

    assert_eq!(
        runtime.level_state("l-1", std::slice::from_ref(&failed), 19),
        RuntimeLevelState::Cooldown
    );
    assert_eq!(
        runtime.level_state("l-1", &[failed], 60),
        RuntimeLevelState::Armed
    );
    assert_eq!(
        runtime.level_state("l-1", std::slice::from_ref(&filled), 20),
        RuntimeLevelState::Filled
    );
    assert_eq!(
        runtime.level_state("l-1", &[filled, executing.clone()], 20),
        RuntimeLevelState::Executing
    );
    assert_eq!(
        runtime.level_state("l-1", &[executing, quarantined], 20),
        RuntimeLevelState::Quarantined
    );
}

fn order(state: OrderState, created_at: i64) -> Order {
    let entry = StrategyLevel {
        strategy: Strategy {
            id: "s-1".into(),
            venue: VenueName::Uniswap,
            chain: "base".into(),
            base_token: "WETH".into(),
            quote_token: "USDC".into(),
        },
        level: tempo_agentic_strategy::Level {
            id: "l-1".into(),
            strategy_id: "s-1".into(),
            side: Side::Buy,
            trigger_price_usd: 3_000.0,
            amount: U256::ONE,
            amount_decimals: 6,
            slippage_bps: 50,
        },
    };
    let mut order = Order::new(
        "o-1".into(),
        &entry,
        ExecutionPlan::Uniswap {
            chain_name: "base".into(),
            chain_id: 8453,
            input_token: "USDC".into(),
            input_amount: "1".into(),
            quote: json!({}),
        },
        created_at,
    );
    order.state = state;
    order
}
