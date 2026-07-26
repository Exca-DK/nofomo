use alloy_primitives::U256;
use serde_json::json;
use std::path::PathBuf;
use tempo_agentic_domain::{ExecutionPlan, VenueName};
use tempo_agentic_orchestrator::resolve_quarantine;
use tempo_agentic_storage::{SqliteLevelStore, SqliteOrderStore, connect_pool};
use tempo_agentic_strategy::{Level, LevelStore, Order, OrderState, OrderStore, Side};
use tempo_agentic_trigger::is_spent;

struct Fixture {
    levels: SqliteLevelStore,
    orders: SqliteOrderStore,
    database: PathBuf,
}

impl Fixture {
    // Test names stay unique under parallel execution.
    async fn open(name: &str) -> Self {
        let database = std::env::temp_dir().join(format!(
            "tempo-agentic-orchestrator-quarantine-{}-{name}.db",
            std::process::id(),
        ));
        let pool = connect_pool(&database).await.unwrap();
        Self {
            levels: SqliteLevelStore::new(pool.clone()),
            orders: SqliteOrderStore::new(pool),
            database,
        }
    }

    // Satisfy the order's `level_id` foreign key.
    async fn put(&self, id: &str, state: OrderState) {
        let level = Level {
            id: "l-1".into(),
            venue: VenueName::Uniswap,
            chain: "base".into(),
            token_in: "USDC".into(),
            token_out: "WETH".into(),
            side: Side::Buy,
            trigger_price_usd: 3_000.0,
            amount: U256::from(1_000_000u64),
            amount_decimals: 6,
            slippage_bps: 50,
        };
        self.levels.upsert_level(&level).await.unwrap();
        let plan = ExecutionPlan::Uniswap {
            chain_name: "base".into(),
            chain_id: 8453,
            input_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            input_amount: "1000000".into(),
            quote: json!({"tradeType": "EXACT_INPUT"}),
        };
        let mut order = Order::new(id.into(), &level, plan, 1);
        order.state = state;
        order.swap_attempts = 8;
        order.swap_retry_after_ts = Some(1_700_000_000);
        self.orders.upsert_order(&order).await.unwrap();
    }

    fn cleanup(self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.database.display()));
        }
    }
}

fn quarantined() -> OrderState {
    OrderState::SwapQuarantined {
        amount_in: U256::from(1_000_000u64),
        tx_hash: Some("0xdeadbeef".into()),
        reason: "exhausted broadcast retries".into(),
    }
}

// Release must land on the only status that re-arms a level.
#[tokio::test]
async fn releasing_a_quarantine_frees_the_level_it_blocked() {
    let fixture = Fixture::open("frees-level").await;
    fixture.put("o-1", quarantined()).await;
    assert!(
        is_spent("l-1", &fixture.orders.list_orders().await.unwrap()),
        "a quarantined order has to hold its level down to begin with"
    );

    let level_id = resolve_quarantine(&fixture.orders, "o-1").await.unwrap();

    assert_eq!(level_id, "l-1");
    let stored = fixture.orders.list_orders().await.unwrap();
    assert!(matches!(stored[0].state, OrderState::Failed { .. }));
    assert_eq!(stored[0].swap_attempts, 0);
    assert_eq!(stored[0].swap_retry_after_ts, None);
    assert!(!is_spent("l-1", &stored), "the level has to be free again");

    fixture.cleanup();
}

// Refuse to rewind an active order.
#[tokio::test]
async fn an_order_that_is_not_quarantined_is_refused() {
    let fixture = Fixture::open("not-quarantined").await;
    fixture
        .put(
            "o-1",
            OrderState::Submitted {
                step: tempo_agentic_domain::ExecStep::Swap,
                amount_in: U256::from(1_000_000u64),
                tx_hash: "0xdeadbeef".into(),
                withdraw_action_id: None,
                submitted_at: 1_700_000_000,
            },
        )
        .await;

    let error = resolve_quarantine(&fixture.orders, "o-1")
        .await
        .expect_err("a submitted order is not a quarantine to release");
    assert!(error.to_string().contains("not quarantined"), "{error}");

    let unchanged = fixture.orders.list_orders().await.unwrap();
    assert!(matches!(unchanged[0].state, OrderState::Submitted { .. }));

    fixture.cleanup();
}

#[tokio::test]
async fn an_unknown_order_is_reported_rather_than_ignored() {
    let fixture = Fixture::open("unknown-order").await;

    let error = resolve_quarantine(&fixture.orders, "o-missing")
        .await
        .expect_err("there is no such order");
    assert!(error.to_string().contains("o-missing"), "{error}");

    fixture.cleanup();
}
