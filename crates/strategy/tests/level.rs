use std::str::FromStr;

use alloy_primitives::U256;
use tempo_agentic_domain::VenueName;
use tempo_agentic_strategy::{Level, Side, base_token, level_fires};

fn level(side: Side) -> Level {
    Level {
        id: "l-1".into(),
        venue: VenueName::Uniswap,
        chain: "base".into(),
        token_in: "USDC".into(),
        token_out: "WETH".into(),
        side,
        trigger_price_usd: 3_000.0,
        amount: U256::from(1_000_000u64),
        amount_decimals: 6,
        slippage_bps: 50,
    }
}

#[test]
fn buy_fires_at_or_below_trigger() {
    let level = level(Side::Buy);
    assert!(level_fires(&level, 2_999.99));
    assert!(level_fires(&level, 3_000.0));
    assert!(!level_fires(&level, 3_000.01));
}

#[test]
fn sell_fires_at_or_above_trigger() {
    let level = level(Side::Sell);
    assert!(level_fires(&level, 3_000.01));
    assert!(level_fires(&level, 3_000.0));
    assert!(!level_fires(&level, 2_999.99));
}

#[test]
fn base_token_is_the_asset_being_traded() {
    assert_eq!(base_token(&level(Side::Buy)), "WETH");
    assert_eq!(base_token(&level(Side::Sell)), "USDC");
}

#[test]
fn side_round_trips_through_its_sqlite_form() {
    for side in [Side::Buy, Side::Sell] {
        assert_eq!(Side::from_str(side.as_str()).unwrap(), side);
    }
    assert!(Side::from_str("hold").is_err());
}
