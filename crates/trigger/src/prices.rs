use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tempo_agentic_price::{PricePair, PriceSource, PriceTick};
use tempo_agentic_strategy::LevelStore;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::TokenResolver;

/// Poll interval for rule subscription changes.
const RECONCILE_SECS: u64 = 30;

/// Keeps one shared subscription per active price pair.
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

// A finished stream means the pair is unsupported.
async fn pump(source: Arc<dyn PriceSource>, pair: PricePair, ticks: mpsc::Sender<PriceTick>) {
    let mut stream = source.stream(&pair);
    while let Some(item) = stream.next().await {
        match item {
            // A full channel applies backpressure instead of buffering stale ticks.
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
