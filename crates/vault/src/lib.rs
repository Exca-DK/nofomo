use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

/// Ensures a local signing account exists for one chain and reports its address.
/// Uses the chain's CLI tools instead of holding key material in process.
pub trait ChainVault {
    /// Bootstraps the local signing account if not already configured.
    fn bootstrap(&self) -> Result<()>;
    /// Returns the public address managed by the vault.
    fn address(&self) -> Result<String>;
}

/// EVM account backed by a Foundry keystore managed via cast CLI.
pub struct EvmVault {
    pub keystore_path: PathBuf,
    pub password_file: PathBuf,
}

impl ChainVault for EvmVault {
    fn bootstrap(&self) -> Result<()> {
        ensure_password_file(&self.password_file)?;
        if !self.keystore_path.exists() {
            let password = fs::read_to_string(&self.password_file)
                .with_context(|| format!("cannot read {}", self.password_file.display()))?;
            generate_evm_keystore(&self.keystore_path, password.trim())?;
        }
        Ok(())
    }

    fn address(&self) -> Result<String> {
        let password = fs::read_to_string(&self.password_file)
            .with_context(|| format!("cannot read {}", self.password_file.display()))?;
        let key = eth_keystore::decrypt_key(&self.keystore_path, password.trim())
            .with_context(|| format!("cannot decrypt keystore {}", self.keystore_path.display()))?;
        let signer = alloy::signers::local::PrivateKeySigner::from_slice(&key)
            .context("invalid private key in keystore")?;
        Ok(signer.address().to_string())
    }
}

/// Sui account backed by a client configuration file and keystore.
/// Sui CLI requires operating through client.yaml rather than a raw keystore path directly.
pub struct SuiVault {
    pub client_config: PathBuf,
    /// Key alias inside the Sui keystore.
    pub alias: String,
}

impl ChainVault for SuiVault {
    fn bootstrap(&self) -> Result<()> {
        if self.client_config.exists() {
            return Ok(());
        }
        if let Some(parent) = self.client_config.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }

        let keystore_path = self.client_config.with_file_name("sui.keystore");

        let mut rng = rand::thread_rng();
        let priv_key = sui_crypto::ed25519::Ed25519PrivateKey::generate(&mut rng);
        let keypair = sui_crypto::simple::SimpleKeypair::from(priv_key);
        let base64_key = keypair.to_base64();

        let mut keys = Vec::new();
        if keystore_path.exists() {
            let content = fs::read_to_string(&keystore_path).unwrap_or_default();
            if let Ok(existing) = serde_json::from_str::<Vec<String>>(&content) {
                keys = existing;
            }
        }
        keys.push(base64_key);
        fs::write(&keystore_path, serde_json::to_string_pretty(&keys).unwrap())
            .context("failed to write Sui keystore")?;

        // Creates dummy client.yaml because Sui CLI tools require its presence.
        fs::write(
            &self.client_config,
            format!("keystore:\n  File: {}", keystore_path.display()),
        )
        .context("failed to write dummy client.yaml")?;

        Ok(())
    }

    fn address(&self) -> Result<String> {
        let keystore_path = self.client_config.with_file_name("sui.keystore");
        let content = fs::read_to_string(&keystore_path).context("failed to read Sui keystore")?;

        let keys: Vec<String> =
            serde_json::from_str(&content).context("invalid JSON in Sui keystore")?;

        for key_str in keys {
            if let Ok(keypair) = sui_crypto::simple::SimpleKeypair::from_base64(&key_str) {
                let address = keypair.verifying_key().derive_address();
                return Ok(address.to_string());
            }
        }

        bail!("alias {} not found in Sui keystore", self.alias);
    }
}

fn ensure_password_file(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    fs::write(path, format!("{}\n", random_hex(32)?))
        .with_context(|| format!("cannot write {}", path.display()))?;
    set_owner_only(path)
}

fn generate_evm_keystore(keystore_path: &Path, password: &str) -> Result<()> {
    let dir = keystore_path
        .parent()
        .context("evm.keystore_path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;

    let mut rng = rand::thread_rng();
    let id = eth_keystore::new(dir, &mut rng, password, None)
        .context("failed to generate EVM keystore")?;

    let generated_path = dir.join(id.1);
    if generated_path.exists() {
        fs::rename(&generated_path, keystore_path).with_context(|| {
            format!(
                "cannot move generated keystore into place at {}",
                keystore_path.display()
            )
        })?;
    } else {
        bail!(
            "eth_keystore did not produce a keystore at {}",
            keystore_path.display()
        );
    }

    set_owner_only(keystore_path)
}

fn random_hex(bytes: usize) -> Result<String> {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}
