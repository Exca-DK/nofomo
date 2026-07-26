use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tempo_agentic_config::EvmConfig;
use tempo_agentic_daemon::admin::{AdminServer, manifest_path};
use tempo_agentic_mcp::AdminHandler;
use tempo_agentic_price_dexpaprika::DexPaprikaSource;
use tempo_agentic_storage::{SqliteLevelStore, SqliteOrderStore, connect_pool};

struct Fixture {
    server: AdminServer,
    token: String,
    database: PathBuf,
}

impl Fixture {
    async fn start() -> Self {
        let database = std::env::temp_dir().join(format!(
            "tempo-agentic-daemon-admin-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = connect_pool(&database).await.unwrap();
        let handler = AdminHandler::new(
            Arc::new(SqliteLevelStore::new(pool.clone())),
            Arc::new(SqliteOrderStore::new(pool)),
            EvmConfig {
                keystore_path: "/dev/null".into(),
                password_file: "/dev/null".into(),
                chains: Vec::new(),
            },
            500,
            false,
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

    fn cleanup(self) {
        let database = self.database.clone();
        drop(self.server);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", database.display()));
        }
    }
}

// Loopback keeps other machines out; it does nothing about other processes on
// this one, and `set_level` stores rules that spend funds.
#[tokio::test]
async fn a_request_without_the_token_is_refused() {
    let fixture = Fixture::start().await;

    assert_eq!(
        fixture.post(None).await,
        reqwest::StatusCode::UNAUTHORIZED,
        "an unauthenticated caller must not reach the tools at all"
    );
    assert_eq!(
        fixture.post(Some("not-the-token")).await,
        reqwest::StatusCode::UNAUTHORIZED
    );

    let allowed = fixture.post(Some(&fixture.token.clone())).await;
    assert_ne!(
        allowed,
        reqwest::StatusCode::UNAUTHORIZED,
        "the token from the manifest has to get through"
    );

    fixture.cleanup();
}

// A manifest left behind would point at a port nobody is listening on, and hand
// out a token that no longer opens anything.
#[tokio::test]
async fn stopping_the_server_takes_the_manifest_with_it() {
    let fixture = Fixture::start().await;
    let manifest = manifest_path(&fixture.database);
    assert!(manifest.exists());

    fixture.cleanup();

    assert!(!manifest.exists());
}
