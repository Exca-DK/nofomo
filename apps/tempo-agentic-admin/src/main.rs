use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tempo_agentic_config::Config;
use tempo_agentic_storage::SqliteAuditStore;
use tempo_agentic_vault::{ChainVault, EvmVault, SuiVault};

#[derive(Parser)]
#[command(name = "tempo-agentic-admin", version)]
struct Cli {
    #[arg(long, env = "TEMPO_AGENTIC_CONFIG", default_value = "config.json")]
    config: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create throwaway local dev accounts if missing without modifying git.
    Bootstrap,
    /// Apply embedded SQLite migrations.
    Migrate,
    /// Validate config, secrets, tools, and SQLite without printing secrets.
    Health,
    /// Print recent redacted quote and execution audit records.
    Audit {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Delete audit rows older than the supplied number of days.
    Prune {
        #[arg(long)]
        older_than_days: u32,
    },
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
}

#[derive(Clone, clap::ValueEnum)]
enum ImportChain {
    Evm,
    Sui,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Bootstrap => bootstrap(&cli.config)?,
        Command::Migrate => {
            load(&cli.config).await?;
            println!("migrations: ok");
        }
        Command::Health => {
            let (config, store) = load(&cli.config).await?;
            require_env(&config.uniswap.api_key_env)?;
            store.health().await?;
            println!("config: ok\ndatabase: ok\ntools: ok");
        }
        Command::Audit { limit } => {
            let (_, store) = load(&cli.config).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&store.recent_audit(limit).await?)?
            );
        }
        Command::Prune { older_than_days } => {
            let (_, store) = load(&cli.config).await?;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let cutoff = now.saturating_sub(u64::from(older_than_days) * 86_400);
            let deleted = store.prune(i64::try_from(cutoff)?).await?;
            println!("deleted quotes: {deleted}");
        }
        Command::ImportKey {
            chain,
            private_key,
            force,
        } => import_key(&cli.config, chain, &private_key, force)?,
    }
    Ok(())
}

// Loads config and opens audit store for subcommands that require an existing config.
async fn load(config_path: &str) -> Result<(Config, SqliteAuditStore)> {
    let config = Config::load(config_path)?;
    let store = SqliteAuditStore::admin(&config.state_db_path).await?;
    Ok((config, store))
}

const SUI_DEV_ALIAS: &str = "tempo-agentic";

// Reads raw JSON instead of Config::load because validation fails before keystore files exist.
fn bootstrap(config_path: &str) -> Result<()> {
    let raw = fs::read_to_string(config_path)
        .with_context(|| format!("cannot read config {config_path}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("invalid JSON in {config_path}"))?;

    let str_at = |pointer: &str| value.pointer(pointer).and_then(|v| v.as_str());

    let evm = EvmVault {
        keystore_path: PathBuf::from(
            str_at("/evm/keystore_path").context("evm.keystore_path is required")?,
        ),
        password_file: PathBuf::from(
            str_at("/evm/password_file").context("evm.password_file is required")?,
        ),
    };
    evm.bootstrap()?;
    println!("evm: {}", evm.address()?);

    if value
        .pointer("/sui/enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let sui = SuiVault {
            keystore_path: PathBuf::from(
                str_at("/sui/keystore_path")
                    .context("sui.keystore_path is required when sui.enabled is true")?,
            ),
            alias: SUI_DEV_ALIAS.to_string(),
        };
        sui.bootstrap()?;
        println!("sui: {}", sui.address()?);
    }

    if let Some(path) = str_at("/state_db_path")
        && let Some(parent) = Path::new(path).parent()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    println!("bootstrap: ok");
    Ok(())
}

// Reads raw JSON instead of Config::load because validation fails before keystore files exist.
fn import_key(config_path: &str, chain: ImportChain, private_key: &str, force: bool) -> Result<()> {
    let raw = fs::read_to_string(config_path)
        .with_context(|| format!("cannot read config {config_path}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("invalid JSON in {config_path}"))?;

    let str_at = |pointer: &str| value.pointer(pointer).and_then(|v| v.as_str());

    match chain {
        ImportChain::Evm => {
            let evm = EvmVault {
                keystore_path: PathBuf::from(
                    str_at("/evm/keystore_path").context("evm.keystore_path is required")?,
                ),
                password_file: PathBuf::from(
                    str_at("/evm/password_file").context("evm.password_file is required")?,
                ),
            };
            let address = evm.import_key(private_key, force)?;
            println!("evm: {address}");
        }
        ImportChain::Sui => {
            let sui = SuiVault {
                keystore_path: PathBuf::from(
                    str_at("/sui/keystore_path").context("sui.keystore_path is required")?,
                ),
                alias: SUI_DEV_ALIAS.to_string(),
            };
            let address = sui.import_key(private_key, force)?;
            println!("sui: {address}");
        }
    }

    Ok(())
}

fn require_env(name: &str) -> Result<()> {
    if std::env::var_os(name).is_none() {
        bail!("missing required environment variable {name}");
    }
    Ok(())
}
