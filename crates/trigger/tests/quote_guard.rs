use std::collections::HashMap;

use alloy_primitives::U256;
use serde_json::json;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, SuiConfig};
use tempo_agentic_domain::{ExecutionPlan, QuoteDraft, VenueName};
use tempo_agentic_price::{PricePair, PriceTick};
use tempo_agentic_strategy::{Level, Side, Strategy, StrategyLevel};
use tempo_agentic_trigger::{TokenResolver, check_quote};

const WETH: &str = "0x4200000000000000000000000000000000000006";
const USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";

fn tokens(pegged: bool) -> TokenResolver {
    TokenResolver::from_config(
        &EvmConfig {
            chains: vec![EvmChain {
                name: "base".into(),
                chain_id: 8453,
                rpc_url: "https://example.invalid".into(),
                graph_subgraph_id: String::new(),
                tokens: HashMap::from([
                    (
                        "WETH".into(),
                        EvmToken {
                            address: WETH.into(),
                            decimals: 18,
                            usd_peg: false,
                        },
                    ),
                    (
                        "USDC".into(),
                        EvmToken {
                            address: USDC.into(),
                            decimals: 6,
                            usd_peg: pegged,
                        },
                    ),
                ]),
            }],
        },
        &SuiConfig::default(),
    )
}

fn entry(side: Side) -> StrategyLevel {
    StrategyLevel {
        strategy: Strategy {
            id: "s-1".into(),
            venue: VenueName::Uniswap,
            chain: "base".into(),
            base_token: "WETH".into(),
            quote_token: "USDC".into(),
        },
        level: Level {
            id: "l-1".into(),
            strategy_id: "s-1".into(),
            side,
            trigger_price_usd: 3_000.0,
            amount: U256::ONE,
            amount_decimals: 18,
            slippage_bps: 50,
        },
    }
}

fn tick(price_usd: f64) -> PriceTick {
    PriceTick {
        pair: PricePair::new(8453, WETH),
        price_usd,
        published_at: 0,
    }
}

fn draft(amount_in: &str, expected_out: &str) -> QuoteDraft {
    QuoteDraft {
        venue: "uniswap".into(),
        chain: "base".into(),
        token_in: "WETH".into(),
        token_out: "USDC".into(),
        amount_in: amount_in.into(),
        expected_amount_out: expected_out.into(),
        minimum_amount_out: expected_out.into(),
        graph_guard: String::new(),
        plan: ExecutionPlan::Uniswap {
            chain_name: "base".into(),
            chain_id: 8453,
            input_token: WETH.into(),
            input_amount: "1".into(),
            quote: json!({}),
        },
    }
}

#[test]
fn a_quote_that_matches_the_observed_price_is_accepted() {
    let checked = check_quote(
        &tokens(true),
        &entry(Side::Sell),
        &draft("1", "2995"),
        &tick(3_000.0),
        500,
    );
    assert!(checked.is_ok(), "{checked:?}");
}

#[test]
fn a_quote_far_below_the_observed_price_is_refused() {
    let error = check_quote(
        &tokens(true),
        &entry(Side::Sell),
        &draft("1", "300"),
        &tick(3_000.0),
        500,
    )
    .expect_err("a quote returning a tenth of the value must be refused");
    let error = error.to_string();
    assert!(error.contains("bps away"), "unclear: {error}");
    assert!(error.contains("3000.00"), "say what was at stake: {error}");
}

#[test]
fn a_quote_far_above_the_observed_price_is_refused() {
    assert!(
        check_quote(
            &tokens(true),
            &entry(Side::Sell),
            &draft("1", "30000"),
            &tick(3_000.0),
            500,
        )
        .is_err()
    );
}

#[test]
fn buying_is_checked_from_the_other_direction() {
    let accepted = check_quote(
        &tokens(true),
        &entry(Side::Buy),
        &draft("3000", "0.999"),
        &tick(3_000.0),
        500,
    );
    assert!(accepted.is_ok(), "{accepted:?}");

    assert!(
        check_quote(
            &tokens(true),
            &entry(Side::Buy),
            &draft("3000", "0.1"),
            &tick(3_000.0),
            500,
        )
        .is_err(),
        "paying 3000 USD for 300 USD of WETH must be refused"
    );
}

#[test]
fn an_unpegged_counter_token_leaves_the_quote_unchecked() {
    let checked = check_quote(
        &tokens(false),
        &entry(Side::Sell),
        &draft("1", "300"),
        &tick(3_000.0),
        500,
    );
    assert!(
        checked.is_ok(),
        "an unpegged pair cannot be judged, so it must not be refused here"
    );
}

#[test]
fn the_configured_limit_decides() {
    assert!(
        check_quote(
            &tokens(true),
            &entry(Side::Sell),
            &draft("1", "2700"),
            &tick(3_000.0),
            500,
        )
        .is_err()
    );
    assert!(
        check_quote(
            &tokens(true),
            &entry(Side::Sell),
            &draft("1", "2700"),
            &tick(3_000.0),
            2_000,
        )
        .is_ok()
    );
}

#[test]
fn an_unreadable_amount_is_refused() {
    assert!(
        check_quote(
            &tokens(true),
            &entry(Side::Sell),
            &draft("1", "lots"),
            &tick(3_000.0),
            500,
        )
        .is_err()
    );
}
