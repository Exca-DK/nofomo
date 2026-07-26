use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretFileError {
    #[error("cannot create {path}: {source}")]
    CreateDir {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot set permissions on {path}: {source}")]
    Permissions {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot move {tmp} into place at {path}: {source}")]
    Rename {
        tmp: String,
        path: String,
        source: std::io::Error,
    },
    #[error("{path} must not be readable by group or others")]
    TooPermissive { path: String },
}

/// Writes through an owner-only temporary file and atomic rename.
pub fn write_atomic_secret(path: &Path, contents: &[u8]) -> Result<(), SecretFileError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SecretFileError::CreateDir {
            path: parent.display().to_string(),
            source,
        })?;
    }

    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = path.with_file_name(tmp_name);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    {
        use std::io::Write;
        let mut file = options
            .open(&tmp)
            .map_err(|source| SecretFileError::Write {
                path: tmp.display().to_string(),
                source,
            })?;
        file.write_all(contents)
            .map_err(|source| SecretFileError::Write {
                path: tmp.display().to_string(),
                source,
            })?;
        file.sync_all().map_err(|source| SecretFileError::Write {
            path: tmp.display().to_string(),
            source,
        })?;
    }

    secure_permissions(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|source| SecretFileError::Rename {
        tmp: tmp.display().to_string(),
        path: path.display().to_string(),
        source,
    })
}

/// Reads an owner-only key file.
pub fn read_secret(path: &Path) -> Result<String, SecretFileError> {
    check_private(path)?;
    std::fs::read_to_string(path)
        .map(|raw| raw.trim().to_string())
        .map_err(|source| SecretFileError::Read {
            path: path.display().to_string(),
            source,
        })
}

// Restricts a file to its owner.
fn secure_permissions(path: &Path) -> Result<(), SecretFileError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| SecretFileError::Permissions {
                path: path.display().to_string(),
                source,
            },
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn check_private(path: &Path) -> Result<(), SecretFileError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .map_err(|source| SecretFileError::Read {
            path: path.display().to_string(),
            source,
        })?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(SecretFileError::TooPermissive {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_path: &Path) -> Result<(), SecretFileError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tempo-agentic-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir.join(name)
    }

    #[test]
    fn a_written_secret_is_owner_only_and_leaves_no_temporary_behind() {
        let path = scratch("owner-only");
        write_atomic_secret(&path, b"0xdeadbeef").expect("write");

        assert_eq!(read_secret(&path).expect("read"), "0xdeadbeef");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.path() != path)
            .collect();
        assert!(leftovers.is_empty(), "the temporary file must be gone");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn reading_refuses_a_world_readable_key() {
        use std::os::unix::fs::PermissionsExt;

        let path = scratch("world-readable");
        std::fs::write(&path, "0xdeadbeef").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        assert!(matches!(
            read_secret(&path),
            Err(SecretFileError::TooPermissive { .. })
        ));
        let _ = std::fs::remove_file(&path);
    }
}
