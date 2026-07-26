//! The price feed keeping one shared subscription per active pair.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use alloy_primitives::U256;
use anyhow::Result;
use async_trait::async_trait;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, SuiConfig};
use tempo_agentic_domain::VenueName;
use tempo_agentic_price::{PricePair, PriceSource, PriceStream, PriceTick};
use tempo_agentic_strategy::{Level, LevelStore, Side, Strategy, StrategyLevel};
use tempo_agentic_trigger::{FeedHealth, RuntimeStatus, TokenResolver, produce_every, pump};
use tokio::sync::mpsc;

// The gates drop anything older than two minutes, so a tick has to be dated now.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

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
            venue: VenueName::Uniswap,
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
                            usd_peg: false,
                        },
                    ),
                    (
                        "USDC".into(),
                        EvmToken {
                            address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                            decimals: 6,
                            usd_peg: false,
                        },
                    ),
                ]),
            }],
        },
        &SuiConfig::default(),
    )
}
