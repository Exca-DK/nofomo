use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;

use crate::{PricePair, PriceSource, PriceStream, PriceTick, is_implausible, is_stale};

/// Wraps a price source, dropping quotes that are too old or that moved further
/// than a real market would.
pub struct FilteredSource<S> {
    inner: S,
    max_age_secs: i64,
    max_move_bps: u32,
}

impl<S> FilteredSource<S> {
    pub fn new(inner: S, max_age_secs: i64, max_move_bps: u32) -> Self {
        Self {
            inner,
            max_age_secs,
            max_move_bps,
        }
    }
}

impl<S: PriceSource> PriceSource for FilteredSource<S> {
    fn stream(&self, pair: &PricePair) -> PriceStream {
        let inner = self.inner.stream(pair);
        let max_age_secs = self.max_age_secs;
        let max_move_bps = self.max_move_bps;

        // The previous price lives in the stream rather than in `self`: each
        // call is for one pair and needs its own history.
        let filtered = inner
            .scan(None::<f64>, move |previous, item| {
                let decision = match item {
                    Err(error) => Some(Err(error)),
                    Ok(tick) => keep(tick, previous, max_age_secs, max_move_bps).map(Ok),
                };
                // Always `Some`, so a dropped quote never ends the stream.
                futures::future::ready(Some(decision))
            })
            .filter_map(futures::future::ready);
        Box::pin(filtered)
    }
}

// Rejections are logged rather than dropped quietly: a feed that has frozen or
// gone haywire rejects everything, which is indistinguishable from a flat market
// unless it says so.
fn keep(
    tick: PriceTick,
    previous: &mut Option<f64>,
    max_age_secs: i64,
    max_move_bps: u32,
) -> Option<PriceTick> {
    if is_stale(&tick, now_secs(), max_age_secs) {
        tracing::warn!(
            token = %tick.pair.token_address,
            published_at = tick.published_at,
            "dropping stale price"
        );
        return None;
    }
    if let Some(previous) = *previous
        && is_implausible(previous, tick.price_usd, max_move_bps)
    {
        tracing::warn!(
            token = %tick.pair.token_address,
            previous,
            next = tick.price_usd,
            "dropping implausible price move"
        );
        // `previous` is deliberately left alone: one bad quote must not become
        // the baseline that rejects every good one after it.
        return None;
    }
    *previous = Some(tick.price_usd);
    Some(tick)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}
