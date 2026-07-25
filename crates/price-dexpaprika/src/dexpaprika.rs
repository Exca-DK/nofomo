use std::time::Duration;

use futures::StreamExt;
use reqwest_eventsource::{Event, EventSource, retry::ExponentialBackoff};
use tempo_agentic_price::{PricePair, PriceSource, PriceStream, PriceTick};

use crate::wire::{chain_slug, parse_tick};

/// Delay before the first reconnect, doubling up to [`MAX_RECONNECT_DELAY`].
const FIRST_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// Live prices from DexPaprika's server-sent event feed.
pub struct DexPaprikaSource {
    base_url: String,
}

impl DexPaprikaSource {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

// Session state carried across polls by `unfold`.
struct Feed {
    url: String,
    pair: PricePair,
    session: Option<EventSource>,
    /// Consecutive sessions that ended without delivering a tick. Reset by a
    /// delivered tick, so a connection that worked for a week reconnects
    /// promptly instead of starting at the maximum delay.
    failures: u32,
}

impl PriceSource for DexPaprikaSource {
    fn stream(&self, pair: &PricePair) -> PriceStream {
        // DexPaprika names chains itself, so a chain it has no name for cannot
        // be quoted. Retrying would never help, so this is the one case where
        // the stream reports and ends instead of reconnecting forever.
        let Some(slug) = chain_slug(pair.chain_id) else {
            let chain_id = pair.chain_id;
            return Box::pin(futures::stream::once(async move {
                Err(anyhow::anyhow!(
                    "DexPaprika does not price chain {chain_id}"
                ))
            }));
        };
        let feed = Feed {
            url: format!(
                "{}/sse/prices?method=token_price&chain={slug}&address={}",
                self.base_url.trim_end_matches('/'),
                pair.token_address
            ),
            pair: pair.clone(),
            session: None,
            failures: 0,
        };
        Box::pin(futures::stream::unfold(feed, |mut feed| async move {
            Some((next_tick(&mut feed).await, feed))
        }))
    }
}

// Runs until it has something to hand the caller, reconnecting as often as
// needed. `EventSource` retries within a session, but closes for good on any
// non-2xx response, so without this loop one transient 503 would silently end
// the feed and the daemon would stop trading without saying so.
async fn next_tick(feed: &mut Feed) -> anyhow::Result<PriceTick> {
    loop {
        let session = match feed.session.as_mut() {
            Some(session) => session,
            None => {
                if feed.failures > 0 {
                    let delay = reconnect_delay(feed.failures);
                    tracing::warn!(
                        pair = %feed.pair.token_address,
                        failures = feed.failures,
                        ?delay,
                        "price feed dropped; reconnecting"
                    );
                    tokio::time::sleep(delay).await;
                }
                let mut source = EventSource::get(&feed.url);
                source.set_retry_policy(Box::new(ExponentialBackoff::new(
                    FIRST_RECONNECT_DELAY,
                    2.0,
                    Some(MAX_RECONNECT_DELAY),
                    None,
                )));
                feed.session.insert(source)
            }
        };

        match session.next().await {
            Some(Ok(Event::Open)) => {}
            Some(Ok(Event::Message(message))) => {
                if let Some(tick) = parse_tick(&feed.pair, &message.data) {
                    feed.failures = 0;
                    return Ok(tick);
                }
                // Heartbeats and frames about other tokens are not errors.
            }
            // The session keeps retrying after this, so surface the error and
            // let the caller decide; only a `None` means it gave up.
            Some(Err(error)) => return Err(anyhow::anyhow!(error)),
            None => {
                feed.session = None;
                feed.failures = feed.failures.saturating_add(1);
            }
        }
    }
}

fn reconnect_delay(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    FIRST_RECONNECT_DELAY
        .saturating_mul(1u32 << shift)
        .min(MAX_RECONNECT_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_delay_grows_and_caps() {
        assert_eq!(reconnect_delay(1), Duration::from_secs(1));
        assert_eq!(reconnect_delay(2), Duration::from_secs(2));
        assert_eq!(reconnect_delay(3), Duration::from_secs(4));
        assert_eq!(reconnect_delay(6), Duration::from_secs(30));
        // A feed down for days must not overflow into a tiny delay.
        assert_eq!(reconnect_delay(u32::MAX), MAX_RECONNECT_DELAY);
    }
}
