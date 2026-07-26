use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use tempo_agentic_config::Config;
use tempo_agentic_daemon::{Options, dashboard, deps, keystore, run};
use tempo_agentic_domain::{ChainFamily, Signer};
use tempo_agentic_orchestrator::resolve_quarantine;
use tempo_agentic_storage::{
    LockFile, SqliteLevelStore, SqliteOrderStore, SqliteStrategyStore, connect_pool,
    initialize_new_under_lock, open_existing_read_only,
};
use tempo_agentic_strategy::{LevelStore, StrategyStore, trade_direction};
use tempo_agentic_trigger::{LevelDraft, StrategyDraft, validate_level, validate_strategy};

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
    /// Attach a terminal dashboard to an already-running daemon.
    Dashboard,
    /// Create throwaway local dev accounts if missing without modifying git.
    Bootstrap,
    /// Import an existing account by private key instead of generating a throwaway one.
    Keystore {
        #[command(subcommand)]
        action: KeystoreCommand,
    },
    /// Validate config, secrets, and SQLite without printing secrets.
    Health,
    /// Manage strategy markets.
    Strategy {
        #[command(subcommand)]
        action: StrategyCommand,
    },
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
    /// Generate a signing key at the path the config points at.
    Generate {
        /// Chain or family the key is for, like `base`, `eip155` or `sui`.
        #[arg(long)]
        chain: String,
    },
    /// Import a key, asking for it unless `--key` is given.
    Import {
        /// Chain or family the key is for, like `base`, `eip155` or `sui`.
        #[arg(long)]
        chain: String,
        /// Passing a key exposes it in shell history and process listings.
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

#[derive(Subcommand)]
enum StrategyCommand {
    /// Store a strategy market after checking it against the configuration.
    Add(AddStrategy),
    List,
}

#[derive(clap::Args)]
struct AddStrategy {
    #[arg(long)]
    id: String,
    #[arg(long, default_value = "uniswap")]
    venue: String,
    #[arg(long)]
    chain: String,
    #[arg(long)]
    base_token: String,
    #[arg(long)]
    quote_token: String,
}

#[derive(clap::Args)]
struct AddLevel {
    #[arg(long)]
    id: String,
    #[arg(long)]
    strategy_id: String,
    /// `buy` spends quote for base; `sell` spends base for quote.
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
            strategy_id: args.strategy_id,
            side: args.side,
            trigger_price_usd: args.trigger_price_usd,
            amount: args.amount,
            slippage_bps: args.slippage_bps,
        }
    }
}

