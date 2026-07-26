use std::collections::HashMap;

use tempo_agentic_config::{EvmConfig, SuiConfig};
use tempo_agentic_domain::{ChainId, normalize_coin_type};
use tempo_agentic_price::PricePair;
use tempo_agentic_strategy::{Level, base_token};

pub const SUI_CHAIN_NAME: &str = "sui";

#[derive(Clone, Debug)]
pub struct RegisteredToken {
    pub chain: ChainId,
    pub chain_name: String,
    /// Venue identifier: symbol or Move type.
    pub id: String,
    pub price_ref: Option<PricePair>,
    pub decimals: u8,
}

/// Maps configured tokens to venue and feed identifiers.
#[derive(Default)]
pub struct TokenResolver {
    chains: HashMap<String, HashMap<String, RegisteredToken>>,
}

impl TokenResolver {
    pub fn from_config(evm: &EvmConfig, sui: &SuiConfig) -> Self {
        let mut chains: HashMap<String, HashMap<String, RegisteredToken>> = HashMap::new();

        for chain in &evm.chains {
            let tokens = chain
                .tokens
                .iter()
                .map(|(symbol, token)| {
                    (
                        lookup_key(symbol),
                        RegisteredToken {
                            chain: ChainId::Evm(chain.chain_id),
                            chain_name: chain.name.clone(),
                            id: symbol.clone(),
                            price_ref: Some(PricePair::new(chain.chain_id, token.address.clone())),
                            decimals: token.decimals,
                        },
                    )
                })
                .collect();
            chains.insert(chain.name.to_ascii_lowercase(), tokens);
        }

        if sui.enabled {
            let mut coins = HashMap::new();
            for (symbol, coin) in &sui.coins {
                let token = RegisteredToken {
                    chain: ChainId::Sui,
                    chain_name: SUI_CHAIN_NAME.to_string(),
                    id: normalize_coin_type(&coin.coin_type),
                    price_ref: coin.price_ref.as_ref().map(|reference| {
                        PricePair::new(reference.chain_id, reference.address.clone())
                    }),
                    decimals: coin.decimals,
                };
                // Index both symbol and Move type.
                coins.insert(lookup_key(&token.id), token.clone());
                coins.insert(lookup_key(symbol), token);
            }
            chains.insert(SUI_CHAIN_NAME.to_string(), coins);
        }

        Self { chains }
    }

    /// Looks up a symbol or Move type, ignoring case.
    pub fn token(&self, chain: &str, name: &str) -> Option<&RegisteredToken> {
        self.chains
            .get(&chain.to_ascii_lowercase())?
            .get(&lookup_key(name))
    }

    pub fn price_pair(&self, level: &Level) -> Option<PricePair> {
        self.token(&level.chain, base_token(level))?
            .price_ref
            .clone()
    }
}

// Canonicalize Move types; symbols pass through.
fn lookup_key(name: &str) -> String {
    normalize_coin_type(name).to_ascii_lowercase()
}
