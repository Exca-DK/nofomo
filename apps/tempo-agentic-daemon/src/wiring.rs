use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tempo_agentic_chain::EvmChainClient;
use tempo_agentic_config::Config;
use tempo_agentic_domain::{ChainClient, Signer, TradeVenue};
use tempo_agentic_graph::GraphClient;
use tempo_agentic_orchestrator::ExecDeps;
use tempo_agentic_strategy::{LevelStore, OrderStore};
use tempo_agentic_trigger::{TokenResolver, TriggerDeps};
use tempo_agentic_uniswap::UniswapVenue;
use tempo_agentic_vault::EvmSigner;

pub struct Wiring {
    pub exec: ExecDeps,
    pub trigger: TriggerDeps,
}

/// Turns configuration into the implementations the two loops run on.
///
/// Returns an error if the graph client, a chain RPC URL, or the keystore cannot
/// be initialized.
pub fn build(
    config: &Config,
    allow_broadcast: bool,
    levels: Arc<dyn LevelStore>,
    orders: Arc<dyn OrderStore>,
) -> Result<Wiring> {
    let graph = GraphClient::new(&config.graph)?;

    let mut chains: HashMap<u64, Arc<dyn ChainClient>> = HashMap::new();
    for chain in &config.evm.chains {
        let client = EvmChainClient::new(&chain.rpc_url, chain.chain_id)
            .with_context(|| format!("cannot build a chain client for {}", chain.name))?;
        chains.insert(chain.chain_id, Arc::new(client));
    }

    // Decrypted once at startup rather than per transaction, so scrypt never runs
    // on the async runtime mid-trade.
    let signer: Arc<dyn Signer> = Arc::new(EvmSigner::from_keystore(
        Path::new(&config.evm.keystore_path),
        Path::new(&config.evm.password_file),
    )?);

    let venues: Vec<Arc<dyn TradeVenue>> = vec![Arc::new(UniswapVenue::new(
        &config.uniswap,
        &config.evm,
        signer.address().to_string(),
        chains.clone(),
        graph,
        config.max_slippage_bps,
    )?)];

    Ok(Wiring {
        exec: ExecDeps {
            venues: venues.clone(),
            chains,
            signer,
            allow_broadcast,
        },
        trigger: TriggerDeps {
            levels,
            orders,
            venues,
            resolver: TokenResolver::from_config(&config.evm),
        },
    })
}
