use std::collections::HashMap;

use alloy_primitives::U256;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, PriceRef, SuiCoin, SuiConfig};
use tempo_agentic_price::{PricePair, PriceSource, PriceStream};
use tempo_agentic_strategy::{Side, StrategyLevel, trade_direction};
use tempo_agentic_trigger::{
    LevelDraft, StrategyDraft, TokenResolver, validate_level, validate_stored_level,
    validate_strategy,
};

struct Prices(bool);

impl PriceSource for Prices {
    fn supports(&self, pair: &PricePair) -> bool {
        self.0 && pair.chain_id == 8453
    }

    fn stream(&self, _pair: &PricePair) -> PriceStream {
        Box::pin(futures::stream::empty())
    }
}

fn evm(usdc_decimals: u8) -> EvmConfig {
    EvmConfig {
        chains: vec![EvmChain {
            name: "base".into(),
            chain_id: 8453,
            rpc_url: "https://example.invalid".into(),
            graph_subgraph_id: "subgraph".into(),
            tokens: HashMap::from([
                (
                    "WETH".into(),
                    EvmToken {
                        address: "0x4200000000000000000000000000000000000006".into(),
                        decimals: 18,
                        usd_peg: false,
                    },
                ),
                (
                    "USDC".into(),
                    EvmToken {
                        address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                        decimals: usdc_decimals,
                        usd_peg: false,
                    },
                ),
            ]),
        }],
    }
}

fn sui() -> SuiConfig {
    SuiConfig {
        enabled: true,
        rpc_url: "https://example.invalid".into(),
        coins: HashMap::from([
            (
                "hBTC".to_string(),
                SuiCoin {
                    coin_type: "0xfce::btc::BTC".into(),
                    decimals: 8,
                    price_ref: Some(PriceRef {
                        chain_id: 8453,
                        address: "0x1111111111111111111111111111111111111111".into(),
                    }),
                    usd_peg: false,
                },
            ),
            (
                "SUI".to_string(),
                SuiCoin {
                    coin_type: "0x2::sui::SUI".into(),
                    decimals: 9,
                    price_ref: None,
                    usd_peg: false,
                },
            ),
        ]),
        ..SuiConfig::default()
    }
}

fn tokens(usdc_decimals: u8) -> TokenResolver {
    TokenResolver::from_config(&evm(usdc_decimals), &sui())
}

fn sui_strategy_draft() -> StrategyDraft {
    StrategyDraft {
        id: "s-sui".into(),
        venue: "cetus".into(),
        chain: "sui".into(),
        base_token: "hBTC".into(),
        quote_token: "SUI".into(),
    }
}

fn strategy_draft() -> StrategyDraft {
    StrategyDraft {
        id: "s-1".into(),
        venue: "uniswap".into(),
        chain: "BASE".into(),
        base_token: "weth".into(),
        quote_token: "usdc".into(),
    }
}

fn level_draft(side: &str, amount: &str) -> LevelDraft {
    LevelDraft {
        id: format!("l-{side}"),
        strategy_id: "s-1".into(),
        side: side.into(),
        trigger_price_usd: 3_000.0,
        amount: amount.into(),
        slippage_bps: 50,
    }
}

#[test]
fn strategy_is_canonical_and_must_be_priceable() {
    let strategy = validate_strategy(&tokens(6), &Prices(true), &strategy_draft()).unwrap();
    assert_eq!(strategy.chain, "base");
    assert_eq!(strategy.base_token, "WETH");
    assert_eq!(strategy.quote_token, "USDC");

    let error = validate_strategy(&tokens(6), &Prices(false), &strategy_draft())
        .unwrap_err()
        .to_string();
    assert!(error.contains("could never fire"));
}

