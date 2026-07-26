use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::{ReceiptStatus, SignedTx, TxContext};

/// Sentinel the venue APIs use for a chain's native currency.
pub const NATIVE_TOKEN_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

pub fn is_native_token(address: &str) -> bool {
    address.eq_ignore_ascii_case(NATIVE_TOKEN_ADDRESS)
}

/// Chains sharing a key format and signing scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChainFamily {
    Evm,
    Sui,
}

impl ChainFamily {
    /// CAIP-2 namespace.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Evm => "eip155",
            Self::Sui => "sui",
        }
    }

    /// Resolves a CAIP-2 namespace or known chain name.
    pub fn resolve(name: &str) -> Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "eip155" | "evm" | "ethereum" | "base" | "arbitrum" | "unichain" | "robinhood" => {
                Ok(Self::Evm)
            }
            "sui" | "move" => Ok(Self::Sui),
            other => bail!("unknown chain family '{other}'"),
        }
    }
}

impl std::fmt::Display for ChainFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Chain bound to a client and order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChainId {
    Evm(u64),
    Sui,
}

impl ChainId {
    pub fn family(&self) -> ChainFamily {
        match self {
            Self::Evm(_) => ChainFamily::Evm,
            Self::Sui => ChainFamily::Sui,
        }
    }
}

impl std::fmt::Display for ChainId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Evm(chain_id) => write!(f, "eip155:{chain_id}"),
            Self::Sui => f.write_str("sui"),
        }
    }
}

/// Chain operations used by execution.
#[async_trait]
pub trait ChainClient: Send + Sync {
    fn chain(&self) -> ChainId;

    /// Reads state needed to finalize a transaction for `from`.
    async fn tx_context(&self, from: &str) -> Result<TxContext>;

    /// Broadcasts a signed transaction; identical rebroadcasts must succeed.
    async fn broadcast(&self, signed: &SignedTx) -> Result<String>;

    /// Looks up the receipt without waiting for it.
    async fn confirmation(&self, tx_hash: &str) -> Result<ReceiptStatus>;
}

/// EVM-only venue reads.
#[async_trait]
pub trait EvmNode: Send + Sync {
    fn chain_id(&self) -> u64;

    /// Raw token balance; the zero address selects the native currency.
    async fn balance_of(&self, token: &str, owner: &str) -> Result<String>;

    /// ERC-20 allowance in raw base units as a decimal string.
    async fn allowance(&self, token: &str, owner: &str, spender: &str) -> Result<String>;

    /// Estimates the gas limit for a call, used when a venue's API omits one.
    async fn estimate_gas(&self, from: &str, to: &str, value: &str, data: &str) -> Result<u64>;
}

#[cfg(test)]
mod tests {
    use super::{ChainFamily, ChainId};

    #[test]
    fn resolves_chain_names_onto_their_family() {
        assert_eq!(ChainFamily::resolve("base").unwrap(), ChainFamily::Evm);
        assert_eq!(ChainFamily::resolve("eip155").unwrap(), ChainFamily::Evm);
        assert_eq!(ChainFamily::resolve("SUI").unwrap(), ChainFamily::Sui);
        assert!(ChainFamily::resolve("dogecoin").is_err());
    }

    #[test]
    fn every_chain_reports_the_family_its_key_belongs_to() {
        assert_eq!(ChainId::Evm(8453).family(), ChainFamily::Evm);
        assert_eq!(ChainId::Sui.family(), ChainFamily::Sui);
    }
}
