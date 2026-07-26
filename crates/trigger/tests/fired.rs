use std::collections::HashMap;

use alloy_primitives::U256;
use serde_json::json;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, SuiConfig};
use tempo_agentic_domain::ExecutionPlan;
use tempo_agentic_domain::VenueName;
use tempo_agentic_price::{PricePair, PriceTick};
use tempo_agentic_strategy::{Level, Order, OrderState, Side, Strategy, StrategyLevel};
use tempo_agentic_trigger::{TokenResolver, cooling_down, fired_levels};

const BASE_ID: u64 = 8453;
const ETHEREUM_ID: u64 = 1;
// Deliberately checksummed: the feed and the config may disagree on casing.
const WETH_BASE: &str = "0x4200000000000000000000000000000000000006";
const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const WETH_ETHEREUM: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";

fn token(address: &str, decimals: u8) -> EvmToken {
    EvmToken {
        address: address.to_string(),
        decimals,
    }
}

fn chain(name: &str, chain_id: u64, tokens: &[(&str, &str, u8)]) -> EvmChain {
    EvmChain {
        name: name.to_string(),
        chain_id,
        rpc_url: "https://example.invalid".to_string(),
        graph_subgraph_id: "subgraph".to_string(),
        tokens: tokens
            .iter()
            .map(|(symbol, address, decimals)| (symbol.to_string(), token(address, *decimals)))
            .collect::<HashMap<_, _>>(),
    }
}

fn resolver() -> TokenResolver {
    TokenResolver::from_config(
        &EvmConfig {
            chains: vec![
                chain(
                    "base",
                    BASE_ID,
                    &[("WETH", WETH_BASE, 18), ("USDC", USDC_BASE, 6)],
                ),
                chain("ethereum", ETHEREUM_ID, &[("WETH", WETH_ETHEREUM, 18)]),
            ],
        },
        &SuiConfig::default(),
    )
}

fn level(id: &str, side: Side) -> StrategyLevel {
    StrategyLevel {
        strategy: Strategy {
            id: "s-1".into(),
            venue: VenueName::Uniswap,
            chain: "base".into(),
            base_token: "WETH".into(),
            quote_token: "USDC".into(),
        },
        level: Level {
            id: id.to_string(),
            strategy_id: "s-1".into(),
            side,
            trigger_price_usd: 3_000.0,
            amount: U256::from(1_000_000u64),
            amount_decimals: if side == Side::Buy { 6 } else { 18 },
            slippage_bps: 50,
        },
    }
}

fn tick(chain_id: u64, token_address: &str, price_usd: f64) -> PriceTick {
    PriceTick {
        pair: PricePair::new(chain_id, token_address),
        price_usd,
        published_at: 1_800_000_000,
    }
}

fn plan() -> ExecutionPlan {
    ExecutionPlan::Uniswap {
        chain_name: "base".into(),
        chain_id: BASE_ID,
        input_token: USDC_BASE.into(),
        input_amount: "1000000".into(),
        quote: json!({}),
    }
}

/// Creates an order for `level_id` in `state`.
fn order(level_id: &str, state: OrderState) -> Order {
    let mut order = Order::new(
        format!("o-{level_id}"),
        &level(level_id, Side::Buy),
        plan(),
        1,
    );
    order.state = state;
    order
}

fn fired_ids(levels: &[StrategyLevel], tick: &PriceTick) -> Vec<String> {
    fired_with_orders(levels, &[], tick)
}

fn fired_with_orders(levels: &[StrategyLevel], orders: &[Order], tick: &PriceTick) -> Vec<String> {
    fired_levels(levels, orders, tick, &resolver())
        .into_iter()
        .map(|entry| entry.level.id.clone())
        .collect()
}

#[test]
fn a_buy_fires_at_the_threshold_and_below() {
    let levels = vec![level("l-1", Side::Buy)];
    assert_eq!(
        fired_ids(&levels, &tick(BASE_ID, WETH_BASE, 3_000.0)),
        ["l-1"]
    );
    assert_eq!(
        fired_ids(&levels, &tick(BASE_ID, WETH_BASE, 2_999.0)),
        ["l-1"]
    );
    assert!(fired_ids(&levels, &tick(BASE_ID, WETH_BASE, 3_001.0)).is_empty());
}

#[test]
fn a_sell_fires_at_the_threshold_and_above() {
    // A sell disposes of the base token, so it is priced on `token_in`.
    let selling = level("l-1", Side::Sell);
    let levels = vec![selling];

    assert_eq!(
        fired_ids(&levels, &tick(BASE_ID, WETH_BASE, 3_000.0)),
        ["l-1"]
    );
    assert_eq!(
        fired_ids(&levels, &tick(BASE_ID, WETH_BASE, 3_001.0)),
        ["l-1"]
    );
    assert!(fired_ids(&levels, &tick(BASE_ID, WETH_BASE, 2_999.0)).is_empty());
}

