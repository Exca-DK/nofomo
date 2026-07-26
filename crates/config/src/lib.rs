use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Application configuration loaded from JSON.
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_state_db_path")]
    pub state_db_path: String,
    #[serde(default = "default_quote_ttl_seconds")]
    pub quote_ttl_seconds: u64,
    #[serde(default = "default_max_slippage_bps")]
    pub max_slippage_bps: u16,
    /// DexPaprika stream URL; pairs come from `evm.chains`.
    #[serde(default = "default_dexpaprika_url")]
    pub dexpaprika_stream_url: String,
    pub uniswap: UniswapConfig,
    pub graph: GraphConfig,
    pub keys: KeysConfig,
    pub evm: EvmConfig,
    pub sui: SuiConfig,
}

/// Paths to owner-only raw key files.
#[derive(Clone, Debug, Deserialize)]
pub struct KeysConfig {
    /// Hex secp256k1 key shared by EVM chains.
    pub evm: String,
    /// Base64 ed25519 keypair, required when Sui is enabled.
    #[serde(default)]
    pub sui: Option<String>,
}

/// Configuration for Uniswap API integration.
#[derive(Clone, Debug, Deserialize)]
pub struct UniswapConfig {
    #[serde(default = "default_uniswap_url")]
    pub api_url: String,
    pub api_key_env: String,
}

/// Configuration for The Graph indexer gateway.
#[derive(Clone, Debug, Deserialize)]
pub struct GraphConfig {
    #[serde(default = "default_graph_url")]
    pub gateway_url: String,
    pub api_key_env: String,
    #[serde(default = "default_min_graph_tvl")]
    pub min_pool_tvl_usd: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EvmConfig {
    pub chains: Vec<EvmChain>,
}

/// Network configuration for a supported EVM chain.
#[derive(Clone, Debug, Deserialize)]
pub struct EvmChain {
    pub name: String,
    pub chain_id: u64,
    pub rpc_url: String,
    /// Uniswap subgraph ID; empty skips the liquidity guard.
    #[serde(default)]
    pub graph_subgraph_id: String,
    pub tokens: HashMap<String, EvmToken>,
}

/// EVM token contract metadata.
#[derive(Clone, Debug, Deserialize)]
pub struct EvmToken {
    pub address: String,
    pub decimals: u8,
}

/// Configuration for Sui blockchain integration.
#[derive(Clone, Debug, Deserialize)]
pub struct SuiConfig {
    pub enabled: bool,
    pub network: SuiNetwork,
    pub rpc_url: String,
    /// Ceiling on what one swap may spend on gas, in MIST.
    #[serde(default = "default_sui_gas_budget")]
    pub gas_budget: u64,
}

/// Sui network used to resolve venue package and object IDs.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuiNetwork {
    Testnet,
    Mainnet,
}

