use tempo_agentic_price::{
    DEFAULT_MAX_AGE_SECS, FUTURE_TOLERANCE_SECS, PricePair, PriceTick, is_implausible, is_stale,
};

const NOW: i64 = 1_800_000_000;

fn tick(published_at: i64) -> PriceTick {
    PriceTick {
        pair: PricePair::new("base", "0x4200000000000000000000000000000000000006"),
        price_usd: 1_600.0,
        published_at,
    }
}

#[test]
fn a_recent_tick_is_fresh() {
    assert!(!is_stale(&tick(NOW - 10), NOW, DEFAULT_MAX_AGE_SECS));
    assert!(!is_stale(&tick(NOW), NOW, DEFAULT_MAX_AGE_SECS));
}

#[test]
fn the_age_limit_is_inclusive() {
    let exactly_at_limit = NOW - DEFAULT_MAX_AGE_SECS;
    assert!(!is_stale(
        &tick(exactly_at_limit),
        NOW,
        DEFAULT_MAX_AGE_SECS
    ));
    assert!(is_stale(
        &tick(exactly_at_limit - 1),
        NOW,
        DEFAULT_MAX_AGE_SECS
    ));
}

#[test]
fn a_frozen_feed_goes_stale() {
    assert!(is_stale(&tick(NOW - 3_600), NOW, DEFAULT_MAX_AGE_SECS));
}

// Clocks drift by fractions of a second; that must not throw away good quotes.
#[test]
fn a_slightly_early_tick_is_still_fresh() {
    assert!(!is_stale(
        &tick(NOW + FUTURE_TOLERANCE_SECS),
        NOW,
        DEFAULT_MAX_AGE_SECS
    ));
}

// Without this, a feed whose clock runs fast would look permanently fresh and
// the age limit would never fire.
#[test]
fn a_tick_from_the_future_is_rejected() {
    assert!(is_stale(
        &tick(NOW + FUTURE_TOLERANCE_SECS + 1),
        NOW,
        DEFAULT_MAX_AGE_SECS
    ));
    assert!(is_stale(&tick(NOW + 3_600), NOW, DEFAULT_MAX_AGE_SECS));
}

#[test]
fn an_ordinary_move_is_plausible() {
    // 1% against a 5% limit.
    assert!(!is_implausible(100.0, 101.0, 500));
    assert!(!is_implausible(100.0, 99.0, 500));
}

#[test]
fn the_move_limit_is_inclusive() {
    // Exactly 5%.
    assert!(!is_implausible(100.0, 105.0, 500));
    assert!(is_implausible(100.0, 105.01, 500));
}

#[test]
fn a_large_move_is_implausible_in_both_directions() {
    assert!(is_implausible(100.0, 200.0, 500));
    assert!(is_implausible(100.0, 1.0, 500));
}

// Anything that cannot be compared meaningfully is rejected rather than assumed
// good: these are the shapes a broken feed produces.
#[test]
fn unusable_values_are_rejected() {
    assert!(is_implausible(0.0, 100.0, 500));
    assert!(is_implausible(-1.0, 100.0, 500));
    assert!(is_implausible(100.0, 0.0, 500));
    assert!(is_implausible(100.0, -1.0, 500));
    assert!(is_implausible(f64::NAN, 100.0, 500));
    assert!(is_implausible(100.0, f64::NAN, 500));
    assert!(is_implausible(f64::INFINITY, 100.0, 500));
    assert!(is_implausible(100.0, f64::INFINITY, 500));
}
