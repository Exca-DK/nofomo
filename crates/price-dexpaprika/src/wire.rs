use serde_json::Value;
use tempo_agentic_price::{PricePair, PriceTick};

/// The name DexPaprika uses for an EVM chain.
///
/// Covers exactly the chains `Config::validate` accepts. Adding a chain to the
/// configuration without adding it here yields levels that nothing ever prices,
/// so the two lists have to move together. An unknown chain is `None`, never a
/// fallback name: pricing against the wrong chain is worse than not pricing.
pub fn chain_slug(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        1 => Some("ethereum"),
        8453 => Some("base"),
        42161 => Some("arbitrum"),
        _ => None,
    }
}

/// Reads a DexPaprika price frame for `pair`.
///
/// Returns `None` when the frame is not a usable quote for this pair: a
/// heartbeat, malformed JSON, a missing price or timestamp, or a frame about
/// some other token. Callers skip those rather than tearing down the stream.
pub fn parse_tick(pair: &PricePair, data: &str) -> Option<PriceTick> {
    let value: Value = serde_json::from_str(data).ok()?;
    // Some frames wrap the payload; accept either shape.
    let body = value.get("data").unwrap_or(&value);

    // A crossed subscription would otherwise price the wrong token, and the
    // daemon spends real funds on whatever price it is handed.
    if !frame_matches(body, pair) {
        return None;
    }

    Some(PriceTick {
        pair: pair.clone(),
        price_usd: price_of(body)?,
        // Required, not defaulted: substituting "now" would silently disable the
        // staleness gate for any feed that stops sending timestamps.
        published_at: body.get("timestamp").and_then(Value::as_i64)?,
    })
}

// The feed echoes the token and chain it is quoting; both must be the ones asked
// for. Compared case-insensitively because an EIP-55 checksummed address and its
// lowercase form name the same token.
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

// The feed reports the price as a JSON number in some frames and as a string in
// others, so both are accepted.
fn price_of(body: &Value) -> Option<f64> {
    let raw = body.get("price").or_else(|| body.get("price_usd"))?;
    raw.as_f64()
        .or_else(|| raw.as_str().and_then(|text| text.parse().ok()))
}
