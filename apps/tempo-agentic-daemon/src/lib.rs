pub mod deps;
pub mod keystore;
pub mod logging;
pub mod wiring;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tempo_agentic_config::Config;
use tempo_agentic_mcp::{AdminHandler, AdminServer, manifest_path};
use tempo_agentic_orchestrator::Waker;
use tempo_agentic_storage::{LockFile, SqliteLevelStore, SqliteOrderStore, connect_pool};
use tempo_agentic_strategy::OrderStore;
use tokio::sync::mpsc;

/// Maximum execution-loop sleep between receipt checks.
const ORCHESTRATOR_POLL: Duration = Duration::from_secs(5);

/// Tick queue limit before feed backpressure.
const TICK_CHANNEL: usize = 256;

pub struct Options {
    pub config: String,
    /// Where the rolling log goes. `None` puts it beside the database.
    pub log_dir: Option<PathBuf>,
    pub allow_broadcast: bool,
}

/// Starts the daemon and runs until shutdown.
pub async fn run(options: Options) -> Result<()> {
    let config = Config::load(&options.config)?;
    let database = PathBuf::from(&config.state_db_path);
    let log_dir = options
        .log_dir
        .clone()
        .unwrap_or_else(|| database.parent().unwrap_or(Path::new(".")).to_path_buf());

    // Install logging early enough to record lock failures.
    let _logs = logging::install(&log_dir)?;
    let _lock = LockFile::acquire(LockFile::path_for(&database))?;

    tracing::info!(
        config = %options.config,
        database = %database.display(),
        logs = %log_dir.display(),
        "starting the tempo-agentic daemon"
    );
    if options.allow_broadcast {
        tracing::warn!("broadcasting is on: this process will spend real funds");
    } else {
        tracing::info!(
            "broadcasting is off: orders are quoted, built and signed but never sent; \
             set MAINNET_SWAP=1 to trade for real"
        );
    }

    let pool = connect_pool(&database).await?;
    let levels = Arc::new(SqliteLevelStore::new(pool.clone()));
    let orders = Arc::new(SqliteOrderStore::new(pool));

    let tokens = deps::tokens(&config);
    let source = deps::prices(&config);
    let wiring = wiring::build(
        &config,
        options.allow_broadcast,
        levels.clone(),
        orders.clone(),
        tokens.clone(),
    )?;

    let admin = AdminServer::start(
        AdminHandler::new(
            levels.clone(),
            orders.clone(),
            tokens.clone(),
            config.max_slippage_bps,
            options.allow_broadcast,
            source.clone(),
        ),
        &database,
    )
    .await?;
    tracing::info!(
        url = %admin.url,
        manifest = %manifest_path(&database).display(),
        "admin MCP surface listening; the manifest holds the bearer token"
    );

    let (ticks_tx, ticks_rx) = mpsc::channel(TICK_CHANNEL);
    let waker = Arc::new(Waker::default());
    // Only the order loop may wait on its waker.
    let notifier = waker.notifier();

    let orchestrator = tokio::spawn(tempo_agentic_orchestrator::run(
        Arc::new(wiring.exec),
        orders as Arc<dyn OrderStore>,
        waker,
        ORCHESTRATOR_POLL,
    ));
    let producer = tokio::spawn(tempo_agentic_trigger::produce(
        levels, tokens, source, ticks_tx,
    ));

    tokio::select! {
        _ = tempo_agentic_trigger::run(wiring.trigger, ticks_rx, notifier) => {
            tracing::warn!("the trigger loop ended: no prices are arriving any more");
        }
        _ = shutdown_signal() => tracing::info!("shutdown signal received"),
    }

    orchestrator.abort();
    producer.abort();
    tracing::info!("daemon stopped");
    Ok(())
}

/// Waits for Ctrl-C or `SIGTERM`.
pub async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = interrupt => {}
        _ = terminate => {}
    }
}
