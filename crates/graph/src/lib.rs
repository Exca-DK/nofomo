use std::cmp::Ordering;
use std::time::Duration;

use alloy_primitives::U256;
use anyhow::{Context, Result, anyhow, bail};
use reqwest::Client;
use serde::Serialize;
use serde_json::{Value, json};

use tempo_agentic_config::{EvmChain, GraphConfig, secret_from_env};
use tempo_agentic_domain::{MarketObservation, MarketResearch};

const MARKET_HOURS: usize = 24;
const TICK_PAGE_SIZE: usize = 1_000;

#[derive(Clone, Debug, PartialEq, Serialize)]
/// Indexed market data ready for an observational chart.
pub struct MarketChart {
    pub indexed_at: Option<i64>,
    pub pool: MarketPool,
    pub prices: Vec<PriceCandle>,
    pub liquidity: Vec<LiquidityPoint>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
/// The highest-TVL pool selected for a market.
pub struct MarketPool {
    pub id: String,
    pub fee_tier: String,
    pub tvl_usd: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
/// One hourly USD candle returned by the subgraph.
pub struct PriceCandle {
    pub started_at: i64,
    pub open_usd: String,
    pub high_usd: String,
    pub low_usd: String,
    pub close_usd: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
/// Active pool liquidity at one USD price boundary.
pub struct LiquidityPoint {
    pub price_usd: String,
    pub active_liquidity: String,
}

struct PoolState {
    id: String,
    fee_tier: String,
    tvl_usd: String,
    liquidity: U256,
    tick: i32,
    pair_price: String,
    base_is_token0: bool,
}

struct Tick {
    index: i32,
    liquidity_net: String,
    pair_price: String,
}

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
            http: Client::builder().timeout(Duration::from_secs(10)).build()?,
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
            // An empty subgraph ID means this chain is not indexed.
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

    /// Returns a 24-hour USD price series and active liquidity for one EVM market.
    ///
    /// Financial values stay decimal strings. `None` means the indexed pair has no pool.
    pub async fn market_chart(
        &self,
        chain: &EvmChain,
        base_token: &str,
        quote_token: &str,
        level_prices_usd: &[String],
    ) -> Result<Option<MarketChart>> {
        if chain.graph_subgraph_id.trim().is_empty() {
            bail!("{} has no configured subgraph", chain.name);
        }
        let base = token(chain, base_token)
            .with_context(|| format!("{} has no {base_token}", chain.name))?;
        let quote = token(chain, quote_token)
            .with_context(|| format!("{} has no {quote_token}", chain.name))?;
        let data = self
            .market_state(chain, &base.address, &quote.address)
            .await?;
        if data
            .pointer("/_meta/hasIndexingErrors")
            .and_then(Value::as_bool)
            == Some(true)
        {
            bail!("The Graph reports indexing errors for {}", chain.name);
        }
        let Some(pool) = best_pool(&data, &base.address)? else {
            return Ok(None);
        };
        let mut prices = parse_prices(&data)?;
        prices.sort_by_key(|candle| candle.started_at);
        prices.truncate(MARKET_HOURS);

        let quote_usd = if quote.usd_peg {
            "1".to_string()
        } else {
            data.pointer("/quoteHours/0/close")
                .and_then(Value::as_str)
                .context("The Graph returned no USD price for the quote token")?
                .to_string()
        };
        let (minimum, maximum) =
            pair_price_bounds(&prices, level_prices_usd, &quote_usd, &pool.pair_price)?;
        let ticks = self
            .ticks(chain, &pool.id, pool.base_is_token0, &minimum, &maximum)
            .await?;
        let liquidity = liquidity_points(&pool, &ticks, &quote_usd)?;
        let indexed_at = data.pointer("/_meta/block/timestamp").and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.parse::<i64>().ok())
        });

