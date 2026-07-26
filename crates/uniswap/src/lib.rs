use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};

use tempo_agentic_config::{EvmChain, EvmConfig, EvmToken, UniswapConfig, secret_from_env};
use tempo_agentic_domain::{
    EvmNode, EvmTx, ExecStep, ExecutionPlan, QuoteDraft, QuoteTradeRequest, TradeVenue, TxContext,
    UnsignedTx, is_native_token,
};
use tempo_agentic_graph::GraphClient;

mod amount;
mod validate;

use amount::{
    api_gas_limit, compare_decimal_integers, compare_scaled_amounts, decimal_value,
    slippage_percent_json, validate_decimal_integer,
};
use validate::{string_field, validate_approval_calldata, validate_quote, validate_transaction};

/// Uniswap API venue that quotes and builds but never signs or broadcasts.
#[derive(Clone)]
pub struct UniswapVenue {
    http: Client,
    api_url: String,
    api_key: String,
    evm: EvmConfig,
    /// Signer's public address used for quotes and approvals.
    wallet_address: String,
    chains: HashMap<u64, Arc<dyn EvmNode>>,
    graph: GraphClient,
    max_slippage_bps: u16,
}

struct Candidate {
    raw_output: String,
    output_decimals: u8,
    draft: QuoteDraft,
}

/// The only contract this venue approves and sends swaps to. Uniswap returns it
/// as the approval spender on every chain, so a response naming anything else is
/// refused rather than signed. Re-derive it from the `spender` inside
/// `/check_approval`'s calldata if Uniswap ever moves it.
pub(crate) const PROXY_APPROVAL_ADDRESS: &str = "0x02E5be68D46DAc0B524905bfF209cf47EE6dB2a9";

impl UniswapVenue {
    pub fn new(
        config: &UniswapConfig,
        evm: &EvmConfig,
        wallet_address: String,
        chains: HashMap<u64, Arc<dyn EvmNode>>,
        graph: GraphClient,
        max_slippage_bps: u16,
    ) -> Result<Self> {
        Ok(Self {
            http: Client::new(),
            api_url: config.api_url.trim_end_matches('/').to_string(),
            api_key: secret_from_env(&config.api_key_env)?,
            evm: evm.clone(),
            wallet_address,
            chains,
            graph,
            max_slippage_bps,
        })
    }

    fn chain_client(&self, chain_id: u64) -> Result<&Arc<dyn EvmNode>> {
        self.chains
            .get(&chain_id)
            .with_context(|| format!("no chain client configured for chain {chain_id}"))
    }

    /// Fetches optional allowance reset and approval transactions.
    async fn check_approval(
        &self,
        chain_id: u64,
        input_token: &str,
        input_amount: &str,
    ) -> Result<Value> {
        self.api_post(
            "check_approval",
            &json!({
                "walletAddress": self.wallet_address,
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
            .balance_of(&input.address, &self.wallet_address)
            .await?;
        if compare_decimal_integers(&balance, &amount) == Ordering::Less {
            bail!(
                "{} has insufficient {} balance for {}",
                chain.name,
                request.token_in,
                request.amount
            );
        }

        // Skip liquidity research on chains without an indexed subgraph.
        let graph_guard = if chain.graph_subgraph_id.trim().is_empty() {
            format!(
                "graph guard skipped: no subgraph configured for {}",
                chain.name
            )
        } else {
            let research = self
                .graph
                .research(&request.token_in, &request.token_out, &[chain])
                .await?;
            if !research.guard_passed {
                bail!("{}", research.guard_reason);
            }
            research.guard_reason
        };

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
                graph_guard,
                plan: ExecutionPlan::Uniswap {
                    chain_name: chain.name.clone(),
                    chain_id: chain.chain_id,
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
            // No router version is pinned on purpose. Asking for one the chain has
            // no deployment of answers "no quotes available" for every pair, which
            // is indistinguishable from a market with no liquidity.
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

    /// Completes a validated API transaction with chain state.
    async fn build_unsigned(
        &self,
        transaction: &Value,
        ctx: &TxContext,
        expected_to: &str,
        expected_value: &str,
    ) -> Result<EvmTx> {
        let TxContext::Evm {
            chain_id,
            nonce,
            max_fee_per_gas,
            max_priority_fee_per_gas,
        } = ctx
        else {
            bail!("Uniswap needs EVM chain state, not another family's");
        };
        validate_transaction(
            transaction,
            &self.wallet_address,
            *chain_id,
            expected_to,
            expected_value,
        )?;
        let to = string_field(transaction, "to")?.to_string();
        let data = string_field(transaction, "data")?.to_string();
        let value = decimal_value(transaction)?;

        let gas_limit = match api_gas_limit(transaction)? {
            Some(gas_limit) => gas_limit,
            None => {
                self.chain_client(*chain_id)?
                    .estimate_gas(&self.wallet_address, &to, &value, &data)
                    .await?
            }
        };

        Ok(EvmTx {
            chain_id: *chain_id,
            nonce: *nonce,
            gas_limit,
            max_fee_per_gas: *max_fee_per_gas,
            max_priority_fee_per_gas: *max_priority_fee_per_gas,
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
        let TxContext::Evm {
            chain_id: ctx_chain_id,
            ..
        } = ctx
        else {
            bail!("Uniswap needs EVM chain state, not another family's");
        };
        if chain_id != ctx_chain_id {
            bail!(
                "transaction context is for chain {ctx_chain_id} but the plan targets {chain_id}"
            );
        }

        match step {
            // Re-fetch so a stored plan can resume without prior step state.
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
                    .map(UnsignedTx::Evm)
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
                    .map(UnsignedTx::Evm)
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

/// Maps approval requirements to ordered steps ending in the swap.
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

    use super::steps_from_check_approval;

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
}
