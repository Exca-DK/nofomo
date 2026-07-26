use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tempo_agentic_mcp::manifest_path;

use crate::admin_client::Endpoint;
use crate::dashboard::locate_database;

/// What an authoring agent gets: the two tools that store a rule, plus enough
/// reading to store a correct one. Monitoring and deleting are left out, so a
/// mistake there cannot come from this direction.
const AUTHORING_TOOLS: [&str; 5] = [
    "set_strategy",
    "set_level",
    "strategies",
    "levels",
    // Without it an agent cannot tell whether a rule it stores will trade for real.
    "status",
];

/// Builds the `mcp.servers.nofomo` entry for the daemon this config names.
///
/// # Errors
///
/// Returns an error if the daemon is not running, since its address and token
/// only exist while it is.
pub fn entry(config_path: &str) -> Result<Value> {
    let database = locate_database(Path::new(config_path))?;
    let manifest = manifest_path(&database);
    let endpoint = Endpoint::read(&manifest).with_context(|| {
        format!(
            "no daemon is publishing {}; start it with `run` first",
            manifest.display()
        )
    })?;
    Ok(json!({
        "url": endpoint.url.to_string(),
        "transport": "streamable-http",
        "headers": { "Authorization": format!("Bearer {}", endpoint.token) },
        "toolFilter": { "include": AUTHORING_TOOLS },
    }))
}

/// Merges the entry into an OpenClaw config, leaving everything else in place.
///
/// # Errors
///
/// Returns an error if the daemon is not running, if the existing file is not a
/// JSON object, or if it cannot be written.
pub fn install(config_path: &str, target: Option<PathBuf>) -> Result<PathBuf> {
    let entry = entry(config_path)?;
    let target = match target {
        Some(path) => path,
        None => default_config()?,
    };

    let mut document = match std::fs::read(&target) {
        Ok(raw) => serde_json::from_slice(&raw)
            .with_context(|| format!("{} is not valid JSON", target.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", target.display()));
        }
    };

    // Reach into mcp.servers without disturbing any other server or setting.
    let root = document
        .as_object_mut()
        .with_context(|| format!("{} does not hold a JSON object", target.display()))?;
    let mcp = root
        .entry("mcp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .with_context(|| {
            format!(
                "{} has an mcp section that is not an object",
                target.display()
            )
        })?;
    let servers = mcp
        .entry("servers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .with_context(|| {
            format!(
                "{} has an mcp.servers that is not an object",
                target.display()
            )
        })?;
    servers.insert("nofomo".to_string(), entry);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let mut body = serde_json::to_vec_pretty(&document)?;
    body.push(b'\n');
    std::fs::write(&target, body).with_context(|| format!("cannot write {}", target.display()))?;
    // The file now holds a bearer token, so keep it to its owner.
    restrict(&target)?;
    Ok(target)
}

fn default_config() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set, so pass the config path")?;
    Ok(PathBuf::from(home).join(".openclaw/openclaw.json"))
}

#[cfg(unix)]
fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path)
        .with_context(|| format!("cannot read permissions of {}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot restrict {}", path.display()))
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use tempo_agentic_mcp::manifest_path;

    use super::{AUTHORING_TOOLS, entry, install};

    fn fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let database = std::env::temp_dir().join(format!(
            "tempo-agentic-openclaw-{}-{name}.db",
            std::process::id()
        ));
        let config = database.with_extension("config.json");
        std::fs::write(&config, json!({ "state_db_path": database }).to_string()).unwrap();
        let manifest = manifest_path(&database);
        std::fs::write(
            &manifest,
            json!({ "url": "http://127.0.0.1:4242/", "token": "secret-token" }).to_string(),
        )
        .unwrap();
        let target = database.with_extension("openclaw.json");
        (config, manifest, target)
    }

    fn clean(paths: &[PathBuf]) {
        for path in paths {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn the_entry_carries_the_live_address_token_and_tool_filter() {
        let (config, manifest, target) = fixture("entry");

        let entry = entry(config.to_str().unwrap()).unwrap();

        assert_eq!(entry["url"], "http://127.0.0.1:4242/");
        assert_eq!(entry["transport"], "streamable-http");
        assert_eq!(entry["headers"]["Authorization"], "Bearer secret-token");
        assert_eq!(entry["toolFilter"]["include"], json!(AUTHORING_TOOLS));
        // Authoring must not be able to reach execution history or deletion.
        let included = entry["toolFilter"]["include"].as_array().unwrap().clone();
        for hidden in ["orders", "delete_level"] {
            assert!(!included.iter().any(|tool| tool == hidden), "{hidden}");
        }
        clean(&[config, manifest, target]);
    }

    #[test]
    fn installing_keeps_every_other_server_and_setting() {
        let (config, manifest, target) = fixture("merge");
        std::fs::write(
            &target,
            json!({
                "gateway": { "port": 1234 },
                "mcp": { "servers": { "docs": { "url": "http://example.invalid/" } } }
            })
            .to_string(),
        )
        .unwrap();

        install(config.to_str().unwrap(), Some(target.clone())).unwrap();

        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(written["gateway"]["port"], 1234);
        assert_eq!(
            written["mcp"]["servers"]["docs"]["url"],
            "http://example.invalid/"
        );
        assert_eq!(
            written["mcp"]["servers"]["nofomo"]["headers"]["Authorization"],
            "Bearer secret-token"
        );
        clean(&[config, manifest, target]);
    }

    #[test]
    fn installing_twice_replaces_rather_than_duplicates() {
        let (config, manifest, target) = fixture("twice");

        install(config.to_str().unwrap(), Some(target.clone())).unwrap();
        std::fs::write(
            &manifest,
            json!({ "url": "http://127.0.0.1:5353/", "token": "rotated" }).to_string(),
        )
        .unwrap();
        install(config.to_str().unwrap(), Some(target.clone())).unwrap();

        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        let servers = written["mcp"]["servers"].as_object().unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers["nofomo"]["url"], "http://127.0.0.1:5353/");
        clean(&[config, manifest, target]);
    }

    #[cfg(unix)]
    #[test]
    fn the_written_file_keeps_the_token_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let (config, manifest, target) = fixture("perms");
        install(config.to_str().unwrap(), Some(target.clone())).unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o} exposes a bearer token");
        clean(&[config, manifest, target]);
    }

    #[test]
    fn a_stopped_daemon_is_reported_rather_than_guessed() {
        let (config, manifest, target) = fixture("stopped");
        std::fs::remove_file(&manifest).unwrap();

        let error = entry(config.to_str().unwrap()).unwrap_err().to_string();

        assert!(error.contains("no daemon is publishing"), "{error}");
        clean(&[config, target]);
    }
}
