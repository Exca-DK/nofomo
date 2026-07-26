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

/// Normalizes padded Sui addresses.
pub fn normalize_coin_type(coin_type: &str) -> String {
    let Some((address, rest)) = coin_type.split_once("::") else {
        return coin_type.to_string();
    };
    let trimmed = address.trim_start_matches("0x").trim_start_matches('0');
    // Preserve zero.
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
    format!("0x{trimmed}::{rest}")
}

/// Validates a fully qualified, lowercase-prefixed Sui coin type.
pub fn validate_coin_type(coin_type: &str) -> Result<()> {
    let parts: Vec<&str> = coin_type.split("::").collect();
    if parts.len() < 3 || !parts[0].starts_with("0x") || parts.iter().any(|part| part.is_empty()) {
        bail!("{coin_type} is not a fully-qualified Sui coin type");
    }
    Ok(())
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

    /// Asks the node to execute the transaction and throw the result away.
    ///
    /// Used when broadcasting is off, so a run that spends nothing still says
    /// whether the transaction would have worked.
    ///
    /// # Errors
    ///
    /// Returns an error only when the node cannot be reached. A transaction the
    /// node rejects is a [`DryRun::Failed`], not an error.
    async fn dry_run(&self, _signed: &SignedTx) -> Result<DryRun> {
        Ok(DryRun::Unsupported)
    }
}

/// What a node says about a transaction it was asked not to keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DryRun {
    Succeeded,
    /// The node executed it and it would have failed, with its reason.
    Failed(String),
    /// This chain client cannot simulate, so nothing was learned.
    Unsupported,
}

impl DryRun {
    /// One line for an order's failure reason.
    pub fn note(&self) -> String {
        match self {
            Self::Succeeded => "dry run succeeded".to_string(),
            Self::Failed(reason) => format!("dry run failed: {reason}"),
            Self::Unsupported => "not simulated on this chain".to_string(),
        }
    }
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

    #[test]
    fn padded_and_short_addresses_name_the_same_coin() {
        let short = super::normalize_coin_type("0x2::sui::SUI");
        let padded = super::normalize_coin_type(
            "0x0000000000000000000000000000000000000000000000000000000000000002::sui::SUI",
        );
        assert_eq!(short, padded);
        assert_eq!(short, "0x2::sui::SUI");
    }

    #[test]
    fn the_zero_address_survives_normalization() {
        assert_eq!(super::normalize_coin_type("0x0::a::B"), "0x0::a::B");
    }

    #[test]
    fn a_coin_type_must_be_fully_qualified_and_lowercase_prefixed() {
        assert!(super::validate_coin_type("0x2::sui::SUI").is_ok());
        assert!(super::validate_coin_type("0X2::SUI::SUI").is_err());
        assert!(super::validate_coin_type("0x2::sui").is_err());
        assert!(super::validate_coin_type("0x2::::SUI").is_err());
        assert!(super::validate_coin_type("SUI").is_err());
    }
}
