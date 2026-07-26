use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::json;
use tempo_agentic_config::Config;
use tempo_agentic_daemon::admin_client::AdminClient;
use tempo_agentic_daemon::{Options, dashboard, deps, keystore, run};
use tempo_agentic_domain::{ChainFamily, Signer};
use tempo_agentic_mcp::manifest_path;
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
            StrategyCommand::Add(args) => match authoring(&cli.config)? {
                Authoring::Offline(config, _lock) => strategy_add(&config, args).await?,
                Authoring::Daemon(client) => {
                    let draft = StrategyDraft::from(args);
                    client
                        .call("set_strategy", serde_json::to_value(&draft)?)
                        .await?;
                    println!("strategy {}: stored by the running daemon", draft.id);
                }
            },
        },
        Command::Level { action } => match action {
            LevelCommand::List => {
                let config = Config::load(&cli.config)?;
                level_list(&config).await?;
            }
            action => match authoring(&cli.config)? {
                Authoring::Offline(config, _lock) => level_mutation(&config, action).await?,
                Authoring::Daemon(client) => daemon_level_mutation(&client, action).await?,
            },
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

async fn daemon_level_mutation(client: &AdminClient, action: LevelCommand) -> Result<()> {
    match action {
        LevelCommand::Add(args) => {
            let draft = LevelDraft::from(args);
            client
                .call("set_level", serde_json::to_value(&draft)?)
                .await?;
            println!("level {}: stored by the running daemon", draft.id);
        }
        LevelCommand::Rm { id } => {
            client.call("delete_level", json!({ "id": id })).await?;
            println!("level {id}: deleted by the running daemon");
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

/// Where an authoring write goes.
enum Authoring {
    /// Nothing is trading, so the write goes straight into SQLite.
    Offline(Box<Config>, LockFile),
    /// A daemon owns the database and applies the write through its admin tools.
    Daemon(AdminClient),
}

fn authoring(config_path: &str) -> Result<Authoring> {
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
    match LockFile::acquire(LockFile::path_for(&located)) {
        Ok(lock) => {
            let config = Config::load(config_path)?;
            if database(&config) != located {
                bail!("state_db_path changed while acquiring the authoring lock; retry");
            }
            Ok(Authoring::Offline(Box::new(config), lock))
        }
        // Whoever holds the lock is trading, so it owns the config too: hand the
        // draft to its admin surface instead of reading config from disk again.
        Err(held) => AdminClient::attach(&manifest_path(&located))
            .map(Authoring::Daemon)
            .map_err(|error| {
                anyhow::anyhow!("{held}; and it publishes no admin surface ({error})")
            }),
    }
}

fn database(config: &Config) -> &Path {
    Path::new(&config.state_db_path)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use tempo_agentic_mcp::manifest_path;
    use tempo_agentic_storage::LockFile;

    use super::{Authoring, authoring};

    #[test]
    fn a_locked_database_with_no_admin_surface_is_refused() {
        let (database, config) = fixture("no-surface");
        let lock = LockFile::acquire(LockFile::path_for(&database)).unwrap();

        let error = authoring(config.to_str().unwrap())
            .err()
            .expect("a held lock with nothing listening cannot be authored")
            .to_string();

        assert!(error.contains("another daemon holds"), "{error}");
        assert!(error.contains("admin surface"), "{error}");
        // Config drift must never be adopted: the lock decides before it is read.
        assert!(!error.contains("changed-config-b"), "{error}");

        drop(lock);
        clean([config]);
    }

    #[test]
    fn a_locked_database_is_authored_through_the_daemon_that_holds_it() {
        let (database, config) = fixture("surface");
        let lock = LockFile::acquire(LockFile::path_for(&database)).unwrap();
        let manifest = manifest_path(&database);
        std::fs::write(
            &manifest,
            serde_json::json!({ "url": "http://127.0.0.1:1/", "token": "t" }).to_string(),
        )
        .unwrap();

        // The broken config on disk is never loaded on this path.
        assert!(matches!(
            authoring(config.to_str().unwrap()).unwrap(),
            Authoring::Daemon(_)
        ));

        drop(lock);
        clean([config, manifest]);
    }

    #[test]
    fn an_unlocked_database_takes_the_direct_path() {
        let (_database, config) = fixture("free");

        // With nothing holding the lock the direct path is chosen, which is the
        // only one that loads config. The fixture's config is unusable, so that
        // choice shows up as a config error rather than a daemon one.
        let error = authoring(config.to_str().unwrap())
            .err()
            .expect("the fixture config cannot load")
            .to_string();

        assert!(!error.contains("another daemon holds"), "{error}");
        assert!(!error.contains("admin surface"), "{error}");

        clean([config]);
    }

    // The config deliberately names a chain no venue can satisfy, so any test that
    // reaches configuration loading fails loudly instead of passing by accident.
    fn fixture(name: &str) -> (PathBuf, PathBuf) {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let database = std::env::temp_dir().join(format!(
            "tempo-agentic-cli-lock-{}-{name}-{id}.db",
            std::process::id()
        ));
        let config = database.with_extension("json");
        std::fs::write(
            &config,
            serde_json::json!({
                "state_db_path": database,
                "evm": { "chains": [{ "name": "changed-config-b" }] }
            })
            .to_string(),
        )
        .unwrap();
        (database, config)
    }

    fn clean<const N: usize>(paths: [PathBuf; N]) {
        for path in paths {
            let _ = std::fs::remove_file(path);
        }
    }
}
