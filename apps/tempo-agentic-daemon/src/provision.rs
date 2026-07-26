use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tempo_agentic_config::Config;
use tempo_agentic_vault::{ChainVault, EvmVault, SuiVault};

const SUI_DEV_ALIAS: &str = "tempo-agentic";

/// Addresses the daemon will sign with after a bootstrap.
pub struct BootstrapReport {
    pub evm: String,
    /// Only set when `sui.enabled` is true in the configuration.
    pub sui: Option<String>,
}

/// Creates missing local development accounts and state directories.
pub fn bootstrap(config_path: &str) -> Result<BootstrapReport> {
    let config = load(config_path)?;

    let evm = evm_vault(&config);
    evm.bootstrap()?;

    let sui = if config.sui.enabled {
        let vault = sui_vault(&config)?;
        vault.bootstrap()?;
        Some(vault.address()?)
    } else {
        None
    };

    if let Some(parent) = Path::new(&config.state_db_path).parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    Ok(BootstrapReport {
        evm: evm.address()?,
        sui,
    })
}

/// Imports an EVM key, optionally replacing its keystore.
pub fn import_evm_key(config_path: &str, private_key: &str, force: bool) -> Result<String> {
    evm_vault(&load(config_path)?).import_key(private_key, force)
}

/// Imports a Sui keypair, optionally replacing its keystore.
pub fn import_sui_key(config_path: &str, private_key: &str, force: bool) -> Result<String> {
    sui_vault(&load(config_path)?)?.import_key(private_key, force)
}

// Skip validation because these commands create the required keystores.
fn load(config_path: &str) -> Result<Config> {
    Config::load_unvalidated(config_path)
}

fn evm_vault(config: &Config) -> EvmVault {
    EvmVault {
        keystore_path: PathBuf::from(&config.evm.keystore_path),
        password_file: PathBuf::from(&config.evm.password_file),
    }
}

fn sui_vault(config: &Config) -> Result<SuiVault> {
    Ok(SuiVault {
        keystore_path: PathBuf::from(
            config
                .sui
                .keystore_path
                .as_deref()
                .context("sui.keystore_path is required")?,
        ),
        alias: SUI_DEV_ALIAS.to_string(),
    })
}
