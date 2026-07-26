use std::collections::HashMap;

use alloy_primitives::U256;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken};
use tempo_agentic_price::{PricePair, PriceSource, PriceStream};
use tempo_agentic_strategy::{Side, StrategyLevel, trade_direction};
use tempo_agentic_trigger::{
    LevelDraft, StrategyDraft, validate_level, validate_stored_level, validate_strategy,
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
        keystore_path: "/dev/null".into(),
        password_file: "/dev/null".into(),
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
                    },
                ),
                (
                    "USDC".into(),
                    EvmToken {
                        address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                        decimals: usdc_decimals,
                    },
                ),
            ]),
        }],
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
    let strategy = validate_strategy(&evm(6), &Prices(true), &strategy_draft()).unwrap();
    assert_eq!(strategy.chain, "base");
    assert_eq!(strategy.base_token, "WETH");
    assert_eq!(strategy.quote_token, "USDC");

    let error = validate_strategy(&evm(6), &Prices(false), &strategy_draft())
        .unwrap_err()
        .to_string();
    assert!(error.contains("could never fire"));
}

#[test]
fn buy_spends_quote_and_sell_spends_base() {
    let config = evm(6);
    let strategy = validate_strategy(&config, &Prices(true), &strategy_draft()).unwrap();
    let buy = validate_level(
        &config,
        500,
        &Prices(true),
        &strategy,
        &level_draft("buy", "25"),
    )
    .unwrap();
    let sell = validate_level(
        &config,
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
    let config = evm(6);
    let strategy = validate_strategy(&config, &Prices(true), &strategy_draft()).unwrap();

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
            validate_level(&config, 500, &Prices(true), &strategy, &draft).is_err(),
            "invalid draft was accepted: {draft:?}"
        );
    }
}

#[test]
fn startup_check_detects_decimals_config_drift() {
    let config_a = evm(6);
    let strategy = validate_strategy(&config_a, &Prices(true), &strategy_draft()).unwrap();
    let level = validate_level(
        &config_a,
        500,
        &Prices(true),
        &strategy,
        &level_draft("buy", "1"),
    )
    .unwrap();
    let entry = StrategyLevel { strategy, level };

    let error = validate_stored_level(&evm(18), 500, &Prices(true), &entry)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("config has 18"),
        "unclear drift error: {error}"
    );
}
