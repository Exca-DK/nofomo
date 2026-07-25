use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::{Value, json};

use tempo_agentic_config::{EvmChain, GraphConfig, secret_from_env};
use tempo_agentic_domain::{MarketObservation, MarketResearch};

/// Queries subgraph data via The Graph gateway.
#[derive(Clone)]
pub struct GraphClient {
    http: Client,
    gateway_url: String,
    api_key: String,
    min_pool_tvl_usd: String,
}

impl GraphClient {
    pub fn new(config: &GraphConfig) -> Result<Self> {
        Ok(Self {
            http: Client::new(),
            gateway_url: config.gateway_url.trim_end_matches('/').to_string(),
            api_key: secret_from_env(&config.api_key_env)?,
            min_pool_tvl_usd: config.min_pool_tvl_usd.clone(),
        })
    }

    /// Queries Uniswap pool metrics for token pairs across multiple EVM chains.
    pub async fn research(
        &self,
        token_in: &str,
        token_out: &str,
        chains: &[&EvmChain],
    ) -> Result<MarketResearch> {
        let mut observations = Vec::new();
        let mut errors = Vec::new();

        for chain in chains {
            // Chains without an indexed subgraph (e.g. Robinhood Chain) have no
            // pools to query, so report that rather than hit a malformed URL.
            if chain.graph_subgraph_id.trim().is_empty() {
                errors.push(format!("{} has no configured subgraph", chain.name));
                continue;
            }
            let Some(input) = token(chain, token_in) else {
                errors.push(format!("{} has no {token_in}", chain.name));
                continue;
            };
            let Some(output) = token(chain, token_out) else {
                errors.push(format!("{} has no {token_out}", chain.name));
                continue;
            };
            match self
                .query_chain(chain, &input.address, &output.address)
                .await
            {
                Ok(mut pools) => observations.append(&mut pools),
                Err(error) => errors.push(format!("{}: {error}", chain.name)),
            }
        }

        let liquid_pools = observations
            .iter()
            .filter(|pool| decimal_at_least(&pool.tvl_usd, &self.min_pool_tvl_usd))
            .count();
        let guard_passed = liquid_pools > 0;
        let guard_reason = if guard_passed {
            format!(
                "{liquid_pools} live Uniswap pool(s) meet the ${} TVL guard",
                self.min_pool_tvl_usd
            )
        } else if errors.is_empty() {
            format!(
                "no indexed Uniswap pool meets the ${} TVL guard",
                self.min_pool_tvl_usd
            )
        } else {
            format!("Graph guard failed: {}", errors.join("; "))
        };

        Ok(MarketResearch {
            pair: format!(
                "{}/{}",
                token_in.to_ascii_uppercase(),
                token_out.to_ascii_uppercase()
            ),
            observations,
            guard_passed,
            guard_reason,
        })
    }

    async fn query_chain(
        &self,
        chain: &EvmChain,
        token_in: &str,
        token_out: &str,
    ) -> Result<Vec<MarketObservation>> {
        const QUERY: &str = r#"
          query PairPools($tokenIn: String!, $tokenOut: String!) {
            forward: pools(
              first: 5,
              orderBy: totalValueLockedUSD,
              orderDirection: desc,
              where: { token0: $tokenIn, token1: $tokenOut }
            ) {
              id feeTier token0Price token1Price totalValueLockedUSD volumeUSD txCount
              token0 { symbol }
              token1 { symbol }
            }
            reverse: pools(
              first: 5,
              orderBy: totalValueLockedUSD,
              orderDirection: desc,
              where: { token0: $tokenOut, token1: $tokenIn }
            ) {
              id feeTier token0Price token1Price totalValueLockedUSD volumeUSD txCount
              token0 { symbol }
              token1 { symbol }
            }
          }
        "#;
        let url = format!(
            "{}/subgraphs/id/{}",
            self.gateway_url, chain.graph_subgraph_id
        );
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&json!({
                "query": QUERY,
                "variables": {
                    "tokenIn": token_in.to_ascii_lowercase(),
                    "tokenOut": token_out.to_ascii_lowercase()
                }
            }))
            .send()
            .await
            .context("The Graph request failed")?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .context("The Graph returned invalid JSON")?;
        if !status.is_success() {
            bail!("The Graph returned {status}: {}", compact_error(&body));
        }
        if let Some(errors) = body.get("errors") {
            bail!("GraphQL error: {}", compact_error(errors));
        }
        let data = body
            .get("data")
            .and_then(Value::as_object)
            .context("The Graph response has no data")?;
        let mut observations = Vec::new();
        for key in ["forward", "reverse"] {
            for pool in data
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                observations.push(parse_pool(&chain.name, pool)?);
            }
        }
        Ok(observations)
    }
}

fn decimal_at_least(value: &str, minimum: &str) -> bool {
    fn parts(value: &str) -> Option<(&str, &str)> {
        let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some((
            whole.trim_start_matches('0'),
            fraction.trim_end_matches('0'),
        ))
    }
    let Some((value_whole, value_fraction)) = parts(value) else {
        return false;
    };
    let Some((minimum_whole, minimum_fraction)) = parts(minimum) else {
        return false;
    };
    match value_whole.len().cmp(&minimum_whole.len()) {
        std::cmp::Ordering::Greater => return true,
        std::cmp::Ordering::Less => return false,
        std::cmp::Ordering::Equal => {}
    }
    match value_whole.cmp(minimum_whole) {
        std::cmp::Ordering::Greater => return true,
        std::cmp::Ordering::Less => return false,
        std::cmp::Ordering::Equal => {}
    }
    let width = value_fraction.len().max(minimum_fraction.len());
    format!("{value_fraction:0<width$}") >= format!("{minimum_fraction:0<width$}")
}

fn token<'a>(chain: &'a EvmChain, symbol: &str) -> Option<&'a tempo_agentic_config::EvmToken> {
    chain
        .tokens
        .iter()
        .find(|(configured, _)| configured.eq_ignore_ascii_case(symbol))
        .map(|(_, token)| token)
}

fn parse_pool(chain: &str, pool: &Value) -> Result<MarketObservation> {
    let field = |name: &str| {
        pool.get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .with_context(|| format!("pool is missing {name}"))
    };
    let symbol = |token: &str| {
        pool.pointer(&format!("/{token}/symbol"))
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string()
    };
    Ok(MarketObservation {
        chain: chain.to_string(),
        pool_id: field("id")?,
        protocol: format!("Uniswap v3 / {} bps fee tier", field("feeTier")?),
        token0: symbol("token0"),
        token1: symbol("token1"),
        token0_price: field("token0Price")?,
        token1_price: field("token1Price")?,
        tvl_usd: field("totalValueLockedUSD")?,
        volume_usd: field("volumeUSD")?,
        tx_count: field("txCount")?,
    })
}

fn compact_error(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 500 {
        format!("{}…", &text[..500])
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::decimal_at_least;

    #[test]
    fn compares_graph_decimals_without_float_math() {
        assert!(decimal_at_least("1000.0001", "1000"));
        assert!(decimal_at_least("01000.0", "1000.000"));
        assert!(!decimal_at_least("999.9999", "1000"));
        assert!(!decimal_at_least("1e9", "1000"));
    }
}
