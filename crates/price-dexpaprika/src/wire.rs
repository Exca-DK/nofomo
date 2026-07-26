use serde_json::Value;
use tempo_agentic_price::{PricePair, PriceTick};

/// Maps supported EVM chain IDs to DexPaprika names.
pub fn chain_slug(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        1 => Some("ethereum"),
        8453 => Some("base"),
        42161 => Some("arbitrum"),
        _ => None,
    }
}

/// Parses a usable quote for `pair`, skipping unrelated or invalid frames.
pub fn parse_tick(pair: &PricePair, data: &str) -> Option<PriceTick> {
    let value: Value = serde_json::from_str(data).ok()?;
    // Some frames wrap the payload; accept either shape.
    let body = value.get("data").unwrap_or(&value);

    // Reject frames for a different pair.
    if !frame_matches(body, pair) {
        return None;
    }

    Some(PriceTick {
        pair: pair.clone(),
        price_usd: price_of(body)?,
        // Missing timestamps must not bypass the staleness gate.
        published_at: body.get("timestamp").and_then(Value::as_i64)?,
    })
}

// Match the echoed pair case-insensitively for EIP-55 addresses.
fn frame_matches(body: &Value, pair: &PricePair) -> bool {
    let Some(expected_chain) = chain_slug(pair.chain_id) else {
        return false;
    };
    let address = body.get("address").and_then(Value::as_str);
    let chain = body.get("chain").and_then(Value::as_str);
    match (address, chain) {
        (Some(address), Some(chain)) => {
            address.eq_ignore_ascii_case(&pair.token_address)
                && chain.eq_ignore_ascii_case(expected_chain)
        }
        _ => false,
    }
}

// The feed sends prices as either numbers or strings.
fn price_of(body: &Value) -> Option<f64> {
    let raw = body.get("price").or_else(|| body.get("price_usd"))?;
    raw.as_f64()
        .or_else(|| raw.as_str().and_then(|text| text.parse().ok()))
}
