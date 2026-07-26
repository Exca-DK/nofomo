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

async fn produce_every(
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

async fn pump(
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use alloy_primitives::U256;
    use anyhow::Result;
    use async_trait::async_trait;
    use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, SuiConfig};
    use tempo_agentic_price::{PriceStream, PriceTick};
    use tempo_agentic_strategy::{Level, Side, Strategy, StrategyLevel};

    use super::*;
    use crate::FeedHealth;

    struct Levels(Vec<StrategyLevel>);

    #[async_trait]
    impl LevelStore for Levels {
        async fn upsert_level(&self, _level: &Level, _strategy: &Strategy) -> Result<()> {
            Ok(())
        }

        async fn get_level(&self, id: &str) -> Result<Option<StrategyLevel>> {
            Ok(self.0.iter().find(|entry| entry.level.id == id).cloned())
        }

        async fn list_levels(&self) -> Result<Vec<StrategyLevel>> {
            Ok(self.0.clone())
        }

        async fn delete_level(&self, _id: &str) -> Result<()> {
            Ok(())
        }
    }

    struct EndsImmediately(AtomicUsize);

    impl PriceSource for EndsImmediately {
        fn stream(&self, _pair: &PricePair) -> PriceStream {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(futures::stream::empty())
        }
    }

    struct OneTick(PriceTick);

    impl PriceSource for OneTick {
        fn stream(&self, _pair: &PricePair) -> PriceStream {
            Box::pin(futures::stream::iter([Ok(self.0.clone())]))
        }
    }

    #[tokio::test]
    async fn a_finished_stream_is_removed_and_retried() {
        let source = Arc::new(EndsImmediately(AtomicUsize::new(0)));
        let runtime = Arc::new(RuntimeStatus::new(false, 30));
        let (tx, _rx) = mpsc::channel(1);
        let producer = tokio::spawn(produce_every(
            Arc::new(Levels(vec![level()])),
            Arc::new(resolver()),
            source.clone(),
            tx,
            runtime,
            Duration::from_millis(1),
        ));

        tokio::time::timeout(Duration::from_millis(100), async {
            while source.0.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        producer.abort();
    }

    #[tokio::test]
    async fn runtime_observes_a_tick_before_the_trading_queue_can_block() {
        let tick = PriceTick {
            pair: pair(),
            price_usd: 3_000.0,
            published_at: now_secs(),
        };
        let runtime = Arc::new(RuntimeStatus::new(false, 30));
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(tick.clone()).await.unwrap();
        let task = tokio::spawn(pump(Arc::new(OneTick(tick)), pair(), tx, runtime.clone()));

        tokio::time::timeout(Duration::from_millis(100), async {
            while runtime
                .snapshot()
                .feeds
                .first()
                .and_then(|feed| feed.last_tick.as_ref())
                .is_none()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(runtime.snapshot().feeds[0].health, FeedHealth::Live);
        assert!(
            !task.is_finished(),
            "the full queue should still apply backpressure"
        );

        rx.recv().await.unwrap();
        task.await.unwrap();
    }

    fn pair() -> PricePair {
        PricePair::new(8453, "0x4200000000000000000000000000000000000006")
    }

    fn level() -> StrategyLevel {
        StrategyLevel {
            strategy: Strategy {
                id: "s-1".into(),
                venue: tempo_agentic_domain::VenueName::Uniswap,
                chain: "base".into(),
                base_token: "WETH".into(),
                quote_token: "USDC".into(),
            },
            level: Level {
                id: "l-1".into(),
                strategy_id: "s-1".into(),
                side: Side::Buy,
                trigger_price_usd: 3_000.0,
                amount: U256::ONE,
                amount_decimals: 6,
                slippage_bps: 50,
            },
        }
    }

    fn resolver() -> TokenResolver {
        TokenResolver::from_config(
            &EvmConfig {
                chains: vec![EvmChain {
                    name: "base".into(),
                    chain_id: 8453,
                    rpc_url: "https://example.invalid".into(),
                    graph_subgraph_id: "subgraph".into(),
                    tokens: HashMap::from([
                        (
                            "WETH".into(),
                            EvmToken {
                                address: pair().token_address,
                                decimals: 18,
                            },
                        ),
                        (
                            "USDC".into(),
                            EvmToken {
                                address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                                decimals: 6,
                            },
                        ),
                    ]),
                }],
            },
            &SuiConfig::default(),
        )
    }
}