        Ok(Some(MarketChart {
            indexed_at,
            pool: MarketPool {
                id: pool.id,
                fee_tier: pool.fee_tier,
                tvl_usd: pool.tvl_usd,
            },
            prices,
            liquidity,
        }))
    }

    async fn market_state(
        &self,
        chain: &EvmChain,
        base_token: &str,
        quote_token: &str,
    ) -> Result<Value> {
        const QUERY: &str = r#"
          query Market($base: String!, $quote: String!) {
            forward: pools(
              first: 1,
              orderBy: totalValueLockedUSD,
              orderDirection: desc,
              where: { token0: $base, token1: $quote }
            ) {
              id feeTier liquidity tick token0Price token1Price totalValueLockedUSD
              token0 { id }
              token1 { id }
            }
            reverse: pools(
              first: 1,
              orderBy: totalValueLockedUSD,
              orderDirection: desc,
              where: { token0: $quote, token1: $base }
            ) {
              id feeTier liquidity tick token0Price token1Price totalValueLockedUSD
              token0 { id }
              token1 { id }
            }
            baseHours: tokenHourDatas(
              first: 24,
              orderBy: periodStartUnix,
              orderDirection: desc,
              where: { token: $base }
            ) {
              periodStartUnix open high low close
            }
            quoteHours: tokenHourDatas(
              first: 1,
              orderBy: periodStartUnix,
              orderDirection: desc,
              where: { token: $quote }
            ) {
              close
            }
            _meta {
              block { timestamp }
              hasIndexingErrors
            }
          }
        "#;
        self.query(
            chain,
            QUERY,
            json!({
                "base": base_token.to_ascii_lowercase(),
                "quote": quote_token.to_ascii_lowercase(),
            }),
        )
        .await
    }

    async fn ticks(
        &self,
        chain: &EvmChain,
        pool: &str,
        base_is_token0: bool,
        minimum: &str,
        maximum: &str,
    ) -> Result<Vec<Tick>> {
        let price_field = if base_is_token0 { "price1" } else { "price0" };
        let query = format!(
            r#"
              query Ticks(
                $pool: Bytes!,
                $minimum: BigDecimal!,
                $maximum: BigDecimal!,
                $skip: Int!
              ) {{
                ticks(
                  first: {TICK_PAGE_SIZE},
                  skip: $skip,
                  orderBy: tickIdx,
                  orderDirection: asc,
                  where: {{
                    poolAddress: $pool,
                    liquidityNet_not: "0",
                    {price_field}_gte: $minimum,
                    {price_field}_lte: $maximum
                  }}
                ) {{
                  tickIdx liquidityNet price0 price1
                }}
              }}
            "#
        );
        let mut ticks = Vec::new();
        loop {
            let data = self
                .query(
                    chain,
                    &query,
                    json!({
                        "pool": pool.to_ascii_lowercase(),
                        "minimum": minimum,
                        "maximum": maximum,
                        "skip": ticks.len(),
                    }),
                )
                .await?;
            let page = data
                .get("ticks")
                .and_then(Value::as_array)
                .context("The Graph response has no ticks")?;
            for tick in page {
                ticks.push(Tick {
                    index: string_field(tick, "tickIdx")?
                        .parse()
                        .context("tickIdx is outside the Uniswap range")?,
                    liquidity_net: string_field(tick, "liquidityNet")?.to_string(),
                    pair_price: string_field(tick, price_field)?.to_string(),
                });
            }
            if page.len() < TICK_PAGE_SIZE {
                break;
            }
        }
        Ok(ticks)
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
        let data = self
            .query(
                chain,
                QUERY,
                json!({
                    "tokenIn": token_in.to_ascii_lowercase(),
                    "tokenOut": token_out.to_ascii_lowercase(),
                }),
            )
            .await?;
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

    async fn query(&self, chain: &EvmChain, query: &str, variables: Value) -> Result<Value> {
        let url = format!(
            "{}/subgraphs/id/{}",
            self.gateway_url, chain.graph_subgraph_id
        );
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&json!({ "query": query, "variables": variables }))
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
        let data = body.get("data").context("The Graph response has no data")?;
        Ok(data.clone())
    }
}

