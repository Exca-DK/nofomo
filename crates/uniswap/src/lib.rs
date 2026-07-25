use std::cmp::Ordering;

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};
use std::str::FromStr;

use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, UniswapConfig, secret_from_env};
use tempo_agentic_domain::{
    ExecutionPlan, QuoteDraft, QuoteTradeRequest, TradeVenue, TransactionReference, VenueExecution,
};
use tempo_agentic_graph::GraphClient;

/// EVM swap venue executing trades via the Uniswap API and a local wallet.
#[derive(Clone)]
pub struct UniswapVenue {
    http: Client,
    api_url: String,
    api_key: String,
    evm: EvmConfig,
    wallet_address: String,
    graph: GraphClient,
    max_slippage_bps: u16,
}

struct Candidate {
    raw_output: String,
    output_decimals: u8,
    draft: QuoteDraft,
}

const PROXY_APPROVAL_ADDRESS: &str = "0x0000000085E102724e78eCd2F45DC9cA239Affad";

impl UniswapVenue {
    pub fn new(
        config: &UniswapConfig,
        evm: &EvmConfig,
        graph: GraphClient,
        max_slippage_bps: u16,
    ) -> Result<Self> {
        let wallet_address = derive_wallet_address(evm)?;
        Ok(Self {
            http: Client::new(),
            api_url: config.api_url.trim_end_matches('/').to_string(),
            api_key: secret_from_env(&config.api_key_env)?,
            evm: evm.clone(),
            wallet_address,
            graph,
            max_slippage_bps,
        })
    }

    async fn candidate(&self, request: &QuoteTradeRequest, chain: &EvmChain) -> Result<Candidate> {
        let input = find_token(chain, &request.token_in)
            .with_context(|| format!("{} does not configure {}", chain.name, request.token_in))?;
        let output = find_token(chain, &request.token_out)
            .with_context(|| format!("{} does not configure {}", chain.name, request.token_out))?;
        let amount = tempo_agentic_domain::parse_units_string(&request.amount, input.decimals)?;

        let balance = self
            .balance(chain, &input.address, &self.wallet_address)
            .await?;
        if !hex_is_at_least(&balance, &amount)? {
            bail!(
                "{} has insufficient {} balance for {}",
                chain.name,
                request.token_in,
                request.amount
            );
        }

        let research = self
            .graph
            .research(&request.token_in, &request.token_out, &[chain])
            .await?;
        if !research.guard_passed {
            bail!("{}", research.guard_reason);
        }

        let response = self
            .api_post(
                "quote",
                &json!({
                    "type": "EXACT_INPUT",
                    "amount": amount,
                    "tokenInChainId": chain.chain_id,
                    "tokenOutChainId": chain.chain_id,
                    "tokenIn": input.address,
                    "tokenOut": output.address,
                    "swapper": self.wallet_address,
                    "slippageTolerance": slippage_percent_json(request.slippage_bps)?,
                    "routingPreference": "BEST_PRICE",
                    "protocols": ["V2", "V3", "V4"]
                }),
            )
            .await?;
        let routing = response
            .get("routing")
            .and_then(Value::as_str)
            .context("Uniswap quote has no routing")?;
        if routing != "CLASSIC" {
            bail!(
                "rejected Uniswap route {routing}; tempo-agentic only executes same-chain AMM swaps"
            );
        }
        let quote = response
            .get("quote")
            .cloned()
            .context("Uniswap response has no quote")?;
        validate_quote(
            &quote,
            chain.chain_id,
            &self.wallet_address,
            &input.address,
            &output.address,
            &amount,
        )?;
        let raw_out = quote
            .pointer("/output/amount")
            .and_then(Value::as_str)
            .context("Uniswap quote has no output amount")?;
        validate_decimal_integer(raw_out).context("invalid Uniswap output amount")?;
        if raw_out.bytes().all(|byte| byte == b'0') {
            bail!("Uniswap quote returned zero output");
        }
        let minimum_out = quote
            .pointer("/output/minimumAmount")
            .and_then(Value::as_str)
            .context("Uniswap quote has no minimum output")?;
        validate_decimal_integer(minimum_out).context("invalid Uniswap minimum output")?;
        let local_minimum =
            tempo_agentic_domain::apply_slippage_string(raw_out, request.slippage_bps)?;
        if compare_decimal_integers(minimum_out, &local_minimum) == Ordering::Less {
            bail!("Uniswap minimum output is below the requested slippage floor");
        }

        Ok(Candidate {
            raw_output: raw_out.to_string(),
            output_decimals: output.decimals,
            draft: QuoteDraft {
                venue: "uniswap".into(),
                chain: chain.name.clone(),
                token_in: request.token_in.to_ascii_uppercase(),
                token_out: request.token_out.to_ascii_uppercase(),
                amount_in: request.amount.clone(),
                expected_amount_out: tempo_agentic_domain::format_units_string(
                    raw_out,
                    output.decimals,
                )?,
                minimum_amount_out: tempo_agentic_domain::format_units_string(
                    minimum_out,
                    output.decimals,
                )?,
                graph_guard: research.guard_reason,
                plan: ExecutionPlan::Uniswap {
                    chain_name: chain.name.clone(),
                    chain_id: chain.chain_id,
                    rpc_url: chain.rpc_url.clone(),
                    input_token: input.address.clone(),
                    input_amount: amount,
                    quote,
                },
            },
        })
    }