// Buy and sell both observe the strategy's base token.
#[test]
fn both_sides_are_priced_on_the_base_token() {
    let buying = level("buy", Side::Buy);
    let mut selling = level("sell", Side::Sell);
    selling.level.trigger_price_usd = 3_000.0;
    let levels = vec![buying, selling];

    assert_eq!(
        fired_ids(&levels, &tick(BASE_ID, WETH_BASE, 2_999.0)),
        ["buy"]
    );
    assert!(fired_ids(&levels, &tick(BASE_ID, USDC_BASE, 3_001.0)).is_empty());
    assert_eq!(
        fired_ids(&levels, &tick(BASE_ID, WETH_BASE, 3_001.0)),
        ["sell"]
    );
}

#[test]
fn a_tick_from_another_chain_is_ignored() {
    let levels = vec![level("l-1", Side::Buy)];
    assert!(
        fired_ids(&levels, &tick(ETHEREUM_ID, WETH_ETHEREUM, 2_999.0)).is_empty(),
        "a base level must not fire on an ethereum tick"
    );
}

#[test]
fn a_tick_for_another_token_is_ignored() {
    let levels = vec![level("l-1", Side::Buy)];
    assert!(fired_ids(&levels, &tick(BASE_ID, USDC_BASE, 2_999.0)).is_empty());
}

#[test]
fn address_casing_does_not_decide_the_match() {
    let levels = vec![level("l-1", Side::Buy)];
    let lowercase = WETH_BASE.to_ascii_lowercase();
    assert_eq!(
        fired_ids(&levels, &tick(BASE_ID, &lowercase, 2_999.0)),
        ["l-1"]
    );
}

// A spent level must not fire again.
#[test]
fn a_level_with_an_order_in_flight_or_filled_does_not_fire() {
    let levels = vec![level("l-1", Side::Buy)];
    let qualifying = tick(BASE_ID, WETH_BASE, 2_999.0);

    let in_flight = OrderState::Submitted {
        step: tempo_agentic_domain::ExecStep::Swap,
        amount_in: U256::from(1_000_000u64),
        tx_hash: "0xhash".into(),
        withdraw_action_id: None,
        submitted_at: 0,
    };
    let filled = OrderState::Filled {
        tx_hash: "0xhash".into(),
    };

    for state in [in_flight, filled] {
        assert!(
            fired_with_orders(&levels, &[order("l-1", state)], &qualifying).is_empty(),
            "a level with a live or filled order must not fire again"
        );
    }
}

// A failed attempt committed nothing, so the level is still available.
#[test]
fn a_failed_order_leaves_the_level_free() {
    let levels = vec![level("l-1", Side::Buy)];
    let failed = order(
        "l-1",
        OrderState::Failed {
            tx_hash: None,
            reason: "reverted".into(),
        },
    );
    assert_eq!(
        fired_with_orders(&levels, &[failed], &tick(BASE_ID, WETH_BASE, 2_999.0)),
        ["l-1"]
    );
}

// Orders are matched by level, so one level's history must not silence another.
#[test]
fn an_order_for_another_level_does_not_block() {
    let levels = vec![level("l-1", Side::Buy)];
    let other = order(
        "l-2",
        OrderState::Filled {
            tx_hash: "0xhash".into(),
        },
    );
    assert_eq!(
        fired_with_orders(&levels, &[other], &tick(BASE_ID, WETH_BASE, 2_999.0)),
        ["l-1"]
    );
}

// Skip unresolved tokens instead of matching loosely.
#[test]
fn a_level_the_configuration_cannot_resolve_never_fires() {
    let mut unknown_token = level("bad-token", Side::Buy);
    unknown_token.strategy.base_token = "NOPE".to_string();
    let mut unknown_chain = level("bad-chain", Side::Buy);
    unknown_chain.strategy.chain = "polygon".to_string();

    assert!(fired_ids(&[unknown_token], &tick(BASE_ID, WETH_BASE, 2_999.0)).is_empty());
    assert!(fired_ids(&[unknown_chain], &tick(BASE_ID, WETH_BASE, 2_999.0)).is_empty());
}

#[test]
fn one_tick_can_fire_several_levels() {
    let mut cheaper = level("l-2", Side::Buy);
    cheaper.level.trigger_price_usd = 2_500.0;
    let mut cheapest = level("l-3", Side::Buy);
    cheapest.level.trigger_price_usd = 1_000.0;
    let levels = vec![level("l-1", Side::Buy), cheaper, cheapest];

    assert_eq!(
        fired_ids(&levels, &tick(BASE_ID, WETH_BASE, 2_400.0)),
        ["l-1", "l-2"]
    );
}

// Failed orders re-arm only after the cooldown.
#[test]
fn a_level_that_just_acted_has_to_rest() {
    let just_failed = [order(
        "l-1",
        OrderState::Failed {
            tx_hash: None,
            reason: "reverted".into(),
        },
    )];

    assert!(cooling_down("l-1", &just_failed, 30));
    assert!(
        !cooling_down("l-1", &just_failed, 61),
        "the rest has to end"
    );
    assert!(
        !cooling_down("l-2", &just_failed, 30),
        "one level's history must not silence another"
    );
    assert!(
        !cooling_down("l-1", &[], 30),
        "a level that never acted is free to act"
    );
}

// Non-failed orders are already blocked by `is_spent`.
#[test]
fn non_failed_orders_do_not_add_a_second_cooldown() {
    let filled = [order(
        "l-1",
        OrderState::Filled {
            tx_hash: "0xabc".into(),
        },
    )];
    assert!(!cooling_down("l-1", &filled, 30));
}