impl From<AddStrategy> for StrategyDraft {
    fn from(args: AddStrategy) -> Self {
        Self {
            id: args.id,
            venue: args.venue,
            chain: args.chain,
            base_token: args.base_token,
            quote_token: args.quote_token,
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
        Command::Dashboard => dashboard::run(&cli.config).await?,
        Command::Bootstrap => {
            let report = keystore::bootstrap(&cli.config)?;
            let config = Config::load(&cli.config)?;
            let database = database(&config);
            let lock = LockFile::acquire(LockFile::path_for(database))?;
            if database.exists() {
                connect_pool(database).await?.close().await;
            } else {
                initialize_new_under_lock(database, &lock)
                    .await?
                    .close()
                    .await;
            }
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
            connect_pool(database(&config)).await?;
            // Loading proves each key parses; only public addresses are printed.
            let vault = keystore::load_vault(&config)?;
            println!("config: ok\ndatabase: ok\ntools: ok");
            println!("evm: {}", vault.address(ChainFamily::Evm)?);
            if config.sui.enabled {
                println!("sui: {}", vault.address(ChainFamily::Sui)?);
            }
        }
        Command::Strategy { action } => match action {
            StrategyCommand::List => {
                let config = Config::load(&cli.config)?;
                strategy_list(&config).await?;
            }
            StrategyCommand::Add(args) => {
                let (config, _lock) = offline_authoring_config(&cli.config)?;
                strategy_add(&config, args).await?;
            }
        },
        Command::Level { action } => match action {
            LevelCommand::List => {
                let config = Config::load(&cli.config)?;
                level_list(&config).await?;
            }
            action => {
                let (config, _lock) = offline_authoring_config(&cli.config)?;
                level_mutation(&config, action).await?;
            }
        },
        Command::ResolveQuarantine { order_id } => {
            let config = Config::load(&cli.config)?;
            let orders = SqliteOrderStore::new(connect_pool(database(&config)).await?);
            let level_id = resolve_quarantine(&orders, &order_id).await?;
            println!("order {order_id}: quarantine resolved, level {level_id} released");
        }
    }
    Ok(())
}

async fn strategy_add(config: &Config, args: AddStrategy) -> Result<()> {
    let pool = connect_pool(database(config)).await?;
    let strategies = SqliteStrategyStore::new(pool);
    let strategy = validate_strategy(
        deps::tokens(config).as_ref(),
        deps::prices(config).as_ref(),
        &args.into(),
    )?;
    strategies.upsert_strategy(&strategy).await?;
    println!("strategy {}: stored", strategy.id);
    Ok(())
}

async fn strategy_list(config: &Config) -> Result<()> {
    let strategies = SqliteStrategyStore::new(open_existing_read_only(database(config)).await?);
    for strategy in strategies.list_strategies().await? {
        println!(
            "{}  {} {} {}/{}",
            strategy.id,
            strategy.venue.as_str(),
            strategy.chain,
            strategy.base_token,
            strategy.quote_token
        );
    }
    Ok(())
}

async fn level_mutation(config: &Config, action: LevelCommand) -> Result<()> {
    let pool = connect_pool(database(config)).await?;
    let strategies = SqliteStrategyStore::new(pool.clone());
    let levels = SqliteLevelStore::new(pool);
    match action {
        LevelCommand::Add(args) => {
            let draft = LevelDraft::from(args);
            let strategy = strategies
                .get_strategy(&draft.strategy_id)
                .await?
                .with_context(|| format!("strategy {} does not exist", draft.strategy_id))?;
            let level = validate_level(
                deps::tokens(config).as_ref(),
                config.max_slippage_bps,
                deps::prices(config).as_ref(),
                &strategy,
                &draft,
            )?;
            levels.upsert_level(&level, &strategy).await?;
            println!("level {}: stored", level.id);
        }
        LevelCommand::Rm { id } => {
            levels.delete_level(&id).await?;
            println!("level {id}: deleted");
        }
        LevelCommand::List => unreachable!("list is handled without an authoring lock"),
    }
    Ok(())
}

async fn level_list(config: &Config) -> Result<()> {
    let levels = SqliteLevelStore::new(open_existing_read_only(database(config)).await?);
    for entry in levels.list_levels().await? {
        let direction = trade_direction(&entry.strategy, entry.level.side);
        println!(
            "{}  {} {} {} {} -> {} at {} USD, {} base units, {} bps",
            entry.level.id,
            entry.strategy.id,
            entry.strategy.chain,
            entry.level.side.as_str(),
            direction.token_in,
            direction.token_out,
            entry.level.trigger_price_usd,
            entry.level.amount,
            entry.level.slippage_bps,
        );
    }
    Ok(())
}

#[derive(Deserialize)]
struct DatabaseLocator {
    state_db_path: Option<String>,
}

fn offline_authoring_config(config_path: &str) -> Result<(Config, LockFile)> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("cannot read config {config_path}"))?;
    let locator: DatabaseLocator =
        serde_json::from_str(&raw).with_context(|| format!("invalid JSON in {config_path}"))?;
    let located = locator.state_db_path.map_or_else(
        || {
            std::env::var("HOME")
                .map(|home| PathBuf::from(home).join(".tempo-agentic/state.db"))
                .unwrap_or_else(|_| PathBuf::from("/tmp/tempo-agentic.db"))
        },
        PathBuf::from,
    );
    let lock = LockFile::acquire(LockFile::path_for(&located)).map_err(|error| {
        anyhow::anyhow!(
            "direct CLI authoring is offline-only; when the daemon is running use MCP ({error})"
        )
    })?;
    let config = Config::load(config_path)?;
    if database(&config) != located {
        bail!("state_db_path changed while acquiring the authoring lock; retry");
    }
    Ok((config, lock))
}

fn database(config: &Config) -> &Path {
    Path::new(&config.state_db_path)
}

#[cfg(test)]
mod tests {
    use super::offline_authoring_config;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempo_agentic_storage::LockFile;

    #[test]
    fn daemon_lock_refuses_direct_authoring_before_changed_config_is_used() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let database = std::env::temp_dir().join(format!(
            "tempo-agentic-cli-lock-{}-{id}.db",
            std::process::id()
        ));
        let config = database.with_extension("json");
        let lock = LockFile::acquire(LockFile::path_for(&database)).unwrap();
        // The rest deliberately resembles a changed/broken config: the active daemon's lock
        // must refuse the write before CLI validation can adopt any on-disk config drift.
        std::fs::write(
            &config,
            serde_json::json!({
                "state_db_path": database,
                "evm": { "chains": [{ "name": "changed-config-b" }] }
            })
            .to_string(),
        )
        .unwrap();

        let error = offline_authoring_config(config.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("offline-only"));
        assert!(error.contains("use MCP"));

        drop(lock);
        let _ = std::fs::remove_file(config);
    }
}