    async fn balance(&self, chain: &EvmChain, token: &str, owner: &str) -> Result<String> {
        let method;
        let params;
        if is_native_token(token) {
            method = "eth_getBalance";
            params = json!([owner, "latest"]);
        } else {
            method = "eth_call";
            let owner = owner.trim_start_matches("0x");
            let data = format!("0x70a08231000000000000000000000000{owner}");
            params = json!([{"to": token, "data": data}, "latest"]);
        }
        let response = self
            .http
            .post(&chain.rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params
            }))
            .send()
            .await
            .with_context(|| format!("{} RPC request failed", chain.name))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .with_context(|| format!("{} RPC returned invalid JSON", chain.name))?;
        if !status.is_success() || body.get("error").is_some() {
            bail!("{} RPC balance error: {}", chain.name, compact(&body));
        }
        body.get("result")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("RPC balance response has no result")
    }

    async fn api_post(&self, endpoint: &str, body: &Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{}/{}", self.api_url, endpoint))
            .header("x-api-key", &self.api_key)
            .header("x-universal-router-version", "2.0")
            .header("x-permit2-disabled", "true")
            .json(body)
            .send()
            .await
            .with_context(|| format!("Uniswap /{endpoint} request failed"))?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .with_context(|| format!("Uniswap /{endpoint} returned invalid JSON"))?;
        if !status.is_success() {
            bail!("Uniswap /{endpoint} returned {status}: {}", compact(&body));
        }
        Ok(body)
    }

    async fn send_transaction(
        &self,
        rpc_url: &str,
        chain_id: u64,
        transaction: &Value,
        expected_to: &str,
        expected_value: &str,
    ) -> Result<String> {
        validate_transaction(
            transaction,
            &self.wallet_address,
            chain_id,
            expected_to,
            expected_value,
        )?;
        let to = string_field(transaction, "to")?;
        let data = string_field(transaction, "data")?;
        let value = transaction
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("0");
        let to_addr = Address::from_str(to).context("invalid to address")?;
        let data_bytes = Bytes::from_str(data).context("invalid data")?;
        let value_u256 = U256::from_str_radix(value, 10)
            .or_else(|_| U256::from_str_radix(value.trim_start_matches("0x"), 16))
            .unwrap_or_default();

        let password = std::fs::read_to_string(&self.evm.password_file)
            .with_context(|| format!("cannot read password file {}", self.evm.password_file))?;
        let key = eth_keystore::decrypt_key(&self.evm.keystore_path, password.trim())
            .with_context(|| format!("cannot decrypt keystore {}", self.evm.keystore_path))?;
        let signer =
            PrivateKeySigner::from_slice(&key).context("invalid private key in keystore")?;
        let wallet = EthereumWallet::from(signer);

        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .connect(rpc_url)
            .await
            .with_context(|| format!("failed to connect to {}", rpc_url))?;

        let tx = TransactionRequest::default()
            .with_to(to_addr)
            .with_input(data_bytes)
            .with_value(value_u256)
            .with_chain_id(chain_id);

        let pending = provider
            .send_transaction(tx)
            .await
            .with_context(|| "failed to send transaction")?;

        let tx_hash = *pending.tx_hash();
        let receipt = pending
            .get_receipt()
            .await
            .with_context(|| "failed to get transaction receipt")?;

        if !receipt.status() {
            bail!("transaction reverted: {}", tx_hash);
        }

        Ok(format!("{:?}", tx_hash))
    }
}

