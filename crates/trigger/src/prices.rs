use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use tempo_agentic_price::{PricePair, PriceSource, PriceTick};
use tempo_agentic_strategy::LevelStore;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::TokenResolver;
use crate::runtime::{RuntimeStatus, now_secs};

/// Poll interval for rule subscription changes.
const RECONCILE_SECS: u64 = 30;

/// Keeps one shared subscription per active price pair.
pub async fn produce(
    levels: Arc<dyn LevelStore>,
    resolver: Arc<TokenResolver>,
    source: Arc<dyn PriceSource>,
    ticks: mpsc::Sender<PriceTick>,
    runtime: Arc<RuntimeStatus>,
) {
    produce_every(
        levels,
        resolver,
        source,
        ticks,
        runtime,
        Duration::from_secs(RECONCILE_SECS),
    )
    .await;
}

/// Reconciles on `reconcile_every` instead of the fixed interval.
///
/// Exposed only so a test need not wait out [`RECONCILE_SECS`]; not part of the
/// crate's contract.
#[doc(hidden)]
pub async fn produce_every(
    levels: Arc<dyn LevelStore>,
    resolver: Arc<TokenResolver>,
    source: Arc<dyn PriceSource>,
    ticks: mpsc::Sender<PriceTick>,
    runtime: Arc<RuntimeStatus>,
    reconcile_every: Duration,
) {
    let mut tasks: HashMap<PricePair, JoinHandle<()>> = HashMap::new();
    loop {
        match levels.list_levels().await {
            Ok(stored) => {
                let wanted: HashSet<PricePair> = stored
                    .iter()
                    .filter_map(|entry| resolver.price_pair(&entry.strategy))
                    .collect();

                tasks.retain(|pair, task| {
                    let wanted = wanted.contains(pair);
                    let keep = wanted && !task.is_finished();
                    if !wanted {
                        tracing::info!(token = %pair.token_address, "no level needs this price any more");
                        task.abort();
                        runtime.remove_feed(pair);
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
                        tokio::spawn(pump(source.clone(), pair, ticks.clone(), runtime.clone()))
                    });
                }
            }
            Err(error) => tracing::warn!(%error, "cannot read levels; keeping the current streams"),
        }
        tokio::time::sleep(reconcile_every).await;
    }
}

/// Forwards one pair's stream into the trading queue.
///
/// Exposed only so a test can drive a single feed; not part of the contract.
#[doc(hidden)]
pub async fn pump(
    source: Arc<dyn PriceSource>,
    pair: PricePair,
    ticks: mpsc::Sender<PriceTick>,
    runtime: Arc<RuntimeStatus>,
) {
    runtime.feed_connecting(pair.clone(), now_secs());
    let mut stream = source.stream(&pair);
    while let Some(item) = stream.next().await {
        match item {
            Ok(tick) => {
                // Runtime stays trustworthy even while a full trading queue applies backpressure.
                runtime.feed_tick(&tick, now_secs());
                if ticks.send(tick).await.is_err() {
                    runtime.feed_ended(&pair, now_secs());
                    return;
                }
            }
            Err(error) => {
                runtime.feed_error(&pair, now_secs());
                tracing::warn!(token = %pair.token_address, %error, "price stream error");
            }
        }
    }
    runtime.feed_ended(&pair, now_secs());
    tracing::warn!(
        chain = pair.chain_id,
        token = %pair.token_address,
        "price stream ended; the subscription will be retried"
    );
}
