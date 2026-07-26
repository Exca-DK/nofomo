use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use alloy_primitives::U256;
use serde_json::json;
use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, SuiConfig};
use tempo_agentic_domain::{ExecStep, ExecutionPlan, VenueName};
use tempo_agentic_mcp::{AdminHandler, AdminServer, DashboardDeps, manifest_path};
use tempo_agentic_price::PricePair;
use tempo_agentic_price_dexpaprika::DexPaprikaSource;
use tempo_agentic_storage::{
    LockFile, SqliteLevelStore, SqliteOrderStore, SqliteStrategyStore, initialize_new_under_lock,
};
use tempo_agentic_strategy::{
    Level, LevelStore, Order, OrderState, OrderStore, Side, Strategy, StrategyStore,
};
use tempo_agentic_trigger::{RuntimeStatus, TokenResolver};

struct Fixture {
    server: AdminServer,
    token: String,
    database: PathBuf,
    strategies: Arc<SqliteStrategyStore>,
    levels: Arc<SqliteLevelStore>,
    orders: Arc<SqliteOrderStore>,
    runtime: Arc<RuntimeStatus>,
}

impl Fixture {
    // Test names stay unique under parallel execution.
    async fn start(name: &str) -> Self {
        let database = std::env::temp_dir().join(format!(
            "tempo-agentic-mcp-server-{}-{name}.db",
            std::process::id(),
        ));
        let lock = LockFile::acquire(LockFile::path_for(&database)).unwrap();
        let pool = initialize_new_under_lock(&database, &lock).await.unwrap();
        let strategies = Arc::new(SqliteStrategyStore::new(pool.clone()));
        let levels = Arc::new(SqliteLevelStore::new(pool.clone()));
        let orders = Arc::new(SqliteOrderStore::new(pool));
        let runtime = Arc::new(RuntimeStatus::new(false, 30));
        let handler = AdminHandler::new(
            strategies.clone(),
            levels.clone(),
            orders.clone(),
            DashboardDeps {
                store: strategies.clone(),
                runtime: runtime.clone(),
                market: None,
            },
            Arc::new(TokenResolver::from_config(
                &EvmConfig {
                    chains: vec![EvmChain {
                        name: "base".into(),
                        chain_id: 8453,
                        rpc_url: "http://localhost".into(),
                        graph_subgraph_id: String::new(),
                        tokens: HashMap::from([
                            (
                                "WETH".into(),
                                EvmToken {
                                    address: "0xbase".into(),
                                    decimals: 18,
                                    usd_peg: false,
                                },
                            ),
                            (
                                "USDC".into(),
                                EvmToken {
                                    address: "0xquote".into(),
                                    decimals: 6,
                                    usd_peg: false,
                                },
                            ),
                        ]),
                    }],
                },
                &SuiConfig::default(),
            )),
            500,
            Arc::new(DexPaprikaSource::new("https://example.invalid")),
        );
        let server = AdminServer::start(handler, &database).await.unwrap();

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(manifest_path(&database)).unwrap()).unwrap();
        let token = manifest["token"].as_str().unwrap().to_string();
        assert!(!token.is_empty(), "the manifest has to carry a token");

        Self {
            server,
            token,
            database,
            strategies,
            levels,
            orders,
            runtime,
        }
    }

    async fn post(&self, token: Option<&str>) -> reqwest::StatusCode {
        let mut request = reqwest::Client::new()
            .post(&self.server.url)
            .header("accept", "application/json, text/event-stream")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap().status()
    }

    async fn dashboard(&self, token: Option<&str>) -> reqwest::Response {
        let mut request = reqwest::Client::new().get(format!("{}dashboard", self.server.url));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap()
    }

    async fn market(&self, token: Option<&str>, body: serde_json::Value) -> reqwest::Response {
        let mut request = reqwest::Client::new()
            .post(format!("{}dashboard/market", self.server.url))
            .json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap()
    }

    fn cleanup(self) {
        let database = self.database.clone();
        drop(self.server);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", database.display()));
        }
    }
}

