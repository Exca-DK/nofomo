use crate::PriceTick;

/// A tick older than this is not worth trading on.
pub const DEFAULT_MAX_AGE_SECS: i64 = 120;

/// How far ahead of the local clock a feed may be before its timestamps are
/// treated as wrong rather than merely skewed.
pub const FUTURE_TOLERANCE_SECS: i64 = 5;

/// A jump larger than this between consecutive quotes is treated as a broken
/// feed rather than a market move.
pub const DEFAULT_MAX_MOVE_BPS: u32 = 1_000;

/// Whether a tick is too old, or dated far enough ahead that its clock cannot
/// be trusted.
///
/// The future check matters: comparing only `now - published_at` would let a
/// feed with a fast clock look permanently fresh, defeating the gate entirely.
pub fn is_stale(tick: &PriceTick, now: i64, max_age_secs: i64) -> bool {
    let age = now.saturating_sub(tick.published_at);
    age > max_age_secs || age < -FUTURE_TOLERANCE_SECS
}

/// Whether a price moved further from the previous one than a real market would.
///
/// Anything that cannot be compared meaningfully counts as implausible: a zero
/// or negative previous price, or a non-finite value on either side.
pub fn is_implausible(prev_usd: f64, next_usd: f64, max_move_bps: u32) -> bool {
    if !prev_usd.is_finite() || !next_usd.is_finite() {
        return true;
    }
    if prev_usd <= 0.0 || next_usd <= 0.0 {
        return true;
    }
    let move_bps = ((next_usd - prev_usd).abs() / prev_usd) * 10_000.0;
    move_bps > f64::from(max_move_bps)
}