impl Config {
    /// Loads and validates JSON configuration.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let config = Self::load_unvalidated(path)?;
        config.validate()?;
        Ok(config)
    }

    /// Loads JSON without validation for commands that create required files.
    pub fn load_unvalidated(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("invalid JSON in {}", path.display()))
    }

    /// Validates configuration invariants.
    pub fn validate(&self) -> Result<()> {
        if !Path::new(&self.state_db_path).is_absolute() {
            bail!("state_db_path must be absolute");
        }
        if !(1..=3_600).contains(&self.quote_ttl_seconds) {
            bail!("quote_ttl_seconds must be between 1 and 3600");
        }
        if self.max_slippage_bps > 10_000 {
            bail!("max_slippage_bps must not exceed 10000");
        }
        validate_https_url(&self.uniswap.api_url).context("Uniswap API URL")?;
        validate_https_url(&self.graph.gateway_url).context("The Graph Gateway URL")?;
        validate_https_url(&self.dexpaprika_stream_url).context("DexPaprika stream URL")?;
        validate_env_name(&self.uniswap.api_key_env).context("Uniswap API key env name")?;
        validate_env_name(&self.graph.api_key_env).context("The Graph API key env name")?;
        validate_positive_decimal(&self.graph.min_pool_tvl_usd)
            .context("graph.min_pool_tvl_usd")?;
        if self.evm.chains.is_empty() {
            bail!("configure at least one EVM chain");
        }
        self.validate_keys()?;
        let mut chain_ids = HashSet::new();
        for chain in &self.evm.chains {
            if !is_supported_evm_chain(chain.chain_id) {
                bail!(
                    "unsupported chain {}: supported chains are Ethereum, Base, Arbitrum, Unichain and Robinhood Chain",
                    chain.chain_id
                );
            }
            if !chain_ids.insert(chain.chain_id) {
                bail!("duplicate EVM chain ID {}", chain.chain_id);
            }
            if chain.name.trim().is_empty() {
                bail!("EVM chain name must not be empty");
            }
            validate_http_url(&chain.rpc_url).with_context(|| format!("{} RPC URL", chain.name))?;
            // Empty IDs skip research for chains without an indexed subgraph.
            if !chain.graph_subgraph_id.is_empty()
                && chain.graph_subgraph_id.chars().any(char::is_whitespace)
            {
                bail!("{} has an invalid Graph subgraph ID", chain.name);
            }
            let mut symbols = HashSet::new();
            if chain.tokens.len() < 2 {
                bail!("{} must configure at least two tokens", chain.name);
            }
            for (symbol, token) in &chain.tokens {
                if !symbols.insert(symbol.to_ascii_uppercase()) {
                    bail!("{} configures token {symbol} more than once", chain.name);
                }
                if token.decimals > 38 {
                    bail!("{symbol} on {} has unsupported decimals", chain.name);
                }
                validate_hex_address(&token.address)
                    .with_context(|| format!("{symbol} address on {}", chain.name))?;
            }
        }
        if self.sui.enabled {
            validate_http_url(&self.sui.rpc_url).context("Sui RPC URL")?;
        }
        Ok(())
    }

    fn validate_keys(&self) -> Result<()> {
        let mut keys = vec![("keys.evm", self.keys.evm.as_str())];
        if self.sui.enabled {
            let sui = self
                .keys
                .sui
                .as_deref()
                .context("keys.sui is required while sui.enabled is true")?;
            keys.push(("keys.sui", sui));
        }

        let mut seen = HashSet::new();
        for (name, path) in keys {
            validate_key_file(path).context(name)?;
            if !seen.insert(path) {
                bail!("each chain family needs its own key file, but {path} is shared");
            }
        }
        Ok(())
    }
}

/// EVM chains supported by the Uniswap integration.
pub fn is_supported_evm_chain(chain_id: u64) -> bool {
    matches!(chain_id, 1 | 8453 | 42161 | 130 | 4663)
}

/// Reads a required environment variable.
pub fn secret_from_env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("missing required environment variable {name}"))
}

fn validate_hex_address(value: &str) -> Result<()> {
    let raw = value
        .strip_prefix("0x")
        .context("expected a 0x-prefixed hex address")?;
    if raw.len() != 40 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("expected a 20-byte 0x-prefixed hex address");
    }
    Ok(())
}

fn validate_http_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context("invalid URL")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("expected an http(s) URL with a host");
    }
    Ok(())
}

fn validate_https_url(value: &str) -> Result<()> {
    validate_http_url(value)?;
    if !value.starts_with("https://") {
        bail!("HTTPS is required for API credentials");
    }
    Ok(())
}

fn validate_env_name(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        bail!("expected an uppercase environment variable name");
    }
    Ok(())
}

fn validate_positive_decimal(value: &str) -> Result<()> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || value.bytes().all(|byte| matches!(byte, b'0' | b'.'))
    {
        bail!("expected a positive decimal string");
    }
    Ok(())
}

// Key parsing belongs to the vault; configuration checks file safety.
fn validate_key_file(path: &str) -> Result<()> {
    let path = Path::new(path);
    if !path.is_absolute() {
        bail!("must be an absolute path");
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("cannot read key file {}", path.display()))?;
    if contents.trim().is_empty() {
        bail!("key file {} is empty", path.display());
    }
    validate_private_permissions(path)
}

