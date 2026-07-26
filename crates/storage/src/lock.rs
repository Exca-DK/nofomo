use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Process-lifetime exclusive database claim.
#[derive(Debug)]
pub struct LockFile {
    path: PathBuf,
}

impl LockFile {
    /// Claims `path`, reporting any existing owner's PID.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        // Atomic creation lets only one racing daemon win.
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                write!(file, "{}", std::process::id())
                    .with_context(|| format!("cannot write lock file {}", path.display()))?;
                Ok(Self { path })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = fs::read_to_string(&path).unwrap_or_default();
                bail!(
                    "another daemon holds {} (pid {}); stop it, or remove the file if it crashed",
                    path.display(),
                    holder.trim()
                )
            }
            Err(error) => {
                Err(error).with_context(|| format!("cannot create lock file {}", path.display()))
            }
        }
    }

    /// Where the lock for a database lives.
    pub fn path_for(database: &Path) -> PathBuf {
        let mut path = database.as_os_str().to_os_string();
        path.push(".lock");
        PathBuf::from(path)
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            tracing::warn!(path = %self.path.display(), %error, "cannot remove the lock file");
        }
    }
}
