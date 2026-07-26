use anyhow::{Context, Result, bail};
use serde_json::Value;
use sui_rpc::Client;
use sui_rpc::field::{FieldMask, FieldMaskUtil};
use sui_rpc::proto::sui::rpc::v2::{
    GetCoinInfoRequest, GetObjectRequest, ListDynamicFieldsRequest,
};

use crate::swap_math::{PoolState, TickData};

/// Fetches a coin type's decimal precision from its on-chain metadata.
pub async fn fetch_coin_decimals(client: &mut Client, coin_type: &str) -> Result<u8> {
    let mut request = GetCoinInfoRequest::default();
    request.coin_type = Some(coin_type.to_string());
    let response = client
        .state_client()
        .get_coin_info(request)
        .await
        .with_context(|| format!("failed to fetch coin metadata for {coin_type}"))?
        .into_inner();
    response
        .metadata
        .and_then(|metadata| metadata.decimals)
        .with_context(|| format!("{coin_type} has no published CoinMetadata"))?
        .try_into()
        .context("coin decimals out of u8 range")
}

/// Discovered pool metadata for a coin pair.
#[derive(Clone, Debug)]
pub struct DiscoveredPool {
    pub pool_id: String,
    pub coin_type_a: String,
    pub coin_type_b: String,
}

fn json_field<'a>(value: &'a Value, field: &str) -> Result<&'a Value> {
    value
        .get(field)
        .with_context(|| format!("pool object JSON has no `{field}` field"))
}

fn json_u128(value: &Value, field: &str) -> Result<u128> {
    let raw = json_field(value, field)?;
    raw.as_str()
        .and_then(|s| s.parse::<u128>().ok())
        .or_else(|| raw.as_u64().map(u128::from))
        .or_else(|| raw.as_f64().map(|f| f as u128))
        .with_context(|| format!("`{field}` is not a u128-compatible number"))
}

/// Parses Cetus Move `I32` bits as a two's-complement tick index.
fn json_signed_tick_index(value: &Value, field: &str) -> Result<i32> {
    let raw = json_field(value, field)?;
    let bits_holder = raw.get("bits").unwrap_or(raw);
    let bits: u32 = bits_holder
        .as_str()
        .and_then(|s| s.parse::<u32>().ok())
        .or_else(|| bits_holder.as_u64().and_then(|n| u32::try_from(n).ok()))
        .or_else(|| bits_holder.as_f64().map(|f| f as u32))
        .with_context(|| format!("`{field}` is not a u32-compatible signed tick index"))?;
    Ok(bits as i32)
}

fn json_string(value: &Value, field: &str) -> Result<String> {
    json_field(value, field)?
        .as_str()
        .map(str::to_string)
        .with_context(|| format!("`{field}` is not a string"))
}

/// Fetches a Cetus `Pool<A, B>` object's live trading state.
pub async fn fetch_pool_state(client: &mut Client, pool_id: &str) -> Result<(PoolState, String)> {
    let mut request = GetObjectRequest::default();
    request.object_id = Some(pool_id.to_string());
    request.read_mask = Some(FieldMask::from_paths(["json", "object_type"]));
    let response = client
        .ledger_client()
        .get_object(request)
        .await
        .with_context(|| format!("failed to fetch Cetus pool {pool_id}"))?
        .into_inner();
    let object = response
        .object
        .with_context(|| format!("Cetus pool {pool_id} does not exist"))?;
    let object_type = object
        .object_type
        .with_context(|| format!("Cetus pool {pool_id} has no Move type"))?;
    if !object_type.contains("::pool::Pool<") {
        bail!("{pool_id} is not a Cetus CLMM pool object (type: {object_type})");
    }
    let json = object
        .json
        .with_context(|| format!("Cetus pool {pool_id} response had no JSON rendering"))?;
    let fields = prost_types_value_to_json(&json);

    if fields
        .get("is_pause")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        bail!("Cetus pool {pool_id} is paused");
    }

    let state = PoolState {
        current_sqrt_price: json_u128(&fields, "current_sqrt_price")?,
        current_tick_index: json_signed_tick_index(&fields, "current_tick_index")?,
        liquidity: json_u128(&fields, "liquidity")?,
        fee_rate: json_u128(&fields, "fee_rate")?
            .try_into()
            .context("fee_rate out of u64 range")?,
    };
    let tick_manager = json_field(&fields, "tick_manager")?;
    let ticks = json_field(tick_manager, "ticks")?;
    let ticks_list_id = json_string(ticks, "id")?;
    Ok((state, ticks_list_id))
}

/// Lists initialized ticks from the pool's tick skip-list.
pub async fn fetch_ticks(
    client: &mut Client,
    ticks_list_id: &str,
    cap: usize,
) -> Result<Vec<TickData>> {
    let mut ticks = Vec::new();
    let mut page_token = None;
    loop {
        let mut request = ListDynamicFieldsRequest::default();
        request.parent = Some(ticks_list_id.to_string());
        request.page_size = Some(1000);
        request.page_token = page_token;
        request.read_mask = Some(FieldMask::from_paths(["field_id", "field_object.json"]));
        let response = client
            .state_client()
            .list_dynamic_fields(request)
            .await
            .with_context(|| format!("failed to list ticks under {ticks_list_id}"))?
            .into_inner();

        for field in response.dynamic_fields {
            let object = field
                .field_object
                .with_context(|| format!("tick node under {ticks_list_id} has no object"))?;
            let json = object.json.with_context(|| {
                format!("tick node under {ticks_list_id} had no JSON rendering")
            })?;
            let fields = prost_types_value_to_json(&json);
            let node = json_field(&fields, "value")?;
            let tick_value = json_field(node, "value")?;
            ticks.push(TickData {
                index: json_signed_tick_index(tick_value, "index")?,
                sqrt_price: json_u128(tick_value, "sqrt_price")?,
                liquidity_net: parse_signed_liquidity_net(tick_value)?,
            });
            if ticks.len() > cap {
                bail!(
                    "Cetus pool has more than {cap} initialized ticks; refusing to quote against a partial tick set"
                );
            }
        }

        page_token = response.next_page_token;
        if page_token.is_none() {
            break;
        }
    }

    ticks.sort_by_key(|tick| tick.index);
    Ok(ticks)
}

