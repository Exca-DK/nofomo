use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::U256;
use async_trait::async_trait;
use serde_json::json;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, SuiConfig};
use tempo_agentic_domain::{
    ExecStep, ExecutionPlan, QuoteDraft, QuoteTradeRequest, TradeVenue, TxContext, UnsignedTx,
    VenueName,
};
use tempo_agentic_price::{PricePair, PriceTick};
use tempo_agentic_storage::{
    LockFile, SqliteLevelStore, SqliteOrderStore, SqliteStrategyStore, initialize_new_under_lock,
};
use tempo_agentic_strategy::{
    Level, LevelStore, Order, OrderState, OrderStore, Side, Strategy, StrategyLevel, StrategyStore,
};
use tempo_agentic_trigger::{RuntimeStatus, TokenResolver, TriggerDeps, run};
use tokio::sync::{Notify, mpsc};

const BASE_ID: u64 = 8453;
const WETH: &str = "0x4200000000000000000000000000000000000006";
const USDC: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";

/// Answers every quote the same way, and counts how often it was asked.
struct ScriptedVenue {
    accepts: bool,
    calls: Arc<AtomicUsize>,
    request: Arc<Mutex<Option<(String, String, String)>>>,
}

#[async_trait]
impl TradeVenue for ScriptedVenue {
    fn name(&self) -> &'static str {
        "uniswap"
    }

    async fn quote(&self, request: &QuoteTradeRequest) -> anyhow::Result<QuoteDraft> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.request.lock().unwrap() = Some((
            request.token_in.clone(),
            request.token_out.clone(),
            request.amount.clone(),
        ));
        anyhow::ensure!(self.accepts, "pre-flight rejected: insufficient balance");
        Ok(QuoteDraft {
            venue: "uniswap".into(),
            chain: "base".into(),
            token_in: request.token_in.clone(),
            token_out: request.token_out.clone(),
            amount_in: request.amount.clone(),
            expected_amount_out: "1".into(),
            minimum_amount_out: "1".into(),
            graph_guard: "ok".into(),
            plan: ExecutionPlan::Uniswap {
                chain_name: "base".into(),
                chain_id: BASE_ID,
                input_token: USDC.into(),
                input_amount: "1000000".into(),
                quote: json!({"tradeType": "EXACT_INPUT"}),
            },
        })
    }

    async fn steps(&self, _plan: &ExecutionPlan) -> anyhow::Result<Vec<ExecStep>> {
        Ok(vec![ExecStep::Swap])
    }

    async fn build(
        &self,
        _plan: &ExecutionPlan,
        _step: ExecStep,
        _ctx: &TxContext,
    ) -> anyhow::Result<UnsignedTx> {
        anyhow::bail!("not needed for these tests")
    }
}

struct Fixture {
    strategies: SqliteStrategyStore,
    levels: Arc<SqliteLevelStore>,
    orders: Arc<SqliteOrderStore>,
    calls: Arc<AtomicUsize>,
    request: Arc<Mutex<Option<(String, String, String)>>>,
    runtime: Arc<RuntimeStatus>,
    path: PathBuf,
}

