use std::str::FromStr;

use alloy_primitives::U256;
use tempo_agentic_domain::VenueName;
use tempo_agentic_strategy::{Level, Side, Strategy, level_fires, trade_direction};

fn strategy() -> Strategy {
    Strategy {
        id: "s-1".into(),
        venue: VenueName::Uniswap,
        chain: "base".into(),
        base_token: "WETH".into(),
        quote_token: "USDC".into(),
    }
}

fn level(side: Side) -> Level {
    Level {
        id: "l-1".into(),
        strategy_id: "s-1".into(),
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
fn buy_and_sell_have_one_market_direction() {
    let strategy = strategy();
    let buy = trade_direction(&strategy, Side::Buy);
    assert_eq!((buy.token_in, buy.token_out), ("USDC", "WETH"));
    let sell = trade_direction(&strategy, Side::Sell);
    assert_eq!((sell.token_in, sell.token_out), ("WETH", "USDC"));
}

#[test]
fn side_round_trips_through_its_sqlite_form() {
    for side in [Side::Buy, Side::Sell] {
        assert_eq!(Side::from_str(side.as_str()).unwrap(), side);
    }
    assert!(Side::from_str("hold").is_err());
}