fn parse_signed_liquidity_net(tick_value: &Value) -> Result<i128> {
    let raw = tick_value
        .get("liquidity_net")
        .context("tick has no liquidity_net field")?;
    let bits = raw
        .get("bits")
        .unwrap_or(raw)
        .as_str()
        .and_then(|s| s.parse::<u128>().ok())
        .with_context(|| "liquidity_net.bits is not a u128-compatible number")?;
    Ok(bits as i128)
}

pub async fn discover_pool(
    client: &mut Client,
    clmm_pools_handle: &str,
    coin_type_a: &str,
    coin_type_b: &str,
) -> Result<DiscoveredPool> {
    let mut page_token = None;
    let mut candidates = Vec::new();
    loop {
        let mut request = ListDynamicFieldsRequest::default();
        request.parent = Some(clmm_pools_handle.to_string());
        request.page_size = Some(1000);
        request.page_token = page_token;
        request.read_mask = Some(FieldMask::from_paths(["field_id", "field_object.json"]));
        let response = client
            .state_client()
            .list_dynamic_fields(request)
            .await
            .with_context(|| format!("failed to list Cetus pool registry {clmm_pools_handle}"))?
            .into_inner();

        for field in response.dynamic_fields {
            let Some(object) = field.field_object else {
                continue;
            };
            let Some(json) = object.json else { continue };
            let fields = prost_types_value_to_json(&json);
            let Ok(node) = json_field(&fields, "value") else {
                continue;
            };
            let Ok(entry) = json_field(node, "value") else {
                continue;
            };
            let (Ok(a), Ok(b), Ok(pool_id)) = (
                json_string(entry, "coin_type_a"),
                json_string(entry, "coin_type_b"),
                json_string(entry, "pool_id"),
            ) else {
                continue;
            };
            let matches = (coin_types_equal(&a, coin_type_a) && coin_types_equal(&b, coin_type_b))
                || (coin_types_equal(&a, coin_type_b) && coin_types_equal(&b, coin_type_a));
            if matches {
                candidates.push(DiscoveredPool {
                    pool_id,
                    coin_type_a: a,
                    coin_type_b: b,
                });
            }
        }

        page_token = response.next_page_token;
        if page_token.is_none() {
            break;
        }
    }

    if candidates.is_empty() {
        bail!("no Cetus testnet pool found for {coin_type_a} / {coin_type_b}");
    }

    let mut best: Option<(u128, DiscoveredPool)> = None;
    for candidate in candidates {
        let Ok((state, _)) = fetch_pool_state(client, &candidate.pool_id).await else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|(liquidity, _)| state.liquidity > *liquidity)
        {
            best = Some((state.liquidity, candidate));
        }
    }
    best.map(|(_, pool)| pool)
        .with_context(|| format!("no Cetus testnet pool found for {coin_type_a} / {coin_type_b}"))
}

pub fn parse_pool_type_params(object_type: &str) -> Result<(String, String)> {
    let start = object_type
        .find('<')
        .context("pool type has no type parameters")?;
    let end = object_type
        .rfind('>')
        .context("pool type has unbalanced type parameters")?;
    let inner = &object_type[start + 1..end];
    let mut depth = 0i32;
    let mut split_at = None;
    for (i, ch) in inner.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                split_at = Some(i);
                break;
            }
            _ => {}
        }
    }
    let split_at = split_at.context("pool type does not have two type parameters")?;
    let a = inner[..split_at].trim().to_string();
    let b = inner[split_at + 1..].trim().to_string();
    Ok((a, b))
}

fn coin_types_equal(a: &str, b: &str) -> bool {
    normalize_coin_type(a) == normalize_coin_type(b)
}

fn normalize_coin_type(coin_type: &str) -> String {
    let Some((address, rest)) = coin_type.split_once("::") else {
        return coin_type.to_string();
    };
    let trimmed = address.trim_start_matches("0x").trim_start_matches('0');
    format!("0x{trimmed}::{rest}")
}

fn prost_types_value_to_json(value: &prost_types::Value) -> Value {
    use prost_types::value::Kind;
    match &value.kind {
        None | Some(Kind::NullValue(_)) => Value::Null,
        Some(Kind::NumberValue(n)) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Some(Kind::StringValue(s)) => Value::String(s.clone()),
        Some(Kind::BoolValue(b)) => Value::Bool(*b),
        Some(Kind::StructValue(s)) => Value::Object(
            s.fields
                .iter()
                .map(|(key, value)| (key.clone(), prost_types_value_to_json(value)))
                .collect(),
        ),
        Some(Kind::ListValue(list)) => {
            Value::Array(list.values.iter().map(prost_types_value_to_json).collect())
        }
    }
}
