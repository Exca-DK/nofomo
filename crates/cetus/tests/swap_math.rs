use tempo_agentic_cetus::constants::{FEE_RATE_DENOMINATOR, MAX_SQRT_PRICE, MIN_SQRT_PRICE};
use tempo_agentic_cetus::swap_math::{PoolState, TickData, compute_swap};

fn flat_pool() -> PoolState {
    PoolState {
        current_sqrt_price: 1u128 << 64, // price == 1.0
        current_tick_index: 0,
        liquidity: 1_000_000_000_000,
        fee_rate: 2_500, // 0.25%
    }
}

fn wide_open_ticks() -> [TickData; 2] {
    [
        TickData {
            index: -100,
            sqrt_price: MIN_SQRT_PRICE,
            liquidity_net: 0,
        },
        TickData {
            index: 100,
            sqrt_price: MAX_SQRT_PRICE,
            liquidity_net: 0,
        },
    ]
}

#[test]
fn a_to_b_single_tick_matches_golden_vector() {
    let result = compute_swap(true, 1_000_000, flat_pool(), &wide_open_ticks()).unwrap();
    assert_eq!(result.amount_in, 1_000_000);
    assert_eq!(result.amount_out, 997_499);
    assert_eq!(result.fee_amount, 2_500);
    assert_eq!(result.cross_tick_count, 1);
    assert_eq!(result.next_sqrt_price, 18_446_725_673_100_692_699);
}

#[test]
fn b_to_a_single_tick_matches_golden_vector() {
    let result = compute_swap(false, 1_000_000, flat_pool(), &wide_open_ticks()).unwrap();
    assert_eq!(result.amount_in, 1_000_000);
    assert_eq!(result.amount_out, 997_499);
    assert_eq!(result.fee_amount, 2_500);
    assert_eq!(result.cross_tick_count, 1);
    assert_eq!(result.next_sqrt_price, 18_446_762_474_336_765_141);
}

#[test]
fn a_to_b_crosses_multiple_ticks_and_updates_liquidity() {
    let mut pool = flat_pool();
    pool.liquidity = 1_000_000;
    let ticks = [
        TickData {
            index: -200,
            sqrt_price: MIN_SQRT_PRICE,
            liquidity_net: 0,
        },
        // Crossed first walking down from tick 0: liquidity halves.
        TickData {
            index: -10,
            sqrt_price: (99u128 << 64) / 100,
            liquidity_net: -500_000,
        },
        TickData {
            index: 200,
            sqrt_price: MAX_SQRT_PRICE,
            liquidity_net: 0,
        },
    ];
    let result = compute_swap(true, 50_000, pool, &ticks).unwrap();
    assert_eq!(result.cross_tick_count, 2);
    assert_eq!(result.amount_in, 50_000);
    assert_eq!(result.amount_out, 46_134);
    assert_eq!(result.fee_amount, 126);
    assert_eq!(result.next_sqrt_price, 16_929_131_875_710_180_415);
}

#[test]
fn zero_fee_rate_charges_nothing() {
    let mut pool = flat_pool();
    pool.fee_rate = 0;
    let result = compute_swap(true, 1_000_000, pool, &wide_open_ticks()).unwrap();
    assert_eq!(result.fee_amount, 0);
    assert_eq!(result.amount_in, 1_000_000);
    assert_eq!(result.amount_out, 999_999);
}

#[test]
fn high_fee_rate_still_fills_but_takes_most_of_the_input() {
    let mut pool = flat_pool();
    pool.fee_rate = (FEE_RATE_DENOMINATOR / 10) * 9; // 90%
    let result = compute_swap(true, 1_000_000, pool, &wide_open_ticks()).unwrap();
    assert_eq!(result.amount_in, 1_000_000);
    assert_eq!(result.fee_amount, 900_000);
    assert!(result.amount_out > 0 && result.amount_out < 100_000);
}

#[test]
fn insufficient_liquidity_in_fetched_tick_window_is_rejected() {
    let pool = flat_pool();
    let ticks = [TickData {
        index: 1,
        sqrt_price: pool.current_sqrt_price + 1,
        liquidity_net: -1_000_000_000_000,
    }];
    let error = compute_swap(false, u128::MAX / 2, pool, &ticks).unwrap_err();
    assert!(
        error.to_string().contains("insufficient liquidity"),
        "unexpected error: {error}"
    );
}

#[test]
fn empty_tick_window_yields_no_fill() {
    let pool = flat_pool();
    let error = compute_swap(true, 1_000_000, pool, &[]).unwrap_err();
    assert!(
        error.to_string().contains("insufficient liquidity"),
        "unexpected error: {error}"
    );
}
