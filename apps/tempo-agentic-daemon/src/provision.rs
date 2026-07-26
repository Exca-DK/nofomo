use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use tempo_agentic_vault::{ChainVault, EvmVault, SuiVault};

const SUI_DEV_ALIAS: &str = "tempo-agentic";

/// Which keystore an imported private key belongs in.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum ImportChain {
    Evm,
    Sui,
}

/// Addresses the daemon will sign with after a bootstrap.
pub struct BootstrapReport {
    pub evm: String,
    /// Only set when `sui.enabled` is true in the configuration.
    pub sui: Option<String>,
}

/// Creates throwaway local accounts that do not exist yet and makes sure the
/// state directory is there.
///
/// Returns an error if the configuration cannot be read, a required path is
/// missing from it, or a keystore cannot be written.
pub fn bootstrap(config_path: &str) -> Result<BootstrapReport> {
    let raw = raw_config(config_path)?;

    let evm = evm_vault(&raw)?;
    evm.bootstrap()?;

    let sui = if raw
        .pointer("/sui/enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let vault = sui_vault(&raw)?;
        vault.bootstrap()?;
        Some(vault.address()?)
    } else {
        None
    };

    if let Some(path) = str_at(&raw, "/state_db_path")
        && let Some(parent) = Path::new(path).parent()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    Ok(BootstrapReport {
        evm: evm.address()?,
        sui,
    })
}

/// Installs an existing private key as the account the daemon signs with, and
/// reports the address it belongs to.
///
/// Returns an error if the configuration cannot be read, the key is malformed,
/// or a keystore is already there and `force` is false.
pub fn import_key(
    config_path: &str,
    chain: ImportChain,
    private_key: &str,
    force: bool,
) -> Result<String> {
    let raw = raw_config(config_path)?;
    match chain {
        ImportChain::Evm => evm_vault(&raw)?.import_key(private_key, force),
        ImportChain::Sui => sui_vault(&raw)?.import_key(private_key, force),
    }
}

// Read as plain JSON rather than through `Config::load`, because validation
// insists the keystore files exist and these are the commands that create them.
fn raw_config(config_path: &str) -> Result<Value> {
    let raw =
        fs::read_to_string(config_path).with_context(|| format!("cannot read {config_path}"))?;
    serde_json::from_str(&raw).with_context(|| format!("invalid JSON in {config_path}"))
}

fn str_at<'a>(raw: &'a Value, pointer: &str) -> Option<&'a str> {
    raw.pointer(pointer).and_then(Value::as_str)
}

fn evm_vault(raw: &Value) -> Result<EvmVault> {
    Ok(EvmVault {
        keystore_path: PathBuf::from(
            str_at(raw, "/evm/keystore_path").context("evm.keystore_path is required")?,
        ),
        password_file: PathBuf::from(
            str_at(raw, "/evm/password_file").context("evm.password_file is required")?,
        ),
    })
}

fn sui_vault(raw: &Value) -> Result<SuiVault> {
    Ok(SuiVault {
        keystore_path: PathBuf::from(
            str_at(raw, "/sui/keystore_path").context("sui.keystore_path is required")?,
        ),
        alias: SUI_DEV_ALIAS.to_string(),
    })
}