#[test]
fn buy_spends_quote_and_sell_spends_base() {
    let registry = tokens(6);
    let strategy = validate_strategy(&registry, &Prices(true), &strategy_draft()).unwrap();
    let buy = validate_level(
        &registry,
        500,
        &Prices(true),
        &strategy,
        &level_draft("buy", "25"),
    )
    .unwrap();
    let sell = validate_level(
        &registry,
        500,
        &Prices(true),
        &strategy,
        &level_draft("sell", "0.5"),
    )
    .unwrap();

    assert_eq!(buy.side, Side::Buy);
    assert_eq!(buy.amount, U256::from(25_000_000u64));
    assert_eq!(buy.amount_decimals, 6);
    assert_eq!(trade_direction(&strategy, buy.side).token_in, "USDC");
    assert_eq!(sell.amount, U256::from(500_000_000_000_000_000u64));
    assert_eq!(sell.amount_decimals, 18);
    assert_eq!(trade_direction(&strategy, sell.side).token_in, "WETH");
}

#[test]
fn bad_level_input_is_rejected() {
    let registry = tokens(6);
    let strategy = validate_strategy(&registry, &Prices(true), &strategy_draft()).unwrap();

    for draft in [
        LevelDraft {
            strategy_id: "other".into(),
            ..level_draft("buy", "1")
        },
        LevelDraft {
            side: "hodl".into(),
            ..level_draft("buy", "1")
        },
        LevelDraft {
            amount: "lots".into(),
            ..level_draft("buy", "1")
        },
        LevelDraft {
            slippage_bps: 501,
            ..level_draft("buy", "1")
        },
    ] {
        assert!(
            validate_level(&registry, 500, &Prices(true), &strategy, &draft).is_err(),
            "invalid draft was accepted: {draft:?}"
        );
    }
}

#[test]
fn startup_check_detects_decimals_config_drift() {
    let registry = tokens(6);
    let strategy = validate_strategy(&registry, &Prices(true), &strategy_draft()).unwrap();
    let level = validate_level(
        &registry,
        500,
        &Prices(true),
        &strategy,
        &level_draft("buy", "1"),
    )
    .unwrap();
    let entry = StrategyLevel { strategy, level };

    let error = validate_stored_level(&tokens(18), 500, &Prices(true), &entry)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("config has 18"),
        "unclear drift error: {error}"
    );
}

// A Sui strategy has to reach the venue as a Move type. Upper-casing the symbol,
// which is what EVM strategies get, would produce a type nothing resolves.
#[test]
fn a_sui_strategy_stores_coin_types_with_their_case_intact() {
    let strategy = validate_strategy(&tokens(6), &Prices(true), &sui_strategy_draft()).unwrap();

    assert_eq!(strategy.chain, "sui");
    assert_eq!(strategy.base_token, "0xfce::btc::BTC");
    assert_eq!(strategy.quote_token, "0x2::sui::SUI");
}

// hBTC is priced off its mainnet reference, so a Sui strategy is quotable even
// though no feed indexes the testnet coin itself.
#[test]
fn a_sui_strategy_is_priced_through_its_reference() {
    let registry = tokens(6);
    let strategy = validate_strategy(&registry, &Prices(true), &sui_strategy_draft()).unwrap();
    let level = validate_level(
        &registry,
        500,
        &Prices(true),
        &strategy,
        &LevelDraft {
            strategy_id: "s-sui".into(),
            ..level_draft("sell", "0.001")
        },
    )
    .unwrap();

    // Selling spends the base token, whose decimals the level snapshots.
    assert_eq!(level.amount_decimals, 8);
    assert_eq!(level.amount, U256::from(100_000u64));
}

// A venue trading another family only fails at quote time, long after the
// strategy was accepted and the user was told it was stored.
#[test]
fn a_venue_that_does_not_trade_the_chains_family_is_refused() {
    let error = validate_strategy(
        &tokens(6),
        &Prices(true),
        &StrategyDraft {
            venue: "uniswap".into(),
            ..sui_strategy_draft()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("does not trade"), "unclear: {error}");
}

// SUI carries no price reference, so it can be the quote leg but never the base.
#[test]
fn a_coin_without_a_price_reference_cannot_be_the_base() {
    let error = validate_strategy(
        &tokens(6),
        &Prices(true),
        &StrategyDraft {
            base_token: "SUI".into(),
            quote_token: "hBTC".into(),
            ..sui_strategy_draft()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("base token"), "unclear: {error}");
}
