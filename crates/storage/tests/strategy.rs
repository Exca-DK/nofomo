use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use alloy_primitives::U256;
use sqlx::SqlitePool;
use tempo_agentic_domain::VenueName;
use tempo_agentic_storage::{SqliteLevelStore, SqliteOrderStore, connect_pool};
use tempo_agentic_strategy::{Level, LevelStore, Order, OrderState, OrderStatus, OrderStore, Side};

struct Fixture {
    levels: SqliteLevelStore,
    orders: SqliteOrderStore,
    pool: SqlitePool,
    path: PathBuf,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "tempo-agentic-strategy-{name}-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = connect_pool(&path).await.unwrap();
        Self {
            levels: SqliteLevelStore::new(pool.clone()),
            orders: SqliteOrderStore::new(pool.clone()),
            pool,
            path,
        }
    }

    fn cleanup(self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

fn level() -> Level {
    Level {
        id: "l-1".into(),
        venue: VenueName::Uniswap,
        chain: "base".into(),
        token_in: "USDC".into(),
        token_out: "WETH".into(),
        side: Side::Buy,
        trigger_price_usd: 3_000.5,
        amount: U256::from(1_000_000u64),
        amount_decimals: 6,
        slippage_bps: 50,
    }
}

#[tokio::test]
async fn level_round_trips_including_a_max_u256_amount() {
    let fixture = Fixture::new("level-roundtrip").await;
    let mut stored = level();
    stored.amount = U256::MAX;
    stored.amount_decimals = 18;
    fixture.levels.upsert_level(&stored).await.unwrap();

    assert_eq!(
        fixture.levels.get_level("l-1").await.unwrap(),
        Some(stored.clone())
    );
    assert_eq!(
        fixture.levels.list_levels().await.unwrap(),
        vec![stored.clone()]
    );

    // Upsert is keyed on id: a second write updates rather than duplicating.
    let mut edited = stored.clone();
    edited.side = Side::Sell;
    edited.trigger_price_usd = 4_200.0;
    fixture.levels.upsert_level(&edited).await.unwrap();
    assert_eq!(fixture.levels.list_levels().await.unwrap(), vec![edited]);

    fixture.cleanup();
}

#[tokio::test]
async fn deleting_a_missing_level_succeeds() {
    let fixture = Fixture::new("delete-missing").await;
    fixture.levels.delete_level("nope").await.unwrap();
    assert_eq!(fixture.levels.get_level("nope").await.unwrap(), None);
    fixture.cleanup();
}

#[tokio::test]
async fn every_order_state_round_trips_with_exact_amounts() {
    let fixture = Fixture::new("state-roundtrip").await;
    fixture.levels.upsert_level(&level()).await.unwrap();

    // A value whose decimal and hex readings differ wildly, so an encoding
    // mix-up between the scalar column and the state JSON cannot pass.
    let amount_in = U256::from(1_000u64);
    let states = [
        OrderState::Withdrawing {
            amount_in,
            action_id: "act-1".into(),
        },
        OrderState::SwapReady {
            amount_in,
            withdraw_action_id: None,
        },
        OrderState::SwapReady {
            amount_in,
            withdraw_action_id: Some("act-1".into()),
        },
        OrderState::Broadcasting {
            amount_in,
            signed_tx: "0x02f8".into(),
            tx_hash: "0xhash".into(),
            withdraw_action_id: Some("act-1".into()),
        },
        OrderState::Submitted {
            amount_in,
            tx_hash: "0xhash".into(),
            withdraw_action_id: None,
        },
        OrderState::Depositing {
            tx_hash: "0xhash".into(),
            amount: amount_in,
            action_id: "act-2".into(),
        },
        OrderState::Filled {
            tx_hash: "0xhash".into(),
        },
        OrderState::Failed {
            tx_hash: Some("0xhash".into()),
            reason: "reverted".into(),
        },
        OrderState::SwapQuarantined {
            amount_in,
            withdraw_action_id: "act-1".into(),
            reason: "retries exhausted".into(),
        },
    ];

    for (index, state) in states.into_iter().enumerate() {
        let mut order = Order::new(format!("o-{index}"), &level(), 100 + index as i64);
        order.state = state;
        order.swap_attempts = 3;
        order.swap_retry_after_ts = Some(1_700_000_000);
        fixture.orders.upsert_order(&order).await.unwrap();

        let loaded = fixture.orders.get_order(&order.id).await.unwrap();
        assert_eq!(
            loaded.as_ref(),
            Some(&order),
            "state {index} did not survive"
        );
    }

    assert_eq!(fixture.orders.list_orders().await.unwrap().len(), 9);
    fixture.cleanup();
}

// Locks the encoding split the schema comment warns about: the scalar column
// is decimal, but U256 nested in the state JSON is 0x-prefixed hex. Reading
// one as the other would silently mis-size an order.
#[tokio::test]
async fn scalar_amounts_are_decimal_and_state_amounts_are_hex() {
    let fixture = Fixture::new("encoding").await;
    fixture.levels.upsert_level(&level()).await.unwrap();
    let order = Order::new("o-1".into(), &level(), 1);
    fixture.orders.upsert_order(&order).await.unwrap();

    let row = sqlx::query!("SELECT reserved_amount, state FROM orders WHERE id = 'o-1'")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();

    assert_eq!(row.reserved_amount, "1000000");
    assert!(
        row.state.contains("0xf4240"),
        "expected hex-encoded amount in state JSON, got {}",
        row.state
    );
    assert_eq!(
        fixture
            .orders
            .get_order("o-1")
            .await
            .unwrap()
            .unwrap()
            .reserved_amount,
        U256::from(1_000_000u64)
    );
    fixture.cleanup();
}

#[tokio::test]
async fn denormalized_columns_track_the_state() {
    let fixture = Fixture::new("denormalized").await;
    fixture.levels.upsert_level(&level()).await.unwrap();
    let mut order = Order::new("o-1".into(), &level(), 1);
    fixture.orders.upsert_order(&order).await.unwrap();

    order.state = OrderState::Filled {
        tx_hash: "0xhash".into(),
    };
    fixture.orders.upsert_order(&order).await.unwrap();

    let row = sqlx::query!("SELECT status, tx_hash FROM orders WHERE id = 'o-1'")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(row.status, OrderStatus::Filled.as_str());
    assert_eq!(row.tx_hash.as_deref(), Some("0xhash"));
    fixture.cleanup();
}

#[tokio::test]
async fn an_unknown_venue_on_disk_is_an_error_not_a_default() {
    let fixture = Fixture::new("bad-venue").await;
    fixture.levels.upsert_level(&level()).await.unwrap();
    sqlx::query!("UPDATE levels SET venue = 'bogus' WHERE id = 'l-1'")
        .execute(&fixture.pool)
        .await
        .unwrap();

    assert!(fixture.levels.get_level("l-1").await.is_err());
    fixture.cleanup();
}

#[tokio::test]
async fn an_order_cannot_reference_a_missing_level() {
    let fixture = Fixture::new("fk").await;
    let order = Order::new("o-1".into(), &level(), 1);
    assert!(fixture.orders.upsert_order(&order).await.is_err());
    fixture.cleanup();
}