#[async_trait]
impl TradeVenue for UniswapVenue {
    fn name(&self) -> &'static str {
        "uniswap"
    }

    async fn quote(&self, request: &QuoteTradeRequest) -> Result<QuoteDraft> {
        if request.token_in.eq_ignore_ascii_case(&request.token_out) {
            bail!("token_in and token_out must differ");
        }
        if request.slippage_bps > self.max_slippage_bps {
            bail!(
                "slippage_bps must not exceed configured maximum {}",
                self.max_slippage_bps
            );
        }
        for value in &request.chains {
            if !self.evm.chains.iter().any(|chain| {
                value.eq_ignore_ascii_case(&chain.name) || value == &chain.chain_id.to_string()
            }) {
                bail!("requested EVM chain {value} is not configured");
            }
        }
        let chains: Vec<&EvmChain> = self
            .evm
            .chains
            .iter()
            .filter(|chain| chain_requested(chain, &request.chains))
            .collect();
        if chains.is_empty() {
            bail!("none of the requested chains is configured");
        }
        let mut candidates = Vec::new();
        let mut failures = Vec::new();
        for chain in chains {
            match self.candidate(request, chain).await {
                Ok(candidate) => candidates.push(candidate),
                Err(error) => failures.push(format!("{}: {error}", chain.name)),
            }
        }
        candidates
            .into_iter()
            .max_by(|left, right| {
                compare_scaled_amounts(
                    &left.raw_output,
                    left.output_decimals,
                    &right.raw_output,
                    right.output_decimals,
                )
            })
            .map(|candidate| candidate.draft)
            .with_context(|| format!("no executable same-chain quote: {}", failures.join("; ")))
    }

    async fn execute(&self, plan: &ExecutionPlan) -> Result<VenueExecution> {
        let ExecutionPlan::Uniswap {
            chain_name,
            chain_id,
            rpc_url,
            input_token,
            input_amount,
            quote,
        } = plan
        else {
            bail!("Uniswap received a DeepBook execution plan");
        };

        let mut transactions = Vec::new();
        if !is_native_token(input_token) {
            let approval = self
                .api_post(
                    "check_approval",
                    &json!({
                        "walletAddress": self.wallet_address,
                        "token": input_token,
                        "amount": input_amount,
                        "chainId": chain_id
                    }),
                )
                .await?;
            for field in ["cancel", "approval"] {
                if let Some(transaction) = approval.get(field).filter(|value| !value.is_null()) {
                    validate_approval_calldata(transaction, PROXY_APPROVAL_ADDRESS)?;
                    transactions.push(TransactionReference {
                        kind: field.to_string(),
                        id: self
                            .send_transaction(rpc_url, *chain_id, transaction, input_token, "0")
                            .await?,
                    });
                }
            }
        }

        let swap = self
            .api_post(
                "swap",
                &json!({
                    "quote": quote,
                    "simulateTransaction": true,
                    "safetyMode": "SAFE"
                }),
            )
            .await?;
        let transaction = swap
            .get("swap")
            .context("Uniswap /swap has no transaction")?;
        let expected_value = if is_native_token(input_token) {
            input_amount.as_str()
        } else {
            "0"
        };
        transactions.push(TransactionReference {
            kind: "swap".into(),
            id: self
                .send_transaction(
                    rpc_url,
                    *chain_id,
                    transaction,
                    PROXY_APPROVAL_ADDRESS,
                    expected_value,
                )
                .await?,
        });
        Ok(VenueExecution {
            venue: "uniswap".into(),
            chain: chain_name.clone(),
            transactions,
        })
    }
}

fn derive_wallet_address(evm: &EvmConfig) -> Result<String> {
    let password = std::fs::read_to_string(&evm.password_file)
        .with_context(|| format!("cannot read password file {}", evm.password_file))?;
    let key = eth_keystore::decrypt_key(&evm.keystore_path, password.trim())
        .with_context(|| format!("cannot decrypt keystore {}", evm.keystore_path))?;
    let signer = PrivateKeySigner::from_slice(&key).context("invalid private key in keystore")?;
    Ok(signer.address().to_string())
}

fn find_token<'a>(chain: &'a EvmChain, symbol: &str) -> Option<&'a EvmToken> {
    chain
        .tokens
        .iter()
        .find(|(configured, _)| configured.eq_ignore_ascii_case(symbol))
        .map(|(_, token)| token)
}