fn best_pool(data: &Value, base_token: &str) -> Result<Option<PoolState>> {
    let pools = ["forward", "reverse"]
        .into_iter()
        .filter_map(|key| data.get(key)?.as_array()?.first())
        .collect::<Vec<_>>();
    let Some(pool) = pools.into_iter().max_by(|left, right| {
        decimal_cmp(
            left.get("totalValueLockedUSD")
                .and_then(Value::as_str)
                .unwrap_or("0"),
            right
                .get("totalValueLockedUSD")
                .and_then(Value::as_str)
                .unwrap_or("0"),
        )
        .unwrap_or(Ordering::Equal)
    }) else {
        return Ok(None);
    };
    let token0 = pool
        .pointer("/token0/id")
        .and_then(Value::as_str)
        .context("pool is missing token0.id")?;
    let base_is_token0 = token0.eq_ignore_ascii_case(base_token);
    let pair_price = string_field(
        pool,
        if base_is_token0 {
            "token1Price"
        } else {
            "token0Price"
        },
    )?
    .to_string();
    Ok(Some(PoolState {
        id: string_field(pool, "id")?.to_string(),
        fee_tier: string_field(pool, "feeTier")?.to_string(),
        tvl_usd: string_field(pool, "totalValueLockedUSD")?.to_string(),
        liquidity: U256::from_str_radix(string_field(pool, "liquidity")?, 10)
            .context("pool liquidity is not an unsigned integer")?,
        tick: string_field(pool, "tick")?
            .parse()
            .context("pool tick is outside the Uniswap range")?,
        pair_price,
        base_is_token0,
    }))
}

fn parse_prices(data: &Value) -> Result<Vec<PriceCandle>> {
    data.get("baseHours")
        .and_then(Value::as_array)
        .context("The Graph response has no base token history")?
        .iter()
        .map(|hour| {
            Ok(PriceCandle {
                started_at: hour
                    .get("periodStartUnix")
                    .and_then(Value::as_i64)
                    .context("periodStartUnix is not an integer")?,
                open_usd: string_field(hour, "open")?.to_string(),
                high_usd: string_field(hour, "high")?.to_string(),
                low_usd: string_field(hour, "low")?.to_string(),
                close_usd: string_field(hour, "close")?.to_string(),
            })
        })
        .collect()
}

fn pair_price_bounds(
    prices: &[PriceCandle],
    levels: &[String],
    quote_usd: &str,
    current_pair_price: &str,
) -> Result<(String, String)> {
    let quote = positive_f64(quote_usd, "quote USD price")?;
    let current = positive_f64(current_pair_price, "pool price")?;
    // These padded values only limit the tick query; returned prices remain exact decimals.
    let mut minimum = current;
    let mut maximum = current;
    for value in prices
        .iter()
        .flat_map(|candle| [&candle.low_usd, &candle.high_usd])
        .chain(levels.iter())
    {
        let pair = positive_f64(value, "market price")? / quote;
        minimum = minimum.min(pair);
        maximum = maximum.max(pair);
    }
    Ok((graph_decimal(minimum * 0.95), graph_decimal(maximum * 1.05)))
}

fn liquidity_points(
    pool: &PoolState,
    ticks: &[Tick],
    quote_usd: &str,
) -> Result<Vec<LiquidityPoint>> {
    let mut below = Vec::new();
    let mut active = pool.liquidity;
    for tick in ticks.iter().rev().filter(|tick| tick.index <= pool.tick) {
        active = apply_liquidity(active, &tick.liquidity_net, false)?;
        below.push(LiquidityPoint {
            price_usd: decimal_product(&tick.pair_price, quote_usd)?,
            active_liquidity: active.to_string(),
        });
    }
    below.reverse();
    below.push(LiquidityPoint {
        price_usd: decimal_product(&pool.pair_price, quote_usd)?,
        active_liquidity: pool.liquidity.to_string(),
    });
    active = pool.liquidity;
    for tick in ticks.iter().filter(|tick| tick.index > pool.tick) {
        active = apply_liquidity(active, &tick.liquidity_net, true)?;
        below.push(LiquidityPoint {
            price_usd: decimal_product(&tick.pair_price, quote_usd)?,
            active_liquidity: active.to_string(),
        });
    }
    Ok(below)
}

