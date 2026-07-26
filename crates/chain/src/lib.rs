use std::str::FromStr;

use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::{TransactionInput, TransactionRequest};
use alloy::sol;
use alloy::sol_types::SolCall;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use tempo_agentic_domain::{ChainClient, ReceiptStatus, SignedTx, TxContext, is_native_token};

sol! {
    contract ERC20 {
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
    }
}

/// EVM node client for a single chain.
///
/// The provider is built without a wallet, so this type can never sign; signing
/// belongs to the [`tempo_agentic_domain::Signer`] port.
#[derive(Clone)]
pub struct EvmChainClient {
    provider: DynProvider,
    chain_id: u64,
}

impl EvmChainClient {
    /// Returns an error if the RPC URL cannot be parsed.
    pub fn new(rpc_url: &str, chain_id: u64) -> Result<Self> {
        let url = rpc_url
            .parse()
            .with_context(|| format!("invalid RPC URL {rpc_url}"))?;
        Ok(Self {
            provider: ProviderBuilder::new().connect_http(url).erased(),
            chain_id,
        })
    }

    async fn call_u256(&self, to: &str, data: Vec<u8>) -> Result<U256> {
        let to = Address::from_str(to).with_context(|| format!("invalid contract address {to}"))?;
        let request = TransactionRequest::default()
            .to(to)
            .input(TransactionInput::new(Bytes::from(data)));
        let raw = self
            .provider
            .call(request)
            .await
            .context("eth_call failed")?;
        if raw.len() < 32 {
            bail!("eth_call returned a short word");
        }
        Ok(U256::from_be_slice(&raw[raw.len() - 32..]))
    }
}

/// Nodes disagree on the wording, so match the known phrasings for "this
/// transaction is already accounted for". Treating them as success is what makes
/// a re-broadcast after a crash safe: the bytes are identical, so the same nonce
/// can only ever land once.
pub fn is_duplicate_submission(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "already known",
        "already imported",
        "nonce too low",
        "replacement transaction underpriced",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}

#[async_trait]
impl ChainClient for EvmChainClient {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    async fn tx_context(&self, from: &str) -> Result<TxContext> {
        let from =
            Address::from_str(from).with_context(|| format!("invalid from address {from}"))?;
        let nonce = self
            .provider
            .get_transaction_count(from)
            .await
            .context("eth_getTransactionCount failed")?;
        let fees = self
            .provider
            .estimate_eip1559_fees()
            .await
            .context("fee estimation failed")?;
        Ok(TxContext {
            chain_id: self.chain_id,
            nonce,
            max_fee_per_gas: fees.max_fee_per_gas,
            max_priority_fee_per_gas: fees.max_priority_fee_per_gas,
        })
    }

    async fn balance_of(&self, token: &str, owner: &str) -> Result<String> {
        let owner_addr =
            Address::from_str(owner).with_context(|| format!("invalid owner address {owner}"))?;
        if is_native_token(token) {
            let balance = self
                .provider
                .get_balance(owner_addr)
                .await
                .context("eth_getBalance failed")?;
            return Ok(balance.to_string());
        }
        let data = ERC20::balanceOfCall {
            account: owner_addr,
        }
        .abi_encode();
        Ok(self.call_u256(token, data).await?.to_string())
    }

    async fn allowance(&self, token: &str, owner: &str, spender: &str) -> Result<String> {
        if is_native_token(token) {
            return Ok(U256::MAX.to_string());
        }
        let owner_addr =
            Address::from_str(owner).with_context(|| format!("invalid owner address {owner}"))?;
        let spender_addr = Address::from_str(spender)
            .with_context(|| format!("invalid spender address {spender}"))?;
        let data = ERC20::allowanceCall {
            owner: owner_addr,
            spender: spender_addr,
        }
        .abi_encode();
        Ok(self.call_u256(token, data).await?.to_string())
    }

    async fn estimate_gas(&self, from: &str, to: &str, value: &str, data: &str) -> Result<u64> {
        let from =
            Address::from_str(from).with_context(|| format!("invalid from address {from}"))?;
        let to = Address::from_str(to).with_context(|| format!("invalid to address {to}"))?;
        let value = U256::from_str_radix(value, 10)
            .with_context(|| format!("value is not a decimal integer: {value}"))?;
        let data = Bytes::from_str(data).with_context(|| format!("invalid calldata: {data}"))?;
        let request = TransactionRequest::default()
            .from(from)
            .to(to)
            .value(value)
            .input(TransactionInput::new(data));
        self.provider
            .estimate_gas(request)
            .await
            .context("eth_estimateGas failed")
    }

    async fn broadcast(&self, signed: &SignedTx) -> Result<String> {
        let bytes = Bytes::from_str(&signed.raw).context("signed transaction is not valid hex")?;
        match self.provider.send_raw_transaction(&bytes).await {
            // The hash comes from the signed bytes, not the node, so it is known
            // even when the node reports the transaction as already seen.
            Ok(_pending) => Ok(signed.hash.clone()),
            Err(error) => {
                let message = error.to_string();
                if is_duplicate_submission(&message) {
                    tracing::info!(
                        tx_hash = %signed.hash,
                        %message,
                        "node reports the transaction is already submitted; treating as sent"
                    );
                    Ok(signed.hash.clone())
                } else {
                    bail!("eth_sendRawTransaction failed: {message}")
                }
            }
        }
    }

    async fn confirmation(&self, tx_hash: &str) -> Result<ReceiptStatus> {
        let hash = tx_hash
            .parse()
            .with_context(|| format!("invalid transaction hash {tx_hash}"))?;
        match self
            .provider
            .get_transaction_receipt(hash)
            .await
            .context("eth_getTransactionReceipt failed")?
        {
            None => Ok(ReceiptStatus::Pending),
            Some(receipt) if receipt.status() => Ok(ReceiptStatus::Success),
            Some(_) => Ok(ReceiptStatus::Reverted),
        }
    }
}
