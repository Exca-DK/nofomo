use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};

use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, UniswapConfig, secret_from_env};
use tempo_agentic_domain::{
    ChainClient, ExecStep, ExecutionPlan, QuoteDraft, QuoteTradeRequest, TradeVenue, TxContext,
    UnsignedTx, is_native_token,
};
use tempo_agentic_graph::GraphClient;

/// EVM swap venue that quotes and builds transactions via the Uniswap API.
///
/// It never signs or broadcasts: node access goes through [`ChainClient`] and
/// signing through [`tempo_agentic_domain::Signer`].
#[derive(Clone)]
pub struct UniswapVenue {
    http: Client,
    api_url: String,
    api_key: String,
    evm: EvmConfig,
    chains: HashMap<u64, Arc<dyn ChainClient>>,
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
        chains: HashMap<u64, Arc<dyn ChainClient>>,
        graph: GraphClient,
        max_slippage_bps: u16,
    ) -> Result<Self> {
        Ok(Self {
            http: Client::new(),
            api_url: config.api_url.trim_end_matches('/').to_string(),
            api_key: secret_from_env(&config.api_key_env)?,
            evm: evm.clone(),
            chains,
            graph,
            max_slippage_bps,
        })
    }

    fn chain_client(&self, chain_id: u64) -> Result<&Arc<dyn ChainClient>> {
        self.chains
            .get(&chain_id)
            .with_context(|| format!("no chain client configured for chain {chain_id}"))
    }

    /// Fetches the `check_approval` response, which carries the optional cancel
    /// and approval transactions for an ERC-20 input.
    async fn check_approval(
        &self,
        chain_id: u64,
        input_token: &str,
        input_amount: &str,
    ) -> Result<Value> {
        self.api_post(
            "check_approval",
            &json!({
                "walletAddress": self.evm.wallet_address,
                "token": input_token,
                "amount": input_amount,
                "chainId": chain_id
            }),
        )
        .await
    }

    async fn candidate(&self, request: &QuoteTradeRequest, chain: &EvmChain) -> Result<Candidate> {
        let input = find_token(chain, &request.token_in)
            .with_context(|| format!("{} does not configure {}", chain.name, request.token_in))?;
        let output = find_token(chain, &request.token_out)
            .with_context(|| format!("{} does not configure {}", chain.name, request.token_out))?;
        let amount = tempo_agentic_domain::parse_units_string(&request.amount, input.decimals)?;

        let balance = self
            .chain_client(chain.chain_id)?
            .balance_of(&input.address, &self.evm.wallet_address)
            .await?;
        if compare_decimal_integers(&balance, &amount) == Ordering::Less {
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
                    "swapper": self.evm.wallet_address,
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
            &self.evm.wallet_address,
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

    /// Turns a validated Uniswap API transaction into a signable one.
    ///
    /// The API supplies the target, calldata and value; nonce and fees come from
    /// `ctx`. The gas limit is taken from the API when present and estimated
    /// against the node otherwise.
    async fn build_unsigned(
        &self,
        transaction: &Value,
        ctx: &TxContext,
        expected_to: &str,
        expected_value: &str,
    ) -> Result<UnsignedTx> {
        validate_transaction(
            transaction,
            &self.evm.wallet_address,
            ctx.chain_id,
            expected_to,
            expected_value,
        )?;
        let to = string_field(transaction, "to")?.to_string();
        let data = string_field(transaction, "data")?.to_string();
        let value = decimal_value(transaction)?;

        let gas_limit = match api_gas_limit(transaction)? {
            Some(gas_limit) => gas_limit,
            None => {
                self.chain_client(ctx.chain_id)?
                    .estimate_gas(&self.evm.wallet_address, &to, &value, &data)
                    .await?
            }
        };

        Ok(UnsignedTx {
            chain_id: ctx.chain_id,
            nonce: ctx.nonce,
            gas_limit,
            max_fee_per_gas: ctx.max_fee_per_gas,
            max_priority_fee_per_gas: ctx.max_priority_fee_per_gas,
            to,
            value,
            data,
        })
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

    async fn steps(&self, plan: &ExecutionPlan) -> Result<Vec<ExecStep>> {
        let ExecutionPlan::Uniswap {
            chain_id,
            input_token,
            input_amount,
            ..
        } = plan
        else {
            bail!("Uniswap received a DeepBook execution plan");
        };
        if is_native_token(input_token) {
            return Ok(vec![ExecStep::Swap]);
        }
        let approval = self
            .check_approval(*chain_id, input_token, input_amount)
            .await?;
        Ok(steps_from_check_approval(&approval))
    }

    async fn build(
        &self,
        plan: &ExecutionPlan,
        step: ExecStep,
        ctx: &TxContext,
    ) -> Result<UnsignedTx> {
        let ExecutionPlan::Uniswap {
            chain_id,
            input_token,
            input_amount,
            quote,
            ..
        } = plan
        else {
            bail!("Uniswap received a DeepBook execution plan");
        };
        if *chain_id != ctx.chain_id {
            bail!(
                "transaction context is for chain {} but the plan targets {chain_id}",
                ctx.chain_id
            );
        }

        match step {
            // Re-fetched rather than carried over from `steps` so a resumed
            // execution can rebuild this transaction from the plan alone.
            ExecStep::Cancel | ExecStep::Approval => {
                let approval = self
                    .check_approval(*chain_id, input_token, input_amount)
                    .await?;
                let field = step.as_str();
                let transaction = approval
                    .get(field)
                    .filter(|value| !value.is_null())
                    .with_context(|| format!("Uniswap no longer requires a {field} transaction"))?;
                validate_approval_calldata(transaction, PROXY_APPROVAL_ADDRESS)?;
                self.build_unsigned(transaction, ctx, input_token, "0")
                    .await
            }
            ExecStep::Swap => {
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
                self.build_unsigned(transaction, ctx, PROXY_APPROVAL_ADDRESS, expected_value)
                    .await
            }
        }
    }
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

/// Maps a `check_approval` response onto the steps it implies. The swap always
/// runs last; the allowance transactions only appear when Uniswap asks for them.
fn steps_from_check_approval(approval: &Value) -> Vec<ExecStep> {
    let mut steps = Vec::new();
    for (field, step) in [
        ("cancel", ExecStep::Cancel),
        ("approval", ExecStep::Approval),
    ] {
        if approval.get(field).is_some_and(|value| !value.is_null()) {
            steps.push(step);
        }
    }
    steps.push(ExecStep::Swap);
    steps
}

/// Reads the transaction's native value as a decimal string, accepting the
/// decimal or hex forms the API may use.
fn decimal_value(transaction: &Value) -> Result<String> {
    let raw = transaction
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or("0");
    if let Some(hex) = raw.strip_prefix("0x") {
        return hex_to_decimal(hex);
    }
    validate_decimal_integer(raw).context("transaction value is not a decimal integer")?;
    Ok(raw.to_string())
}

/// The gas limit the venue's API supplied, if any.
fn api_gas_limit(transaction: &Value) -> Result<Option<u64>> {
    for field in ["gasLimit", "gas"] {
        let Some(raw) = transaction.get(field) else {
            continue;
        };
        if let Some(number) = raw.as_u64() {
            return Ok(Some(number));
        }
        let Some(text) = raw.as_str() else { continue };
        let parsed = match text.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16),
            None => text.parse::<u64>(),
        };
        return parsed
            .map(Some)
            .with_context(|| format!("transaction {field} is not a u64: {text}"));
    }
    Ok(None)
}

fn hex_to_decimal(hex: &str) -> Result<String> {
    if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("expected a hex quantity");
    }
    let mut digits: Vec<u8> = vec![0];
    for byte in hex.bytes() {
        let mut carry = char::from(byte).to_digit(16).expect("checked hex digit");
        for digit in digits.iter_mut() {
            let product = u32::from(*digit) * 16 + carry;
            *digit = (product % 10) as u8;
            carry = product / 10;
        }
        while carry > 0 {
            digits.push((carry % 10) as u8);
            carry /= 10;
        }
    }
    let text: String = digits
        .iter()
        .rev()
        .map(|digit| char::from(b'0' + digit))
        .collect();
    let trimmed = text.trim_start_matches('0');
    Ok(if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    })
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

    use tempo_agentic_domain::ExecStep;

    use super::{
        PROXY_APPROVAL_ADDRESS, api_gas_limit, compare_scaled_amounts, decimal_value,
        hex_to_decimal, slippage_percent_json, steps_from_check_approval, validate_transaction,
    };

    #[test]
    fn converts_rpc_hex_without_uint256_truncation() {
        assert_eq!(hex_to_decimal("0100").unwrap(), "256");
        assert_eq!(hex_to_decimal("00ff").unwrap(), "255");
        assert_eq!(
            hex_to_decimal("10000000000000000").unwrap(),
            "18446744073709551616"
        );
        assert_eq!(hex_to_decimal("0").unwrap(), "0");
        assert!(hex_to_decimal("zz").is_err());
    }

    #[test]
    fn approval_response_decides_the_step_sequence() {
        assert_eq!(
            steps_from_check_approval(&json!({"cancel": {"to": "0x1"}, "approval": {"to": "0x2"}})),
            vec![ExecStep::Cancel, ExecStep::Approval, ExecStep::Swap]
        );
        assert_eq!(
            steps_from_check_approval(&json!({"approval": {"to": "0x2"}})),
            vec![ExecStep::Approval, ExecStep::Swap]
        );
        assert_eq!(
            steps_from_check_approval(&json!({"cancel": null, "approval": null})),
            vec![ExecStep::Swap]
        );
        assert_eq!(steps_from_check_approval(&json!({})), vec![ExecStep::Swap]);
    }

    #[test]
    fn reads_gas_limit_and_value_in_either_encoding() {
        assert_eq!(
            api_gas_limit(&json!({"gasLimit": "0x5208"})).unwrap(),
            Some(21_000)
        );
        assert_eq!(
            api_gas_limit(&json!({"gas": "21000"})).unwrap(),
            Some(21_000)
        );
        assert_eq!(api_gas_limit(&json!({"gas": 21000})).unwrap(), Some(21_000));
        assert_eq!(api_gas_limit(&json!({})).unwrap(), None);
        assert!(api_gas_limit(&json!({"gas": "not-a-number"})).is_err());

        assert_eq!(decimal_value(&json!({"value": "0x2a"})).unwrap(), "42");
        assert_eq!(decimal_value(&json!({"value": "42"})).unwrap(), "42");
        assert_eq!(decimal_value(&json!({})).unwrap(), "0");
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