fn apply_liquidity(active: U256, net: &str, upward: bool) -> Result<U256> {
    let (negative, magnitude) = match net.strip_prefix('-') {
        Some(value) => (true, value),
        None => (false, net),
    };
    let magnitude =
        U256::from_str_radix(magnitude, 10).context("liquidityNet is not a signed integer")?;
    if negative == upward {
        active
            .checked_sub(magnitude)
            .ok_or_else(|| anyhow!("liquidityNet would make active liquidity negative"))
    } else {
        active
            .checked_add(magnitude)
            .ok_or_else(|| anyhow!("active liquidity overflow"))
    }
}

fn decimal_product(left: &str, right: &str) -> Result<String> {
    let (left, left_scale) = decimal_digits(left)?;
    let (right, right_scale) = decimal_digits(right)?;
    let product = U256::from_str_radix(&left, 10)?
        .checked_mul(U256::from_str_radix(&right, 10)?)
        .ok_or_else(|| anyhow!("decimal product overflow"))?
        .to_string();
    let scale = left_scale + right_scale;
    if scale == 0 {
        return Ok(product);
    }
    let digits = format!("{product:0>width$}", width = scale + 1);
    let split = digits.len() - scale;
    let fraction = digits[split..].trim_end_matches('0');
    Ok(if fraction.is_empty() {
        digits[..split].to_string()
    } else {
        format!("{}.{}", &digits[..split], fraction)
    })
}

fn decimal_digits(value: &str) -> Result<(String, usize)> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("expected an unsigned decimal string");
    }
    Ok((format!("{whole}{fraction}"), fraction.len()))
}

fn positive_f64(value: &str, field: &str) -> Result<f64> {
    let value = value
        .parse::<f64>()
        .with_context(|| format!("{field} is not decimal"))?;
    if !value.is_finite() || value <= 0.0 {
        bail!("{field} must be positive");
    }
    Ok(value)
}