fn chain_requested(chain: &EvmChain, requested: &[String]) -> bool {
    requested.is_empty()
        || requested.iter().any(|value| {
            value.eq_ignore_ascii_case(&chain.name) || value == &chain.chain_id.to_string()
        })
}

fn is_native_token(address: &str) -> bool {
    address.eq_ignore_ascii_case("0x0000000000000000000000000000000000000000")
}

fn hex_is_at_least(value: &str, minimum: &str) -> Result<bool> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("RPC balance is not a hex quantity");
    }
    validate_decimal_integer(minimum)?;
    let value = value.trim_start_matches('0');
    let minimum = decimal_to_hex(minimum)?;
    Ok(value.len() > minimum.len() || (value.len() == minimum.len() && value >= minimum.as_str()))
}

fn validate_transaction(
    transaction: &Value,
    wallet: &str,
    chain_id: u64,
    expected_to: &str,
    expected_value: &str,
) -> Result<()> {
    let from = string_field(transaction, "from")?;
    if !from.eq_ignore_ascii_case(wallet) {
        bail!("refusing transaction for unexpected sender {from}");
    }
    let actual_chain = transaction
        .get("chainId")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
        })
        .context("transaction has no numeric chainId")?;
    if actual_chain != chain_id {
        bail!("refusing transaction for chain {actual_chain}; expected {chain_id}");
    }
    let to = string_field(transaction, "to")?;
    validate_evm_address(to).context("transaction target")?;
    if !to.eq_ignore_ascii_case(expected_to) {
        bail!("refusing transaction for unexpected target {to}");
    }
    let data = string_field(transaction, "data")?;
    let calldata = data.strip_prefix("0x").unwrap_or("");
    if calldata.len() < 8
        || calldata.len() % 2 != 0
        || !calldata.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("refusing transaction with invalid calldata");
    }
    let value = transaction
        .get("value")
        .and_then(Value::as_str)
        .context("transaction has no value")?;
    if !same_quantity(value, expected_value)? {
        bail!("refusing transaction with unexpected native value {value}");
    }
    Ok(())
}

fn validate_quote(
    quote: &Value,
    chain_id: u64,
    wallet: &str,
    input_token: &str,
    output_token: &str,
    input_amount: &str,
) -> Result<()> {
    let quote_chain = quote
        .get("chainId")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse().ok()))
        })
        .context("Uniswap quote has no numeric chainId")?;
    if quote_chain != chain_id {
        bail!("Uniswap quote returned unexpected chain {quote_chain}");
    }
    for (pointer, expected, name) in [
        ("/swapper", wallet, "swapper"),
        ("/input/token", input_token, "input token"),
        ("/output/token", output_token, "output token"),
        ("/input/amount", input_amount, "input amount"),
    ] {
        let actual = quote
            .pointer(pointer)
            .and_then(Value::as_str)
            .with_context(|| format!("Uniswap quote has no {name}"))?;
        let matches = if name == "input amount" {
            same_quantity(actual, expected)?
        } else {
            actual.eq_ignore_ascii_case(expected)
        };
        if !matches {
            bail!("Uniswap quote returned unexpected {name}");
        }
    }
    if quote.get("tradeType").and_then(Value::as_str) != Some("EXACT_INPUT") {
        bail!("Uniswap quote is not exact-input");
    }
    Ok(())
}

fn validate_approval_calldata(transaction: &Value, expected_spender: &str) -> Result<()> {
    let data = string_field(transaction, "data")?;
    let data = data.strip_prefix("0x").unwrap_or("");
    if data.len() != 136 || !data.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("approval calldata has an invalid ABI length");
    }
    if !data[..8].eq_ignore_ascii_case("095ea7b3") {
        bail!("approval calldata does not call approve(address,uint256)");
    }
    let spender = &data[8 + 24..8 + 64];
    if !spender.eq_ignore_ascii_case(expected_spender.trim_start_matches("0x")) {
        bail!("approval calldata targets an unexpected spender");
    }
    Ok(())
}

fn validate_evm_address(value: &str) -> Result<()> {
    let raw = value.strip_prefix("0x").unwrap_or(value);
    if raw.len() != 40 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("expected a 20-byte hex address");
    }
    Ok(())
}

fn validate_decimal_integer(value: &str) -> Result<()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("expected an unsigned decimal integer");
    }
    Ok(())
}

