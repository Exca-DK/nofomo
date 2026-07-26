mod evm;
mod sui;

pub use evm::{EvmChainClient, is_duplicate_submission};
pub use sui::SuiChainClient;
