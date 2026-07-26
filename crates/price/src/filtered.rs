use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;

use crate::{PricePair, PriceSource, PriceStream, PriceTick, is_implausible, is_stale};

/// Drops stale or implausible quotes from a price source.
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
    // Support is decided by the wrapped source.
    fn supports(&self, pair: &PricePair) -> bool {
        self.inner.supports(pair)
    }

    fn stream(&self, pair: &PricePair) -> PriceStream {
        let inner = self.inner.stream(pair);
        let max_age_secs = self.max_age_secs;
        let max_move_bps = self.max_move_bps;

        // Each pair stream keeps its own previous price.
        let filtered = inner
            .scan(None::<f64>, move |previous, item| {
                let decision = match item {
                    Err(error) => Some(Err(error)),
                    Ok(tick) => keep(tick, previous, max_age_secs, max_move_bps).map(Ok),
                };
                // Dropped quotes do not end the stream.
                futures::future::ready(Some(decision))
            })
            .filter_map(futures::future::ready);
        Box::pin(filtered)
    }
}

// Log rejections so a broken feed is visible.
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
        // A rejected quote must not become the new baseline.
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
