use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tempo_agentic_config::Config;
use tempo_agentic_domain::ChainFamily;
use tempo_agentic_vault::{Vault, VaultSigner, write_atomic_secret};

/// Addresses loaded or generated during bootstrap.
pub struct BootstrapReport {
    pub evm: String,
    pub sui: Option<String>,
}

/// Ensures local accounts and the state directory exist.
pub fn bootstrap(config_path: &str) -> Result<BootstrapReport> {
    let config = load(config_path)?;

    let evm = ensure_key(&config, ChainFamily::Evm)?;
    let sui = if config.sui.enabled {
        Some(ensure_key(&config, ChainFamily::Sui)?)
    } else {
        None
    };

    if let Some(parent) = Path::new(&config.state_db_path).parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    Ok(BootstrapReport { evm, sui })
}

/// Generates a key without replacing an existing one.
pub fn generate(config_path: &str, family: ChainFamily) -> Result<String> {
    let path = key_path(&load(config_path)?, family)?;
    refuse_existing(&path)?;
    persist(&path, VaultSigner::generate(family))
}

/// Imports a key, prompting when omitted, without replacing an existing one.
pub fn import(config_path: &str, family: ChainFamily, key: Option<String>) -> Result<String> {
    let path = key_path(&load(config_path)?, family)?;
    refuse_existing(&path)?;

    let signer = match key {
        Some(key) => VaultSigner::import(family, key.trim())
            .with_context(|| format!("invalid {family} private key"))?,
        None => prompt_key(family)?,
    };
    persist(&path, signer)
}

/// Loads configured keys into one vault.
pub fn load_vault(config: &Config) -> Result<Vault> {
    let mut vault = Vault::new();

    let evm = Path::new(&config.keys.evm);
    vault.add(
        VaultSigner::load(ChainFamily::Evm, evm)
            .with_context(|| format!("cannot load the EVM key at {}", evm.display()))?,
    );

    if config.sui.enabled {
        let sui = key_path(config, ChainFamily::Sui)?;
        vault.add(
            VaultSigner::load(ChainFamily::Sui, &sui)
                .with_context(|| format!("cannot load the Sui key at {}", sui.display()))?,
        );
    }

    Ok(vault)
}

fn ensure_key(config: &Config, family: ChainFamily) -> Result<String> {
    let path = key_path(config, family)?;
    if path.exists() {
        let signer = VaultSigner::load(family, &path)
            .with_context(|| format!("cannot load the {family} key at {}", path.display()))?;
        return Ok(signer.address().to_string());
    }
    persist(&path, VaultSigner::generate(family))
}

fn load(config_path: &str) -> Result<Config> {
    Config::load_unvalidated(config_path)
}

fn key_path(config: &Config, family: ChainFamily) -> Result<PathBuf> {
    let configured = match family {
        ChainFamily::Evm => Some(config.keys.evm.as_str()),
        ChainFamily::Sui => config.keys.sui.as_deref(),
    };
    configured
        .map(PathBuf::from)
        .with_context(|| format!("set keys.{} in the config first", field_name(family)))
}

fn field_name(family: ChainFamily) -> &'static str {
    match family {
        ChainFamily::Evm => "evm",
        ChainFamily::Sui => "sui",
    }
}

fn refuse_existing(path: &Path) -> Result<()> {
    if path.exists() {
        bail!(
            "a key already exists at {}; move it aside first",
            path.display()
        );
    }
    Ok(())
}

fn persist(path: &Path, signer: VaultSigner) -> Result<String> {
    write_atomic_secret(path, signer.secret_material().as_bytes())?;
    Ok(signer.address().to_string())
}

// Prompting keeps secrets out of shell history and process arguments.
fn prompt_key(family: ChainFamily) -> Result<VaultSigner> {
    let validate = move |input: &str| match VaultSigner::import(family, input.trim()) {
        Ok(_) => Ok(inquire::validator::Validation::Valid),
        Err(error) => Ok(inquire::validator::Validation::Invalid(
            error.to_string().into(),
        )),
    };

    let key = inquire::Password::new(&format!("{family} private key"))
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .with_help_message("paste the key in this family's own format; it is never echoed")
        .without_confirmation()
        .with_validator(validate)
        .prompt()
        .context("failed to read the private key")?;

    VaultSigner::import(family, key.trim()).with_context(|| format!("invalid {family} private key"))
}