impl Fixture {
    async fn new(name: &str, accepts: bool) -> (Self, TriggerDeps) {
        let path = std::env::temp_dir().join(format!(
            "tempo-agentic-trigger-{name}-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let lock = LockFile::acquire(LockFile::path_for(&path)).unwrap();
        let pool = initialize_new_under_lock(&path, &lock).await.unwrap();
        let strategies = SqliteStrategyStore::new(pool.clone());
        let levels = Arc::new(SqliteLevelStore::new(pool.clone()));
        let orders = Arc::new(SqliteOrderStore::new(pool));
        let calls = Arc::new(AtomicUsize::new(0));
        let request = Arc::new(Mutex::new(None));
        let runtime = Arc::new(RuntimeStatus::new(false, 30));

        let deps = TriggerDeps {
            levels: levels.clone(),
            orders: orders.clone(),
            venues: vec![Arc::new(ScriptedVenue {
                accepts,
                calls: calls.clone(),
                request: request.clone(),
            })],
            resolver: std::sync::Arc::new(resolver()),
            runtime: runtime.clone(),
            max_quote_deviation_bps: 500,
        };
        (
            Self {
                strategies,
                levels,
                orders,
                calls,
                request,
                runtime,
                path,
            },
            deps,
        )
    }

    async fn store(&self, entry: &StrategyLevel) {
        self.strategies
            .upsert_strategy(&entry.strategy)
            .await
            .unwrap();
        self.levels
            .upsert_level(&entry.level, &entry.strategy)
            .await
            .unwrap();
    }

    fn cleanup(self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

fn resolver() -> TokenResolver {
    TokenResolver::from_config(
        &EvmConfig {
            chains: vec![EvmChain {
                name: "base".into(),
                chain_id: BASE_ID,
                rpc_url: "https://example.invalid".into(),
                graph_subgraph_id: "subgraph".into(),
                tokens: HashMap::from([
                    (
                        "WETH".to_string(),
                        EvmToken {
                            address: WETH.into(),
                            decimals: 18,
                            usd_peg: false,
                        },
                    ),
                    (
                        "USDC".to_string(),
                        EvmToken {
                            address: USDC.into(),
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
            amount: U256::from(1_000_000u64),
            amount_decimals: 6,
            slippage_bps: 50,
        },
    }
}

fn tick(price_usd: f64, published_at: i64) -> PriceTick {
    PriceTick {
        pair: PricePair::new(BASE_ID, WETH),
        price_usd,
        published_at,
    }
}

/// Feeds the loop a fixed set of ticks and waits for it to drain them.
async fn drive(deps: TriggerDeps, waker: Arc<Notify>, ticks: Vec<PriceTick>) {
    let (tx, rx) = mpsc::channel(16);
    let loop_task = tokio::spawn(run(deps, rx, waker));
    for tick in ticks {
        tx.send(tick).await.unwrap();
    }
    // Closing the sender drains and ends the loop.
    drop(tx);
    loop_task.await.unwrap();
}

#[tokio::test]
async fn a_fired_level_becomes_a_stored_order() {
    let (fixture, deps) = Fixture::new("creates", true).await;
    fixture.store(&level()).await;
    let waker = Arc::new(Notify::new());

    drive(deps, waker.clone(), vec![tick(2_999.0, 100)]).await;

    let orders = fixture.orders.list_orders().await.unwrap();
    assert_eq!(orders.len(), 1);
    let order = &orders[0];
    assert_eq!(order.level_id, "l-1");
    assert_eq!(order.token_in, "USDC");
    assert_eq!(order.token_out, "WETH");
    assert_eq!(
        fixture.request.lock().unwrap().as_ref().unwrap(),
        &("USDC".into(), "WETH".into(), "1".into())
    );
    assert!(matches!(
        order.state,
        OrderState::SwapReady {
            step: ExecStep::Swap,
            ..
        }
    ));
    // The plan must survive: the orchestrator rebuilds transactions from it.
    assert!(matches!(order.plan, ExecutionPlan::Uniswap { .. }));

    // Something was created, so whoever drives orders has to be told.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), waker.notified())
            .await
            .is_ok()
    );
    fixture.cleanup();
}

#[tokio::test]
async fn sell_quotes_base_to_quote_and_snapshots_the_same_direction() {
    let (fixture, deps) = Fixture::new("sell-direction", true).await;
    let mut entry = level();
    entry.level.side = Side::Sell;
    entry.level.amount = U256::from(1_000_000_000_000_000_000u64);
    entry.level.amount_decimals = 18;
    fixture.store(&entry).await;

    drive(deps, Arc::new(Notify::new()), vec![tick(3_001.0, 100)]).await;

    let order = fixture.orders.list_orders().await.unwrap().remove(0);
    assert_eq!(order.token_in, "WETH");
    assert_eq!(order.token_out, "USDC");
    assert_eq!(
        fixture.request.lock().unwrap().as_ref().unwrap(),
        &("WETH".into(), "USDC".into(), "1".into())
    );
    fixture.cleanup();
}

#[tokio::test]
async fn a_rejected_preflight_creates_nothing_and_the_loop_survives() {
    let (fixture, deps) = Fixture::new("rejected", false).await;
    fixture.store(&level()).await;

    drive(
        deps,
        Arc::new(Notify::new()),
        vec![tick(2_999.0, 100), tick(2_998.0, 101)],
    )
    .await;

    assert!(fixture.orders.list_orders().await.unwrap().is_empty());
    // The second tick was still handled: the loop did not die on the first.
    assert!(fixture.calls.load(Ordering::SeqCst) >= 1);
    fixture.cleanup();
}

// Rejected pre-flights must not repeat on every tick.
#[tokio::test]
async fn a_rejected_level_goes_quiet_instead_of_requoting() {
    let (fixture, deps) = Fixture::new("quiet", false).await;
    fixture.store(&level()).await;

    drive(
        deps,
        Arc::new(Notify::new()),
        vec![
            tick(2_999.0, 100),
            tick(2_998.0, 101),
            tick(2_997.0, 102),
            tick(2_996.0, 103),
        ],
    )
    .await;

    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "only the first tick should reach the venue"
    );
    assert!(
        fixture.runtime.snapshot().quiet_until.contains_key("l-1"),
        "the trigger and dashboard snapshot must share quiet_until"
    );
    fixture.cleanup();
}

// Replayed ticks upsert the same deterministic order.
#[tokio::test]
async fn the_same_tick_twice_yields_one_order() {
    let (fixture, deps) = Fixture::new("idempotent", true).await;
    fixture.store(&level()).await;

    drive(
        deps,
        Arc::new(Notify::new()),
        vec![tick(2_999.0, 100), tick(2_999.0, 100)],
    )
    .await;

    assert_eq!(fixture.orders.list_orders().await.unwrap().len(), 1);
    fixture.cleanup();
}

#[tokio::test]
async fn a_level_that_already_acted_never_reaches_the_venue() {
    let (fixture, deps) = Fixture::new("spent", true).await;
    fixture.store(&level()).await;

    // First tick spends the level, second finds it taken.
    drive(
        deps,
        Arc::new(Notify::new()),
        vec![tick(2_999.0, 100), tick(2_998.0, 101)],
    )
    .await;

    assert_eq!(fixture.orders.list_orders().await.unwrap().len(), 1);
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "the spent level must be filtered before the venue is asked"
    );
    fixture.cleanup();
}

#[tokio::test]
async fn a_tick_that_fires_nothing_does_not_wake_anyone() {
    let (fixture, deps) = Fixture::new("no-wake", true).await;
    fixture.store(&level()).await;
    let waker = Arc::new(Notify::new());

    // Above the buy threshold: nothing fires.
    drive(deps, waker.clone(), vec![tick(3_500.0, 100)]).await;

    assert!(fixture.orders.list_orders().await.unwrap().is_empty());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), waker.notified())
            .await
            .is_err(),
        "nothing was created, so nothing should be woken"
    );
    fixture.cleanup();
}

// Failed orders re-arm only after the cooldown.
#[tokio::test]
async fn a_level_that_just_failed_is_left_to_rest() {
    let (fixture, deps) = Fixture::new("cooldown", true).await;
    fixture.store(&level()).await;

    let mut failed = Order::new("o-old".into(), &level(), plan(), now_secs());
    failed.state = OrderState::Failed {
        tx_hash: None,
        reason: "reverted on-chain".into(),
    };
    fixture.orders.upsert_order(&failed).await.unwrap();

    drive(deps, Arc::new(Notify::new()), vec![tick(2_999.0, 100)]).await;

    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        0,
        "a resting level must not even be quoted"
    );
    assert_eq!(fixture.orders.list_orders().await.unwrap().len(), 1);
    fixture.cleanup();
}

fn plan() -> ExecutionPlan {
    ExecutionPlan::Uniswap {
        chain_name: "base".into(),
        chain_id: BASE_ID,
        input_token: USDC.into(),
        input_amount: "1000000".into(),
        quote: json!({"tradeType": "EXACT_INPUT"}),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