#[cfg(unix)]
fn validate_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        bail!("{} must not be readable by group or others", path.display());
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn default_uniswap_url() -> String {
    "https://trade-api.gateway.uniswap.org/v1".into()
}

fn default_quote_ttl_seconds() -> u64 {
    120
}

fn default_state_db_path() -> String {
    std::env::var("HOME")
        .map(|home| format!("{home}/.tempo-agentic/state.db"))
        .unwrap_or_else(|_| "/tmp/tempo-agentic.db".into())
}

fn default_dexpaprika_url() -> String {
    "https://streaming.dexpaprika.com".into()
}

fn default_graph_url() -> String {
    "https://gateway.thegraph.com/api".into()
}

fn default_min_graph_tvl() -> String {
    "1000".into()
}

fn default_max_slippage_bps() -> u16 {
    500
}

// Default: 0.1 SUI.
fn default_sui_gas_budget() -> u64 {
    100_000_000
}

#[cfg(test)]
mod tests {
    use super::{Config, is_supported_evm_chain, validate_hex_address};

    const WITHOUT_KEY: &str = r#"{
        "state_db_path": "/tmp/tempo-agentic-test/state.db",
        "uniswap": { "api_key_env": "UNISWAP_API_KEY" },
        "graph": { "api_key_env": "GRAPH_API_KEY" },
        "keys": { "evm": "/tmp/tempo-agentic-test/absent.key" },
        "evm": {
            "chains": [{
                "name": "base",
                "chain_id": 8453,
                "rpc_url": "https://example.invalid",
                "tokens": {
                    "USDC": { "address": "0x0000000000000000000000000000000000000001", "decimals": 6 },
                    "WETH": { "address": "0x0000000000000000000000000000000000000002", "decimals": 18 }
                }
            }]
        },
        "sui": { "enabled": false, "network": "testnet", "rpc_url": "https://example.invalid" }
    }"#;

    fn scratch_config(name: &str, body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tempo-agentic-config-{}-{name}.json",
            std::process::id()
        ));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn an_unvalidated_load_reads_a_config_whose_key_is_not_there_yet() {
        let path = scratch_config("unvalidated", WITHOUT_KEY);

        let parsed = Config::load_unvalidated(&path).expect("parsing must not need the key");
        assert_eq!(parsed.keys.evm, "/tmp/tempo-agentic-test/absent.key");

        let refused = Config::load(&path).expect_err("a checked load has to refuse it");
        assert!(
            format!("{refused:#}").contains("keys.evm"),
            "the key is the only thing wrong with it: {refused:#}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_key_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let key = std::env::temp_dir().join(format!(
            "tempo-agentic-config-{}-readable.key",
            std::process::id()
        ));
        std::fs::write(&key, "0xabc").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

        let path = scratch_config(
            "readable",
            &WITHOUT_KEY.replace(
                "/tmp/tempo-agentic-test/absent.key",
                &key.display().to_string(),
            ),
        );
        let refused = Config::load(&path).expect_err("a checked load has to refuse it");
        assert!(
            format!("{refused:#}").contains("group or others"),
            "permissions are the only thing wrong with it: {refused:#}"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&key);
    }

    #[test]
    fn enabling_sui_without_a_key_is_refused() {
        let path = scratch_config(
            "sui-without-key",
            &WITHOUT_KEY.replace(r#""enabled": false"#, r#""enabled": true"#),
        );

        let refused = Config::load(&path).expect_err("a checked load has to refuse it");
        assert!(format!("{refused:#}").contains("keys.sui"), "{refused:#}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validates_evm_address_shape() {
        assert!(validate_hex_address("0x0000000000000000000000000000000000000000").is_ok());
        assert!(validate_hex_address("0x1234").is_err());
    }

    #[test]
    fn supports_unichain_and_robinhood_alongside_the_originals() {
        for id in [1, 8453, 42161, 130, 4663] {
            assert!(is_supported_evm_chain(id), "chain {id} should be supported");
        }
        for id in [0, 10, 56, 137] {
            assert!(!is_supported_evm_chain(id), "chain {id} should be rejected");
        }
    }
}
