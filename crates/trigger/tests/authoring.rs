use std::collections::HashMap;

use alloy_primitives::U256;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, PriceRef, SuiCoin, SuiConfig};
use tempo_agentic_domain::VenueName;
use tempo_agentic_price::{PricePair, PriceSource, PriceStream};
use tempo_agentic_strategy::Side;
use tempo_agentic_trigger::{LevelDraft, TokenResolver, validate_level};

struct Prices {
    chains: Vec<u64>,
}

impl PriceSource for Prices {
    fn supports(&self, pair: &PricePair) -> bool {
        self.chains.contains(&pair.chain_id)
    }

    fn stream(&self, _pair: &PricePair) -> PriceStream {
        Box::pin(futures::stream::empty())
    }
}

fn prices() -> Prices {
    Prices { chains: vec![8453] }
}

const MAX_SLIPPAGE_BPS: u16 = 500;

fn evm() -> EvmConfig {
    EvmConfig {
        chains: vec![EvmChain {
            name: "base".into(),
            chain_id: 8453,
            rpc_url: "https://example.invalid".into(),
            graph_subgraph_id: "subgraph".into(),
            tokens: HashMap::from([
                (
                    "WETH".to_string(),
                    EvmToken {
                        address: "0x4200000000000000000000000000000000000006".into(),
                        decimals: 18,
                    },
                ),
                (
                    "USDC".to_string(),
                    EvmToken {
                        address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                        decimals: 6,
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
                },
            ),
            (
                "SUI".to_string(),
                SuiCoin {
                    coin_type: "0x2::sui::SUI".into(),
                    decimals: 9,
                    price_ref: None,
                },
            ),
        ]),
        ..SuiConfig::default()
    }
}

fn tokens() -> TokenResolver {
    TokenResolver::from_config(&evm(), &sui())
}

fn draft() -> LevelDraft {
    LevelDraft {
        id: "l-1".into(),
        venue: "uniswap".into(),
        chain: "base".into(),
        token_in: "USDC".into(),
        token_out: "WETH".into(),
        side: "buy".into(),
        trigger_price_usd: 3_000.0,
        amount: "25".into(),
        slippage_bps: 50,
    }
}

fn accept(draft: LevelDraft) -> tempo_agentic_strategy::Level {
    validate_level(&tokens(), MAX_SLIPPAGE_BPS, &prices(), &draft).unwrap()
}

fn reject(draft: LevelDraft) -> String {
    validate_level(&tokens(), MAX_SLIPPAGE_BPS, &prices(), &draft)
        .expect_err("this draft must not be storable")
        .to_string()
}

#[test]
fn a_sound_draft_becomes_a_rule() {
    let level = accept(draft());

    assert_eq!(level.id, "l-1");
    assert_eq!(level.venue, VenueName::Uniswap);
    assert_eq!(level.side, Side::Buy);
    assert_eq!(level.trigger_price_usd, 3_000.0);
    assert_eq!(level.slippage_bps, 50);
}

#[test]
fn the_amount_is_scaled_by_the_input_token() {
    let level = accept(draft());
    assert_eq!(level.amount, U256::from(25_000_000u64));
    assert_eq!(level.amount_decimals, 6);

    let selling = LevelDraft {
        token_in: "WETH".into(),
        token_out: "USDC".into(),
        side: "sell".into(),
        amount: "0.5".into(),
        ..draft()
    };
    let level = accept(selling);
    assert_eq!(level.amount, U256::from(500_000_000_000_000_000u64));
    assert_eq!(level.amount_decimals, 18);
}

#[test]
fn the_chain_is_stored_as_the_configuration_spells_it() {
    let level = accept(LevelDraft {
        chain: "BASE".into(),
        token_in: "usdc".into(),
        token_out: "weth".into(),
        ..draft()
    });

    assert_eq!(level.chain, "base");
    assert_eq!(level.token_in, "USDC");
    assert_eq!(level.token_out, "WETH");
}

#[test]
fn a_rule_nothing_can_price_is_refused() {
    assert!(
        reject(LevelDraft {
            chain: "solana".into(),
            ..draft()
        })
        .contains("solana")
    );
    assert!(
        reject(LevelDraft {
            token_in: "DOGE".into(),
            ..draft()
        })
        .contains("DOGE")
    );
    assert!(
        reject(LevelDraft {
            token_out: "DOGE".into(),
            ..draft()
        })
        .contains("DOGE")
    );
}

#[test]
fn slippage_above_the_ceiling_is_refused() {
    let error = reject(LevelDraft {
        slippage_bps: MAX_SLIPPAGE_BPS + 1,
        ..draft()
    });
    assert!(error.contains("500"), "say what the ceiling is: {error}");
}

#[test]
fn a_pair_that_does_not_trade_is_refused() {
    assert!(
        reject(LevelDraft {
            token_out: "USDC".into(),
            ..draft()
        })
        .contains("must differ")
    );
}

#[test]
fn an_unreadable_side_venue_or_amount_is_refused() {
    assert!(
        reject(LevelDraft {
            side: "hodl".into(),
            ..draft()
        })
        .contains("hodl")
    );
    assert!(
        reject(LevelDraft {
            venue: "sushiswap".into(),
            ..draft()
        })
        .contains("sushiswap")
    );
    assert!(
        !reject(LevelDraft {
            amount: "lots".into(),
            ..draft()
        })
        .is_empty()
    );
}

#[test]
fn a_chain_no_source_quotes_is_refused() {
    let quoting_nothing = Prices { chains: Vec::new() };
    let error = validate_level(&tokens(), MAX_SLIPPAGE_BPS, &quoting_nothing, &draft())
        .expect_err("a rule nothing can price must not be storable")
        .to_string();
    assert!(error.contains("could never fire"), "unclear: {error}");
    assert!(error.contains("WETH"), "say which token: {error}");
}

#[test]
fn the_side_decides_which_token_has_to_be_quotable() {
    assert!(validate_level(&tokens(), MAX_SLIPPAGE_BPS, &prices(), &draft()).is_ok());
    assert!(
        validate_level(
            &tokens(),
            MAX_SLIPPAGE_BPS,
            &prices(),
            &LevelDraft {
                token_in: "WETH".into(),
                token_out: "USDC".into(),
                side: "sell".into(),
                amount: "0.5".into(),
                ..draft()
            }
        )
        .is_ok()
    );
}

fn sui_draft() -> LevelDraft {
    LevelDraft {
        id: "l-sui".into(),
        venue: "cetus".into(),
        chain: "sui".into(),
        token_in: "hBTC".into(),
        token_out: "SUI".into(),
        side: "sell".into(),
        trigger_price_usd: 50_000.0,
        amount: "0.001".into(),
        slippage_bps: 100,
    }
}

#[test]
fn a_sui_rule_stores_the_coin_type_with_its_case_intact() {
    let level = accept(sui_draft());

    assert_eq!(level.chain, "sui");
    assert_eq!(level.token_in, "0xfce::btc::BTC");
    assert_eq!(level.token_out, "0x2::sui::SUI");
    assert_eq!(level.venue, VenueName::Cetus);
    assert_eq!(level.amount_decimals, 8);
}

#[test]
fn an_evm_rule_still_stores_the_symbol() {
    let level = accept(draft());
    assert_eq!(level.token_in, "USDC");
    assert_eq!(level.token_out, "WETH");
}

#[test]
fn a_venue_that_does_not_trade_the_chains_family_is_refused() {
    let error = reject(LevelDraft {
        venue: "uniswap".into(),
        ..sui_draft()
    });
    assert!(error.contains("does not trade"), "unclear: {error}");
}

#[test]
fn a_sui_rule_is_priced_through_its_reference() {
    assert!(validate_level(&tokens(), MAX_SLIPPAGE_BPS, &prices(), &sui_draft()).is_ok());
}

#[test]
fn a_coin_without_a_price_reference_cannot_be_the_priced_side() {
    let error = reject(LevelDraft {
        token_in: "SUI".into(),
        token_out: "hBTC".into(),
        side: "sell".into(),
        amount: "1".into(),
        ..sui_draft()
    });
    assert!(error.contains("priced on"), "unclear: {error}");
}
