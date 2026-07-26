use std::collections::HashMap;

use tempo_agentic_config::EvmConfig;
use tempo_agentic_price::PricePair;
use tempo_agentic_strategy::{Level, base_token};

/// Maps configured chain and token names to feed identifiers.
pub struct TokenResolver {
    chains: HashMap<String, ResolvedChain>,
}

struct ResolvedChain {
    chain_id: u64,
    /// Symbol to contract address, symbols lowercased for lookup.
    tokens: HashMap<String, String>,
}

impl TokenResolver {
    pub fn from_config(evm: &EvmConfig) -> Self {
        let chains = evm
            .chains
            .iter()
            .map(|chain| {
                let tokens = chain
                    .tokens
                    .iter()
                    .map(|(symbol, token)| (symbol.to_ascii_lowercase(), token.address.clone()))
                    .collect();
                (
                    chain.name.to_ascii_lowercase(),
                    ResolvedChain {
                        chain_id: chain.chain_id,
                        tokens,
                    },
                )
            })
            .collect();
        Self { chains }
    }

    /// Resolves the level's priced pair, or `None` if unconfigured.
    pub fn price_pair(&self, level: &Level) -> Option<PricePair> {
        let chain = self.chains.get(&level.chain.to_ascii_lowercase())?;
        let address = chain.tokens.get(&base_token(level).to_ascii_lowercase())?;
        Some(PricePair::new(chain.chain_id, address.clone()))
    }
}
