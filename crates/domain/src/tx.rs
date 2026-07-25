use serde::{Deserialize, Serialize};

/// Chain state needed to finalize a transaction, fetched once per execution step.
///
/// Kept separate from the transaction itself because the venue knows the
/// calldata but not the account's nonce or the current fee market.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxContext {
    pub chain_id: u64,
    pub nonce: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
}

/// An EIP-1559 transaction ready to be signed.
///
/// Fields are plain types rather than chain-library types so the domain stays
/// free of an EVM dependency; the signer converts them.
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

/// A signed transaction plus the hash it will have on chain.
///
/// The hash is derived locally from the signed bytes, so both fields can be
/// persisted before the transaction is ever sent. That is what lets a restarted
/// process tell whether it already broadcast something.
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
