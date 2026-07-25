use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tempo_agentic_daemon::{Options, run};

#[derive(Parser)]
#[command(name = "tempo-agentic-daemon", version)]
struct Cli {
    #[arg(long, env = "TEMPO_AGENTIC_CONFIG", default_value = "config.json")]
    config: String,
    /// Where the rolling log file goes. Defaults to the database's directory.
    #[arg(long, env = "TEMPO_AGENTIC_LOG_DIR")]
    log_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    run(Options {
        config: cli.config,
        log_dir: cli.log_dir,
        // Spending real funds takes an explicit word, and only that one word.
        allow_broadcast: std::env::var("MAINNET_SWAP").as_deref() == Ok("1"),
    })
    .await
}
