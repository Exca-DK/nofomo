use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tempo_agentic_price::{PricePair, PriceSource, PriceTick};
use tempo_agentic_strategy::LevelStore;
use tempo_agentic_trigger::TokenResolver;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// How often the subscription list is compared against the stored levels. Rules
/// come and go through the admin CLI, which the daemon has no other signal for.
const RECONCILE_SECS: u64 = 30;

/// Keeps one subscription running per priced pair, for as long as some level
/// wants it.
///
/// Subscriptions are keyed by pair rather than by level on purpose: three rules
/// on WETH/base are one stream, not three, and `fired_levels` matches a single
/// tick against every level anyway.
pub async fn produce(
    levels: Arc<dyn LevelStore>,
    resolver: TokenResolver,
    source: Arc<dyn PriceSource>,
    ticks: mpsc::Sender<PriceTick>,
) {
    let mut tasks: HashMap<PricePair, JoinHandle<()>> = HashMap::new();
    loop {
        match levels.list_levels().await {
            Ok(stored) => {
                let wanted: HashSet<PricePair> = stored
                    .iter()
                    .filter_map(|level| resolver.price_pair(level))
                    .collect();

                tasks.retain(|pair, task| {
                    let keep = wanted.contains(pair);
                    if !keep {
                        tracing::info!(token = %pair.token_address, "no level needs this price any more");
                        task.abort();
                    }
                    keep
                });

                for pair in wanted {
                    tasks.entry(pair.clone()).or_insert_with(|| {
                        tracing::info!(
                            chain = pair.chain_id,
                            token = %pair.token_address,
                            "subscribing to a price stream"
                        );
                        tokio::spawn(pump(source.clone(), pair, ticks.clone()))
                    });
                }
            }
            Err(error) => tracing::warn!(%error, "cannot read levels; keeping the current streams"),
        }
        tokio::time::sleep(Duration::from_secs(RECONCILE_SECS)).await;
    }
}

// A finished stream is not restarted. The `PriceSource` contract says it ends
// only when the pair cannot be served at all, so retrying would be busywork.
async fn pump(source: Arc<dyn PriceSource>, pair: PricePair, ticks: mpsc::Sender<PriceTick>) {
    let mut stream = source.stream(&pair);
    while let Some(item) = stream.next().await {
        match item {
            // A full channel blocks here, which is the point: it holds the feed
            // back rather than piling up quotes nobody will act on.
            Ok(tick) => {
                if ticks.send(tick).await.is_err() {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(token = %pair.token_address, %error, "price stream error");
            }
        }
    }
    tracing::warn!(
        chain = pair.chain_id,
        token = %pair.token_address,
        "price stream ended; this pair will not be priced again until a restart"
    );
}
