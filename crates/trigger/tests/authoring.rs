use std::collections::HashMap;

use alloy_primitives::U256;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken};
use tempo_agentic_domain::VenueName;
use tempo_agentic_strategy::Side;
use tempo_agentic_trigger::{LevelDraft, validate_level};

const MAX_SLIPPAGE_BPS: u16 = 500;

fn evm() -> EvmConfig {
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

fn reject(draft: LevelDraft) -> String {
    validate_level(&evm(), MAX_SLIPPAGE_BPS, &draft)
        .expect_err("this draft must not be storable")
        .to_string()
}

#[test]
fn a_sound_draft_becomes_a_rule() {
    let level = validate_level(&evm(), MAX_SLIPPAGE_BPS, &draft()).unwrap();

    assert_eq!(level.id, "l-1");
    assert_eq!(level.venue, VenueName::Uniswap);
    assert_eq!(level.side, Side::Buy);
    assert_eq!(level.trigger_price_usd, 3_000.0);
    assert_eq!(level.slippage_bps, 50);
}

// The amount is written the way a person says it and stored the way a chain
// wants it. Getting the scale wrong here would spend a millionth of the intended
// sum, or a million times it.
#[test]
fn the_amount_is_scaled_by_the_input_token() {
    let level = validate_level(&evm(), MAX_SLIPPAGE_BPS, &draft()).unwrap();
    assert_eq!(level.amount, U256::from(25_000_000u64));
    assert_eq!(level.amount_decimals, 6);

    let selling = LevelDraft {
        token_in: "WETH".into(),
        token_out: "USDC".into(),
        side: "sell".into(),
        amount: "0.5".into(),
        ..draft()
    };
    let level = validate_level(&evm(), MAX_SLIPPAGE_BPS, &selling).unwrap();
    assert_eq!(level.amount, U256::from(500_000_000_000_000_000u64));
    assert_eq!(level.amount_decimals, 18);
}

// The stored spelling comes from the configuration, because that is what the
// resolver looks the chain up by when a tick arrives.
#[test]
fn the_chain_is_stored_as_the_configuration_spells_it() {
    let level = validate_level(
        &evm(),
        MAX_SLIPPAGE_BPS,
        &LevelDraft {
            chain: "BASE".into(),
            token_in: "usdc".into(),
            token_out: "weth".into(),
            ..draft()
        },
    )
    .unwrap();

    assert_eq!(level.chain, "base");
    assert_eq!(level.token_in, "USDC");
    assert_eq!(level.token_out, "WETH");
}

// Nothing could ever price these, so they would sit in the database looking
// armed and never fire.
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

// The venue would refuse this on every single tick, so the rule would burn a
// quote a minute and never trade.
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
