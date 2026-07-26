use crate::PriceTick;

/// A tick older than this is not worth trading on.
pub const DEFAULT_MAX_AGE_SECS: i64 = 120;

/// Maximum tolerated feed clock skew.
pub const FUTURE_TOLERANCE_SECS: i64 = 5;

/// Maximum plausible move between consecutive quotes.
pub const DEFAULT_MAX_MOVE_BPS: u32 = 1_000;

/// Rejects stale ticks and timestamps beyond the allowed clock skew.
pub fn is_stale(tick: &PriceTick, now: i64, max_age_secs: i64) -> bool {
    let age = now.saturating_sub(tick.published_at);
    age > max_age_secs || age < -FUTURE_TOLERANCE_SECS
}

/// Rejects excessive moves and invalid values.
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
