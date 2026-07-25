use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Exclusive claim on one database, held for as long as this value lives.
///
/// Two daemons on one database would quote the same levels side by side and
/// start a second order for every rule that fires, so only one may run.
#[derive(Debug)]
pub struct LockFile {
    path: PathBuf,
}

impl LockFile {
    /// Claims `path` for this process.
    ///
    /// Returns an error if another process holds it, naming the pid inside so an
    /// operator can tell a live daemon from a leftover. A crash that skips
    /// `Drop` leaves the file behind and it has to be removed by hand.
    pub fn acquire(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        // `create_new` is a single atomic syscall, so two daemons racing to start
        // cannot both win.
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