fn graph_decimal(value: f64) -> String {
    format!("{value:.18}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn string_field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("The Graph response is missing {name}"))
}

fn decimal_at_least(value: &str, minimum: &str) -> bool {
    decimal_cmp(value, minimum).is_some_and(|ordering| ordering != Ordering::Less)
}

fn decimal_cmp(left: &str, right: &str) -> Option<Ordering> {
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
    let (left_whole, left_fraction) = parts(left)?;
    let (right_whole, right_fraction) = parts(right)?;
    match left_whole.len().cmp(&right_whole.len()) {
        Ordering::Greater => return Some(Ordering::Greater),
        Ordering::Less => return Some(Ordering::Less),
        Ordering::Equal => {}
    }
    match left_whole.cmp(right_whole) {
        Ordering::Greater => return Some(Ordering::Greater),
        Ordering::Less => return Some(Ordering::Less),
        Ordering::Equal => {}
    }
    let width = left_fraction.len().max(right_fraction.len());
    Some(format!("{left_fraction:0<width$}").cmp(&format!("{right_fraction:0<width$}")))
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
    use std::collections::HashMap;

    use alloy_primitives::U256;
    use reqwest::Client;
    use serde_json::json;
    use tempo_agentic_config::{EvmChain, EvmToken};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::{
        GraphClient, PoolState, Tick, best_pool, decimal_at_least, decimal_product,
        liquidity_points,
    };

    #[test]
    fn compares_graph_decimals_without_float_math() {
        assert!(decimal_at_least("1000.0001", "1000"));
        assert!(decimal_at_least("01000.0", "1000.000"));
        assert!(!decimal_at_least("999.9999", "1000"));
        assert!(!decimal_at_least("1e9", "1000"));
    }

    #[test]
    fn selects_the_largest_pool_across_token_orderings() {
        let data = json!({
            "forward": [{
                "id": "small",
                "feeTier": "500",
                "liquidity": "100",
                "tick": "0",
                "token0Price": "0.5",
                "token1Price": "2",
                "totalValueLockedUSD": "999.99",
                "token0": { "id": "0xbase" }
            }],
            "reverse": [{
                "id": "large",
                "feeTier": "3000",
                "liquidity": "200",
                "tick": "0",
                "token0Price": "2",
                "token1Price": "0.5",
                "totalValueLockedUSD": "1000.01",
                "token0": { "id": "0xquote" }
            }]
        });

        let pool = best_pool(&data, "0xbase").unwrap().unwrap();

        assert_eq!(pool.id, "large");
        assert!(!pool.base_is_token0);
        assert_eq!(pool.pair_price, "2");
    }

    #[test]
    fn reconstructs_liquidity_on_both_sides_of_the_current_tick() {
        let pool = PoolState {
            id: "pool".into(),
            fee_tier: "500".into(),
            tvl_usd: "1000".into(),
            liquidity: U256::from(100u64),
            tick: 0,
            pair_price: "2".into(),
            base_is_token0: true,
        };
        let ticks = vec![
            Tick {
                index: -10,
                liquidity_net: "20".into(),
                pair_price: "1.5".into(),
            },
            Tick {
                index: 10,
                liquidity_net: "-30".into(),
                pair_price: "2.5".into(),
            },
        ];

        let points = liquidity_points(&pool, &ticks, "3").unwrap();

        assert_eq!(points[0].price_usd, "4.5");
        assert_eq!(points[0].active_liquidity, "80");
        assert_eq!(points[1].active_liquidity, "100");
        assert_eq!(points[2].price_usd, "7.5");
        assert_eq!(points[2].active_liquidity, "70");
    }

    #[test]
    fn multiplies_graph_decimals_without_float_math() {
        assert_eq!(decimal_product("3000.25", "0.9999").unwrap(), "2999.949975");
        assert_eq!(decimal_product("2", "3").unwrap(), "6");
    }

    #[tokio::test]
    async fn fetches_price_and_liquidity_from_the_graph_contract() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for body in [
                json!({
                    "data": {
                        "forward": [{
                            "id": "0xpool",
                            "feeTier": "500",
                            "liquidity": "100",
                            "tick": "0",
                            "token0Price": "0.5",
                            "token1Price": "2",
                            "totalValueLockedUSD": "1000",
                            "token0": { "id": "0xbase" },
                            "token1": { "id": "0xquote" }
                        }],
                        "reverse": [],
                        "baseHours": [{
                            "periodStartUnix": 1,
                            "open": "5.8",
                            "high": "6.2",
                            "low": "5.7",
                            "close": "6"
                        }],
                        "quoteHours": [],
                        "_meta": {
                            "block": { "timestamp": 2 },
                            "hasIndexingErrors": false
                        }
                    }
                }),
                json!({
                    "data": {
                        "ticks": [
                            {
                                "tickIdx": "-10",
                                "liquidityNet": "20",
                                "price0": "0.666666",
                                "price1": "1.5"
                            },
                            {
                                "tickIdx": "10",
                                "liquidityNet": "-30",
                                "price0": "0.4",
                                "price1": "2.5"
                            }
                        ]
                    }
                }),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 16_384];
                let count = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                assert!(request.contains("authorization: Bearer test-key"));
                let body = serde_json::to_vec(&body).unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
            }
        });
        let client = GraphClient {
            http: Client::new(),
            gateway_url: format!("http://{address}"),
            api_key: "test-key".into(),
            min_pool_tvl_usd: "1".into(),
        };
        let chain = EvmChain {
            name: "base".into(),
            chain_id: 8453,
            rpc_url: "http://localhost".into(),
            graph_subgraph_id: "subgraph".into(),
            tokens: HashMap::from([
                (
                    "WETH".into(),
                    EvmToken {
                        address: "0xbase".into(),
                        decimals: 18,
                        usd_peg: false,
                    },
                ),
                (
                    "USDC".into(),
                    EvmToken {
                        address: "0xquote".into(),
                        decimals: 6,
                        usd_peg: true,
                    },
                ),
            ]),
        };

        let chart = client
            .market_chart(&chain, "WETH", "USDC", &["5.9".into()])
            .await
            .unwrap()
            .unwrap();

        assert_eq!(chart.pool.id, "0xpool");
        assert_eq!(chart.indexed_at, Some(2));
        assert_eq!(chart.prices[0].close_usd, "6");
        assert_eq!(chart.liquidity.len(), 3);
        assert_eq!(chart.liquidity[0].active_liquidity, "80");
        server.await.unwrap();
    }
}
