use anyhow::Result;
use async_trait::async_trait;

use crate::{ReceiptStatus, SignedTx, TxContext};

/// Sentinel the venue APIs use for a chain's native currency.
pub const NATIVE_TOKEN_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

pub fn is_native_token(address: &str) -> bool {
    address.eq_ignore_ascii_case(NATIVE_TOKEN_ADDRESS)
}

/// Port for one EVM chain's node.
///
/// Split from [`crate::TradeVenue`] so a venue only builds transactions and
/// never talks to a node directly.
#[async_trait]
pub trait ChainClient: Send + Sync {
    fn chain_id(&self) -> u64;

    /// Reads the nonce and current fee market for `from`.
    async fn tx_context(&self, from: &str) -> Result<TxContext>;

    /// Token balance in raw base units as a decimal string. The zero address
    /// reads the native balance.
    async fn balance_of(&self, token: &str, owner: &str) -> Result<String>;

    /// ERC-20 allowance in raw base units as a decimal string.
    async fn allowance(&self, token: &str, owner: &str, spender: &str) -> Result<String>;

    /// Estimates the gas limit for a call, used when a venue's API omits one.
    async fn estimate_gas(&self, from: &str, to: &str, value: &str, data: &str) -> Result<u64>;

    /// Submits raw signed bytes and returns the transaction hash.
    ///
    /// Re-sending an already-submitted transaction must succeed, so a retry
    /// after a crash does not turn a landed transaction into an error.
    async fn broadcast(&self, signed: &SignedTx) -> Result<String>;

    /// Looks up the receipt without waiting for it.
    async fn confirmation(&self, tx_hash: &str) -> Result<ReceiptStatus>;
}
