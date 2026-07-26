use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use tempo_agentic_config::Config;
use tempo_agentic_daemon::{Options, provision, run};
use tempo_agentic_orchestrator::resolve_quarantine;
use tempo_agentic_price_dexpaprika::DexPaprikaSource;
use tempo_agentic_storage::{SqliteLevelStore, SqliteOrderStore, connect_pool};
use tempo_agentic_strategy::LevelStore;
use tempo_agentic_trigger::{LevelDraft, validate_level};

#[derive(Parser)]
#[command(name = "tempo-agentic-daemon", version)]
struct Cli {
    #[arg(long, env = "TEMPO_AGENTIC_CONFIG", default_value = "config.json")]
    config: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Watch prices and act on the stored rules until a shutdown signal arrives.
    Run {
        /// Where the rolling log file goes. Defaults to the database's directory.
        #[arg(long, env = "TEMPO_AGENTIC_LOG_DIR")]
        log_dir: Option<PathBuf>,
    },
    /// Create throwaway local dev accounts if missing without modifying git.
    Bootstrap,
    /// Import an existing account by private key instead of generating a throwaway one.
    ImportKey {
        #[arg(long, value_enum)]
        chain: ImportChain,
        /// Raw private key (EVM: 0x-prefixed hex; Sui: base64 keypair string).
        #[arg(long)]
        private_key: String,
        /// Overwrite an existing keystore/keypair for this chain.
        #[arg(long)]
        force: bool,
    },
    /// Validate config, secrets, and SQLite without printing secrets.
    Health,
    /// Manage the standing rules the daemon watches prices against.
    Level {
        #[command(subcommand)]
        action: LevelCommand,
    },
    /// Release a quarantined order so its level can fire again.
    ResolveQuarantine {
        #[arg(long)]
        order_id: String,
    },
}

/// Which keystore an imported private key belongs in.
#[derive(Clone, Copy, clap::ValueEnum)]
enum ImportChain {
    Evm,
    Sui,
}

#[derive(Subcommand)]
enum LevelCommand {
    /// Store a rule after checking it against the configuration.
    Add(AddLevel),
    List,
    Rm {
        #[arg(long)]
        id: String,
    },
}

#[derive(clap::Args)]
struct AddLevel {
    #[arg(long)]
    id: String,
    #[arg(long, default_value = "uniswap")]
    venue: String,
    #[arg(long)]
    chain: String,
    #[arg(long)]
    token_in: String,
    #[arg(long)]
    token_out: String,
    /// `buy` watches the price of token_out, `sell` the price of token_in.
    #[arg(long)]
    side: String,
    #[arg(long)]
    trigger_price_usd: f64,
    /// How much token_in to spend, in whole units rather than base units.
    #[arg(long)]
    amount: String,
    #[arg(long)]
    slippage_bps: u16,
}

impl From<AddLevel> for LevelDraft {
    fn from(args: AddLevel) -> Self {
        Self {
            id: args.id,
            venue: args.venue,
            chain: args.chain,
            token_in: args.token_in,
            token_out: args.token_out,
            side: args.side,
            trigger_price_usd: args.trigger_price_usd,
            amount: args.amount,
            slippage_bps: args.slippage_bps,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { log_dir } => {
            run(Options {
                config: cli.config,
                log_dir,
                // Spending real funds takes an explicit word, and only that one word.
                allow_broadcast: std::env::var("MAINNET_SWAP").as_deref() == Ok("1"),
            })
            .await?;
        }
        Command::Bootstrap => {
            let report = provision::bootstrap(&cli.config)?;
            println!("evm: {}", report.evm);
            if let Some(address) = report.sui {
                println!("sui: {address}");
            }
            println!("bootstrap: ok");
        }
        Command::ImportKey {
            chain,
            private_key,
            force,
        } => {
            let address = match chain {
                ImportChain::Evm => provision::import_evm_key(&cli.config, &private_key, force)?,
                ImportChain::Sui => provision::import_sui_key(&cli.config, &private_key, force)?,
            };
            println!("{address}");
        }
        Command::Health => {
            let config = Config::load(&cli.config)?;
            if std::env::var_os(&config.uniswap.api_key_env).is_none() {
                bail!(
                    "missing required environment variable {}",
                    config.uniswap.api_key_env
                );
            }
            // Opening runs the migrations, so this also proves they apply.
            connect_pool(database(&config)).await?;
            println!("config: ok\ndatabase: ok\ntools: ok");
        }
        Command::Level { action } => {
            let config = Config::load(&cli.config)?;
            level_command(&config, action).await?;
        }
        Command::ResolveQuarantine { order_id } => {
            let config = Config::load(&cli.config)?;
            let orders = SqliteOrderStore::new(connect_pool(database(&config)).await?);
            let level_id = resolve_quarantine(&orders, &order_id).await?;
            println!("order {order_id}: quarantine resolved, level {level_id} released");
        }
    }
    Ok(())
}

async fn level_command(config: &Config, action: LevelCommand) -> Result<()> {
    let levels = SqliteLevelStore::new(connect_pool(database(config)).await?);
    match action {
        LevelCommand::Add(args) => {
            let prices = DexPaprikaSource::new(&config.dexpaprika_stream_url);
            let level =
                validate_level(&config.evm, config.max_slippage_bps, &prices, &args.into())?;
            levels.upsert_level(&level).await?;
            println!("level {}: stored", level.id);
        }
        LevelCommand::List => {
            for level in levels.list_levels().await? {
                println!(
                    "{}  {} {} {} {} -> {} at {} USD, {} base units, {} bps",
                    level.id,
                    level.venue.as_str(),
                    level.chain,
                    level.side.as_str(),
                    level.token_in,
                    level.token_out,
                    level.trigger_price_usd,
                    level.amount,
                    level.slippage_bps,
                );
            }
        }
        LevelCommand::Rm { id } => {
            levels.delete_level(&id).await?;
            println!("level {id}: deleted");
        }
    }
    Ok(())
}

fn database(config: &Config) -> &Path {
    Path::new(&config.state_db_path)
}
