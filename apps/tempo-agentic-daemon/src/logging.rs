use std::path::Path;

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Logs to stderr and a daily file; keep the returned flush guard alive.
pub fn install(dir: &Path) -> Result<WorkerGuard> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("cannot create log directory {}", dir.display()))?;
    let (writer, guard) = tracing_appender::non_blocking(tracing_appender::rolling::daily(
        dir,
        "tempo-agentic-daemon.log",
    ));
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        // Colour codes belong on a terminal, not in a file that gets grepped.
        .with(fmt::layer().with_writer(writer).with_ansi(false))
        .with(fmt::layer().with_writer(std::io::stderr))
        .init();
    Ok(guard)
}
