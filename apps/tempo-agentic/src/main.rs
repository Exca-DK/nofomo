use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{ServiceExt, transport};
use tempo_agentic_agent::AgentService;
use tempo_agentic_config::Config;
use tempo_agentic_mcp::AgentHandler;
use tempo_agentic_storage::SqliteAuditStore;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let config_path =
        std::env::var("TEMPO_AGENTIC_CONFIG").unwrap_or_else(|_| "config.json".to_string());
    let config = Config::load(&config_path)?;
    let audit =
        Arc::new(SqliteAuditStore::open(&config.state_db_path, env!("CARGO_PKG_VERSION")).await?);
    let service = AgentService::new(config, audit)?;
    tracing::info!(config = %config_path, "starting tempo-agentic MCP server on stdio");
    AgentHandler::new(service)
        .serve(transport::stdio())
        .await
        .context("failed to start MCP server")?
        .waiting()
        .await
        .context("MCP server stopped with an error")?;
    Ok(())
}
