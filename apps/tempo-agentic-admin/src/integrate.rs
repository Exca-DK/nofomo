use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

/// Result of an integration operation.
#[derive(Debug, PartialEq)]
pub enum IntegrateOutcome {
    Added,
    Updated,
    Removed,
    NothingToRemove,
}

fn openclaw_config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("OPENCLAW_HOME")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir).join("openclaw.json"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".openclaw").join("openclaw.json"))
}

/// Integrates or removes the tempo-agentic MCP server and skill in OpenClaw.
///
/// Returns an error if reading or writing the OpenClaw configuration fails.
pub async fn openclaw(config_path: &str, remove: bool) -> Result<()> {
    let Some(path) = openclaw_config_path() else {
        println!("no OpenClaw config found (OPENCLAW_HOME or HOME not set)");
        return Ok(());
    };

    if !path.exists() {
        println!("no OpenClaw config found at {}", path.display());
        return Ok(());
    }

    let raw = fs::read_to_string(&path).with_context(|| format!("failed to read {path:?}"))?;
    let mut root: Value =
        serde_json::from_str(&raw).with_context(|| format!("failed to parse {path:?} as JSON"))?;

    let outcome = if remove {
        remove_entry(&mut root)
    } else {
        upsert_entry(&mut root, config_path)?
    };

    if outcome != IntegrateOutcome::NothingToRemove {
        let formatted = serde_json::to_string_pretty(&root)?;
        fs::write(&path, formatted).with_context(|| format!("failed to write {path:?}"))?;
    }

    match outcome {
        IntegrateOutcome::Added => println!("added mcp.servers.tempo-agentic to openclaw.json"),
        IntegrateOutcome::Updated => println!("updated mcp.servers.tempo-agentic in openclaw.json"),
        IntegrateOutcome::Removed => {
            println!("removed mcp.servers.tempo-agentic from openclaw.json")
        }
        IntegrateOutcome::NothingToRemove => println!("mcp.servers.tempo-agentic was not present"),
    }
    if matches!(
        outcome,
        IntegrateOutcome::Added | IntegrateOutcome::Updated | IntegrateOutcome::Removed
    ) {
        println!("run `openclaw mcp reload` to pick up the change");
    }

    if let Err(e) = write_skill(remove) {
        eprintln!("warning: failed to manage openclaw skill: {e}");
    }

    Ok(())
}

fn write_skill(remove: bool) -> Result<()> {
    let home = std::env::var("HOME").ok().context("HOME not set")?;
    let skill_dir = PathBuf::from(home)
        .join(".openclaw")
        .join("skills")
        .join("tempo-agentic");

    if remove {
        if skill_dir.exists() {
            fs::remove_dir_all(&skill_dir).context("failed to remove skill dir")?;
        }
    } else {
        fs::create_dir_all(&skill_dir).context("failed to create skill dir")?;
        let skill_path = skill_dir.join("SKILL.md");
        let content = include_str!("../../../docs/SKILL.md");
        fs::write(&skill_path, content).context("failed to write SKILL.md")?;
    }
    Ok(())
}

fn upsert_entry(root: &mut Value, config_path: &str) -> Result<IntegrateOutcome> {
    let abs_config = fs::canonicalize(config_path)
        .unwrap_or_else(|_| PathBuf::from(config_path))
        .to_string_lossy()
        .to_string();

    let server = json!({
        "command": "tempo-agentic",
        "args": [],
        "env": {
            "TEMPO_AGENTIC_CONFIG": abs_config
        }
    });

    let obj = root.as_object_mut().context("root must be a JSON object")?;
    let mcp = obj
        .entry("mcp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("mcp must be an object")?;
    let servers = mcp
        .entry("servers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("mcp.servers must be an object")?;

    let existed = servers.contains_key("tempo-agentic");
    servers.insert("tempo-agentic".to_string(), server);

    if existed {
        Ok(IntegrateOutcome::Updated)
    } else {
        Ok(IntegrateOutcome::Added)
    }
}

fn remove_entry(root: &mut Value) -> IntegrateOutcome {
    let Some(obj) = root.as_object_mut() else {
        return IntegrateOutcome::NothingToRemove;
    };
    let Some(mcp) = obj.get_mut("mcp").and_then(Value::as_object_mut) else {
        return IntegrateOutcome::NothingToRemove;
    };
    let Some(servers) = mcp.get_mut("servers").and_then(Value::as_object_mut) else {
        return IntegrateOutcome::NothingToRemove;
    };
    if servers.remove("tempo-agentic").is_none() {
        return IntegrateOutcome::NothingToRemove;
    }
    if servers.is_empty() {
        mcp.remove("servers");
    }
    if mcp.is_empty() {
        obj.remove("mcp");
    }
    IntegrateOutcome::Removed
}
