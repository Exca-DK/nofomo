pub mod admin;
pub mod lock;
pub mod logging;
pub mod operate;
pub mod prices;
pub mod provision;
pub mod wiring;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tempo_agentic_config::Config;
use tempo_agentic_mcp::AdminHandler;
use tempo_agentic_orchestrator::Waker;
use tempo_agentic_price::{
    DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_MOVE_BPS, FilteredSource, PriceSource,
};
use tempo_agentic_price_dexpaprika::DexPaprikaSource;
use tempo_agentic_storage::{SqliteLevelStore, SqliteOrderStore, connect_pool};
use tempo_agentic_strategy::OrderStore;
use tempo_agentic_trigger::TokenResolver;
use tokio::sync::mpsc;

use crate::admin::AdminServer;
use crate::lock::LockFile;

/// How long the execution loop sleeps when nothing wakes it. Orders waiting on a
/// receipt need re-checking even when no new one arrives.
const ORCHESTRATOR_POLL: Duration = Duration::from_secs(5);

/// Ticks that may queue before the feed is held back. Bounded on purpose: a
/// pre-flight is slower than the feed, and stale quotes are worth less than
/// steady memory.
const TICK_CHANNEL: usize = 256;

pub struct Options {
    pub config: String,
    /// Where the rolling log goes. `None` puts it beside the database.
    pub log_dir: Option<PathBuf>,
    pub allow_broadcast: bool,
}

/// Runs the daemon until a shutdown signal arrives.
///
/// Returns an error if the configuration, the log directory, the lock, or the
/// database cannot be opened.
pub async fn run(options: Options) -> Result<()> {
    let config = Config::load(&options.config)?;
    let database = PathBuf::from(&config.state_db_path);
    let log_dir = options
        .log_dir
        .clone()
        .unwrap_or_else(|| database.parent().unwrap_or(Path::new(".")).to_path_buf());

    // Installed before the lock so a refusal to start is recorded rather than
    // only printed.
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

    let wiring = wiring::build(
        &config,
        options.allow_broadcast,
        levels.clone(),
        orders.clone(),
    )?;
    let source: Arc<dyn PriceSource> = Arc::new(FilteredSource::new(
        DexPaprikaSource::new(config.dexpaprika_stream_url.clone()),
        DEFAULT_MAX_AGE_SECS,
        DEFAULT_MAX_MOVE_BPS,
    ));

    let admin = AdminServer::start(
        AdminHandler::new(
            levels.clone(),
            orders.clone(),
            config.evm.clone(),
            config.max_slippage_bps,
            options.allow_broadcast,
            source.clone(),
        ),
        &database,
    )
    .await?;
    tracing::info!(
        url = %admin.url,
        manifest = %admin::manifest_path(&database).display(),
        "admin MCP surface listening; the manifest holds the bearer token"
    );

    let (ticks_tx, ticks_rx) = mpsc::channel(TICK_CHANNEL);
    let waker = Arc::new(Waker::default());
    // The trigger only ever wakes the loop; waiting on it belongs to the loop
    // that consumes the work.
    let notifier = waker.notifier();

    let orchestrator = tokio::spawn(tempo_agentic_orchestrator::run(
        Arc::new(wiring.exec),
        orders as Arc<dyn OrderStore>,
        waker,
        ORCHESTRATOR_POLL,
    ));
    let producer = tokio::spawn(prices::produce(
        levels,
        TokenResolver::from_config(&config.evm),
        source,
        ticks_tx,
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

/// Resolves on Ctrl-C, or on `SIGTERM` where there is one.
///
/// `SIGTERM` matters because that is what a service manager or `docker stop`
/// sends; without it the process would only ever be killed outright, and the
/// lock file would survive.
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
