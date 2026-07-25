use std::path::Path;

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Sends events to a daily file and to stderr.
///
/// The returned guard flushes the file buffer when dropped, so it has to be held
/// for as long as the process runs or the tail of the log is lost.
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
