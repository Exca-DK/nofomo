use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail};

/// Ensures a local signing account exists for one chain and reports its address.
pub trait ChainVault {
    /// Bootstraps the local signing account if not already configured.
    fn bootstrap(&self) -> Result<()>;
    /// Returns the public address managed by the vault.
    fn address(&self) -> Result<String>;
}

/// EVM account backed by an encrypted JSON keystore file.
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

impl EvmVault {
    /// Imports an existing private key into the keystore instead of generating a throwaway key.
    /// Returns an error if the keystore already exists and force is not set, or if key encryption fails.
    pub fn import_key(&self, private_key: &str, force: bool) -> Result<String> {
        if self.keystore_path.exists() && !force {
            bail!(
                "keystore already exists at {} (use --force to overwrite)",
                self.keystore_path.display()
            );
        }
        let signer = alloy::signers::local::PrivateKeySigner::from_str(private_key)
            .context("invalid EVM private key")?;

        ensure_password_file(&self.password_file)?;
        let password = fs::read_to_string(&self.password_file)
            .with_context(|| format!("cannot read {}", self.password_file.display()))?;
        encrypt_evm_keystore(
            &self.keystore_path,
            password.trim(),
            signer.to_bytes().as_slice(),
        )?;

        Ok(signer.address().to_string())
    }
}

/// Sui account backed by a JSON keystore file.
pub struct SuiVault {
    pub keystore_path: PathBuf,
    /// Key alias inside the Sui keystore.
    pub alias: String,
}

impl ChainVault for SuiVault {
    fn bootstrap(&self) -> Result<()> {
        if self.keystore_path.exists() {
            return Ok(());
        }

        let mut rng = rand::thread_rng();
        let priv_key = sui_crypto::ed25519::Ed25519PrivateKey::generate(&mut rng);
        let keypair = sui_crypto::simple::SimpleKeypair::from(priv_key);

        write_sui_keystore(&self.keystore_path, &keypair)?;
        Ok(())
    }

    fn address(&self) -> Result<String> {
        let content =
            fs::read_to_string(&self.keystore_path).context("failed to read Sui keystore")?;

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

impl SuiVault {
    /// Imports an existing base64 keypair into the keystore instead of generating a throwaway key.
    /// Returns an error if the keystore already exists and force is not set, or if key decoding fails.
    pub fn import_key(&self, key: &str, force: bool) -> Result<String> {
        if self.keystore_path.exists() && !force {
            bail!(
                "Sui keystore already exists at {} (use --force to overwrite)",
                self.keystore_path.display()
            );
        }
        let keypair = sui_crypto::simple::SimpleKeypair::from_base64(key)
            .context("invalid Sui private key")?;

        write_sui_keystore(&self.keystore_path, &keypair)?;

        Ok(keypair.verifying_key().derive_address().to_string())
    }
}

// Appends a keypair to the Sui keystore JSON array.
fn write_sui_keystore(
    keystore_path: &Path,
    keypair: &sui_crypto::simple::SimpleKeypair,
) -> Result<()> {
    if let Some(parent) = keystore_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    let mut keys = Vec::new();
    if keystore_path.exists() {
        let content = fs::read_to_string(keystore_path).unwrap_or_default();
        if let Ok(existing) = serde_json::from_str::<Vec<String>>(&content) {
            keys = existing;
        }
    }
    keys.push(keypair.to_base64());
    fs::write(keystore_path, serde_json::to_string_pretty(&keys).unwrap())
        .context("failed to write Sui keystore")?;

    Ok(())
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
    let (_, name) = eth_keystore::new(dir, &mut rng, password, None)
        .context("failed to generate EVM keystore")?;

    move_evm_keystore_into_place(dir, &name, keystore_path)
}

fn encrypt_evm_keystore(keystore_path: &Path, password: &str, private_key: &[u8]) -> Result<()> {
    let dir = keystore_path
        .parent()
        .context("evm.keystore_path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;

    let mut rng = rand::thread_rng();
    let name = eth_keystore::encrypt_key(dir, &mut rng, private_key, password, None)
        .context("failed to encrypt EVM keystore")?;

    move_evm_keystore_into_place(dir, &name, keystore_path)
}

fn move_evm_keystore_into_place(dir: &Path, name: &str, keystore_path: &Path) -> Result<()> {
    let generated_path = dir.join(name);
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
