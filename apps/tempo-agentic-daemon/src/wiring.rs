use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use tempo_agentic_cetus::CetusVenue;
use tempo_agentic_chain::{EvmChainClient, SuiChainClient};
use tempo_agentic_config::Config;
use tempo_agentic_domain::{ChainClient, ChainFamily, ChainId, EvmNode, Signer, TradeVenue};
use tempo_agentic_graph::GraphClient;
use tempo_agentic_orchestrator::ExecDeps;
use tempo_agentic_strategy::{LevelStore, OrderStore};
use tempo_agentic_trigger::{RuntimeStatus, TokenResolver, TriggerDeps};
use tempo_agentic_uniswap::UniswapVenue;

use crate::keystore;

pub struct Wiring {
    pub exec: ExecDeps,
    pub trigger: TriggerDeps,
}

/// Builds runtime dependencies from configuration.
pub fn build(
    config: &Config,
    graph: GraphClient,
    allow_broadcast: bool,
    levels: Arc<dyn LevelStore>,
    orders: Arc<dyn OrderStore>,
    tokens: Arc<TokenResolver>,
    runtime: Arc<RuntimeStatus>,
) -> Result<Wiring> {
    // EVM clients also serve venue reads.
    let mut chains: HashMap<ChainId, Arc<dyn ChainClient>> = HashMap::new();
    let mut evm_nodes: HashMap<u64, Arc<dyn EvmNode>> = HashMap::new();
    for chain in &config.evm.chains {
        let client = Arc::new(
            EvmChainClient::new(&chain.rpc_url, chain.chain_id)
                .with_context(|| format!("cannot build a chain client for {}", chain.name))?,
        );
        chains.insert(ChainId::Evm(chain.chain_id), client.clone());
        evm_nodes.insert(chain.chain_id, client);
    }

    // Load keys once; downstream code stays file-free.
    let signer: Arc<dyn Signer> = Arc::new(keystore::load_vault(config)?);

    let mut venues: Vec<Arc<dyn TradeVenue>> = vec![Arc::new(UniswapVenue::new(
        &config.uniswap,
        &config.evm,
        signer.address(ChainFamily::Evm)?.to_string(),
        evm_nodes,
        graph,
        config.max_slippage_bps,
    )?)];

    if config.sui.enabled {
        chains.insert(
            ChainId::Sui,
            Arc::new(
                SuiChainClient::new(&config.sui.rpc_url, config.sui.gas_budget)
                    .context("cannot build the Sui chain client")?,
            ),
        );
        venues.push(Arc::new(CetusVenue::new(&config.sui, signer.clone())?));
    }

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
            resolver: tokens,
            runtime,
            max_quote_deviation_bps: config.max_quote_deviation_bps,
        },
    })
}
