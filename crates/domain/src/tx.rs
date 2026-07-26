use serde::{Deserialize, Serialize};

/// Chain state fetched to finalize one transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxContext {
    pub chain_id: u64,
    pub nonce: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
}

/// An EIP-1559 transaction ready to sign, using chain-independent types.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct UnsignedTx {
    pub chain_id: u64,
    pub nonce: u64,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    /// Recipient, 0x-prefixed.
    pub to: String,
    /// Native amount in decimal wei, matching how amounts travel elsewhere.
    pub value: String,
    /// Calldata, 0x-prefixed.
    pub data: String,
}

/// Signed bytes and their locally derived hash, persisted before broadcast.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct SignedTx {
    /// Raw EIP-2718 bytes, 0x-prefixed, ready for `eth_sendRawTransaction`.
    pub raw: String,
    pub hash: String,
}

/// Outcome of looking up a transaction receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptStatus {
    /// Not mined yet; check again later.
    Pending,
    Success,
    Reverted,
}
