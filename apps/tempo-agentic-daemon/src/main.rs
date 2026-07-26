use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use tempo_agentic_config::Config;
use tempo_agentic_daemon::{Options, deps, keystore, run};
use tempo_agentic_domain::{ChainFamily, Signer};
use tempo_agentic_orchestrator::resolve_quarantine;
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
    /// Manage signing keys.
    Keystore {
        #[command(subcommand)]
        action: KeystoreCommand,
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

#[derive(Subcommand)]
enum KeystoreCommand {
    /// Generate a signing key.
    Generate {
        /// Chain or family the key is for, like `base`, `eip155` or `sui`.
        #[arg(long)]
        chain: String,
    },
    /// Import a key, prompting when omitted.
    Import {
        /// Chain or family the key is for, like `base`, `eip155` or `sui`.
        #[arg(long)]
        chain: String,
        /// Passing a key may expose it in shell history and process listings.
        #[arg(long)]
        key: Option<String>,
    },
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
            let report = keystore::bootstrap(&cli.config)?;
            println!("evm: {}", report.evm);
            if let Some(address) = report.sui {
                println!("sui: {address}");
            }
            println!("bootstrap: ok");
        }
        Command::Keystore { action } => {
            let (family, address) = match action {
                KeystoreCommand::Generate { chain } => {
                    let family = ChainFamily::resolve(&chain)?;
                    (family, keystore::generate(&cli.config, family)?)
                }
                KeystoreCommand::Import { chain, key } => {
                    let family = ChainFamily::resolve(&chain)?;
                    (family, keystore::import(&cli.config, family, key)?)
                }
            };
            println!("{family}: {address}");
        }
        Command::Health => {
            let config = Config::load(&cli.config)?;
            if std::env::var_os(&config.uniswap.api_key_env).is_none() {
                bail!(
                    "missing required environment variable {}",
                    config.uniswap.api_key_env
                );
            }
            // Opening runs migrations; loading validates keys.
            connect_pool(database(&config)).await?;
            let vault = keystore::load_vault(&config)?;
            println!("config: ok\ndatabase: ok\ntools: ok");
            println!("evm: {}", vault.address(ChainFamily::Evm)?);
            if config.sui.enabled {
                println!("sui: {}", vault.address(ChainFamily::Sui)?);
            }
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
            let level = validate_level(
                deps::tokens(config).as_ref(),
                config.max_slippage_bps,
                deps::prices(config).as_ref(),
                &args.into(),
            )?;
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