fn same_quantity(left: &str, right: &str) -> Result<bool> {
    fn normalize(value: &str) -> Result<String> {
        if let Some(hex) = value.strip_prefix("0x") {
            if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("invalid hex quantity");
            }
            return Ok(hex.trim_start_matches('0').to_ascii_lowercase());
        }
        validate_decimal_integer(value)?;
        decimal_to_hex(value)
    }
    Ok(normalize(left)? == normalize(right)?)
}

fn decimal_to_hex(value: &str) -> Result<String> {
    validate_decimal_integer(value)?;
    let mut digits = value.bytes().map(|byte| byte - b'0').collect::<Vec<_>>();
    let mut result = Vec::new();
    while digits.iter().any(|digit| *digit != 0) {
        let mut carry = 0_u16;
        let mut quotient = Vec::with_capacity(digits.len());
        for digit in digits {
            let current = carry * 10 + u16::from(digit);
            if !quotient.is_empty() || current / 16 != 0 {
                quotient.push((current / 16) as u8);
            }
            carry = current % 16;
        }
        result.push(b"0123456789abcdef"[usize::from(carry)]);
        digits = quotient;
    }
    if result.is_empty() {
        return Ok(String::new());
    }
    result.reverse();
    Ok(String::from_utf8(result).expect("hex conversion emits ASCII"))
}

fn compare_decimal_integers(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn compare_scaled_amounts(
    left: &str,
    left_decimals: u8,
    right: &str,
    right_decimals: u8,
) -> Ordering {
    let decimals = left_decimals.max(right_decimals);
    let left = format!(
        "{}{}",
        left.trim_start_matches('0'),
        "0".repeat(usize::from(decimals - left_decimals))
    );
    let right = format!(
        "{}{}",
        right.trim_start_matches('0'),
        "0".repeat(usize::from(decimals - right_decimals))
    );
    compare_decimal_integers(&left, &right)
}

fn slippage_percent_json(slippage_bps: u16) -> Result<Value> {
    let whole = slippage_bps / 100;
    let fraction = slippage_bps % 100;
    let value = if fraction == 0 {
        whole.to_string()
    } else if fraction.is_multiple_of(10) {
        format!("{whole}.{}", fraction / 10)
    } else {
        format!("{whole}.{fraction:02}")
    };
    serde_json::from_str(&value).context("cannot encode slippage as JSON number")
}

fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("transaction has no {field}"))
}

fn compact(value: &Value) -> String {
    let value = value.to_string();
    if value.len() > 500 {
        format!("{}…", &value[..500])
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        PROXY_APPROVAL_ADDRESS, compare_scaled_amounts, hex_is_at_least, slippage_percent_json,
        validate_transaction,
    };

    #[test]
    fn compares_rpc_balance_without_uint256_truncation() {
        assert!(hex_is_at_least("0x0100", "255").unwrap());
        assert!(!hex_is_at_least("0x00ff", "256").unwrap());
        assert!(hex_is_at_least("0x10000000000000000", "18446744073709551616").unwrap());
    }

    #[test]
    fn rejects_wrong_transaction_chain() {
        let transaction = json!({
            "from": "0x1111111111111111111111111111111111111111",
            "to": "0x2222222222222222222222222222222222222222",
            "data": "0x12345678aa",
            "chainId": 8453,
            "value": "0"
        });
        assert!(
            validate_transaction(
                &transaction,
                "0x1111111111111111111111111111111111111111",
                1,
                "0x2222222222222222222222222222222222222222",
                "0"
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_wrong_transaction_sender_and_target() {
        let transaction = json!({
            "from": "0x9999999999999999999999999999999999999999",
            "to": PROXY_APPROVAL_ADDRESS,
            "data": "0x12345678",
            "chainId": 1,
            "value": "0"
        });
        assert!(
            validate_transaction(
                &transaction,
                "0x1111111111111111111111111111111111111111",
                1,
                PROXY_APPROVAL_ADDRESS,
                "0"
            )
            .is_err()
        );
    }

    #[test]
    fn ranks_scaled_outputs_exactly_and_encodes_bps() {
        assert!(compare_scaled_amounts("1000001", 6, "1000000000000000000", 18).is_gt());
        assert_eq!(slippage_percent_json(1).unwrap(), json!(0.01));
        assert_eq!(slippage_percent_json(50).unwrap(), json!(0.5));
    }
}