// Loopback still requires authentication between local processes.
#[tokio::test]
async fn a_request_without_the_token_is_refused() {
    let fixture = Fixture::start("token").await;

    assert_eq!(
        fixture.post(None).await,
        reqwest::StatusCode::UNAUTHORIZED,
        "an unauthenticated caller must not reach the tools at all"
    );
    assert_eq!(
        fixture.post(Some("not-the-token")).await,
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture.dashboard(None).await.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture.dashboard(Some("not-the-token")).await.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture
            .market(None, json!({"strategy_id": "s-1"}))
            .await
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let allowed = fixture.post(Some(&fixture.token.clone())).await;
    assert_eq!(
        allowed,
        reqwest::StatusCode::OK,
        "the authenticated MCP POST must keep its existing route"
    );

    fixture.cleanup();
}

#[tokio::test]
async fn market_endpoint_validates_strategy_and_graph_support() {
    let fixture = Fixture::start("market-errors").await;

    let malformed = fixture.market(Some(&fixture.token), json!({})).await;
    assert_eq!(malformed.status(), reqwest::StatusCode::BAD_REQUEST);

    let missing = fixture
        .market(Some(&fixture.token), json!({"strategy_id": "missing"}))
        .await;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    fixture
        .strategies
        .upsert_strategy(&strategy())
        .await
        .unwrap();
    let unsupported = fixture
        .market(Some(&fixture.token), json!({"strategy_id": "s-1"}))
        .await;
    assert_eq!(
        unsupported.status(),
        reqwest::StatusCode::UNPROCESSABLE_ENTITY
    );
    assert!(
        unsupported.json::<serde_json::Value>().await.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("not configured")
    );

    fixture.cleanup();
}

#[tokio::test]
async fn dashboard_includes_empty_strategies_and_daemon_feed_health() {
    let fixture = Fixture::start("dashboard-empty-strategy").await;
    let strategy = strategy();
    fixture.strategies.upsert_strategy(&strategy).await.unwrap();
    let pair = PricePair::new(8453, "0xbase");
    fixture.runtime.feed_connecting(pair, 1);

    let response = fixture.dashboard(Some(&fixture.token)).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.headers()[reqwest::header::CONTENT_TYPE],
        "application/json"
    );
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["allow_broadcast"], false);
    assert!(body["started_at"].is_i64());
    assert!(body["generated_at"].is_i64());
    assert_eq!(body["strategies"][0]["id"], strategy.id);
    assert_eq!(body["levels"], json!([]));
    assert_eq!(body["feeds"][0]["health"], "connecting");

    fixture.cleanup();
}

#[tokio::test]
async fn dashboard_uses_trade_direction_and_exposes_only_safe_order_fields() {
    let fixture = Fixture::start("dashboard-safe-json").await;
    let strategy = strategy();
    let level = level();
    fixture.strategies.upsert_strategy(&strategy).await.unwrap();
    fixture
        .levels
        .upsert_level(&level, &strategy)
        .await
        .unwrap();
    let order = Order {
        id: "o-1".into(),
        level_id: level.id.clone(),
        venue: VenueName::Uniswap,
        chain: "base".into(),
        token_in: "USDC".into(),
        token_out: "WETH".into(),
        reserved_amount: level.amount,
        plan: ExecutionPlan::Uniswap {
            chain_name: "base".into(),
            chain_id: 8453,
            input_token: "USDC".into(),
            input_amount: "1000000".into(),
            quote: json!({"raw_plan_secret": "DO_NOT_SERIALIZE"}),
        },
        state: OrderState::Broadcasting {
            step: ExecStep::Swap,
            amount_in: level.amount,
            signed_tx: "SIGNED_TX_SECRET".into(),
            tx_hash: "0xsafehash".into(),
            withdraw_action_id: None,
        },
        swap_attempts: 1,
        swap_retry_after_ts: None,
        created_at: 1,
    };
    fixture.orders.upsert_order(&order).await.unwrap();

    let response = fixture.dashboard(Some(&fixture.token)).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.text().await.unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["levels"][0]["token_in"], "USDC");
    assert_eq!(json["levels"][0]["token_out"], "WETH");
    assert_eq!(json["levels"][0]["price_pair"]["chain_id"], 8453);
    assert_eq!(json["levels"][0]["price_pair"]["token_address"], "0xbase");
    assert_eq!(json["levels"][0]["runtime_state"], "executing");
    assert_eq!(json["orders"][0]["tx_hash"], "0xsafehash");
    for forbidden in [
        &fixture.token,
        "SIGNED_TX_SECRET",
        "DO_NOT_SERIALIZE",
        "raw_plan_secret",
        "keystore_path",
        "password_file",
        "signed_tx",
        "plan",
    ] {
        assert!(!body.contains(forbidden), "dashboard leaked {forbidden}");
    }

    fixture.cleanup();
}

#[tokio::test]
async fn runtime_changes_after_a_poll_are_visible_on_the_next_poll() {
    let fixture = Fixture::start("dashboard-eventual-runtime").await;
    let strategy = strategy();
    let level = level();
    fixture.strategies.upsert_strategy(&strategy).await.unwrap();
    fixture
        .levels
        .upsert_level(&level, &strategy)
        .await
        .unwrap();

    let first: serde_json::Value = fixture
        .dashboard(Some(&fixture.token))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(first["levels"][0]["runtime_state"], "armed");

    fixture.runtime.set_quiet_until(level.id.clone(), i64::MAX);
    let second: serde_json::Value = fixture
        .dashboard(Some(&fixture.token))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(second["levels"][0]["runtime_state"], "cooldown");

    fixture.cleanup();
}

// Dropping the server must remove its stale manifest.
#[tokio::test]
async fn stopping_the_server_takes_the_manifest_with_it() {
    let fixture = Fixture::start("manifest").await;
    let manifest = manifest_path(&fixture.database);
    assert!(manifest.exists());

    fixture.cleanup();

    assert!(!manifest.exists());
}

fn strategy() -> Strategy {
    Strategy {
        id: "s-1".into(),
        venue: VenueName::Uniswap,
        chain: "base".into(),
        base_token: "WETH".into(),
        quote_token: "USDC".into(),
    }
}

fn level() -> Level {
    Level {
        id: "l-1".into(),
        strategy_id: "s-1".into(),
        side: Side::Buy,
        trigger_price_usd: 3_000.0,
        amount: U256::from(1_000_000u64),
        amount_decimals: 6,
        slippage_bps: 50,
    }
}
