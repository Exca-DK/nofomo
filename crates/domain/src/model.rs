use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VenueName {
    Uniswap,
    Cetus,
}

impl VenueName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uniswap => "uniswap",
            Self::Cetus => "cetus",
        }
    }
}

impl std::str::FromStr for VenueName {
    type Err = anyhow::Error;

    /// Parses [`VenueName::as_str`] output, rejecting unknown values.
    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "uniswap" => Ok(Self::Uniswap),
            "cetus" => Ok(Self::Cetus),
            other => anyhow::bail!("unknown venue '{other}'"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct MarketResearchRequest {
    pub token_in: String,
    pub token_out: String,
    #[serde(default)]
    pub chains: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct QuoteTradeRequest {
    pub venue: VenueName,
    pub token_in: String,
    pub token_out: String,
    /// Exact input amount in human decimal units (e.g. "0.01").
    pub amount: String,
    /// Maximum allowed slippage in basis points.
    pub slippage_bps: u16,
    #[serde(default)]
    pub chains: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExecuteTradeRequest {
    pub quote_id: String,
    /// Must be true to prevent clients from executing unconfirmed trade quotes.
    pub confirmed: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MarketResearch {
    pub pair: String,
    pub observations: Vec<MarketObservation>,
    pub guard_passed: bool,
    pub guard_reason: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MarketObservation {
    pub chain: String,
    pub pool_id: String,
    pub protocol: String,
    pub token0: String,
    pub token1: String,
    pub token0_price: String,
    pub token1_price: String,
    pub tvl_usd: String,
    pub volume_usd: String,
    pub tx_count: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct QuoteView {
    pub quote_id: String,
    pub venue: String,
    pub chain: String,
    pub token_in: String,
    pub token_out: String,
    pub amount_in: String,
    pub expected_amount_out: String,
    pub minimum_amount_out: String,
    pub expires_at_unix: u64,
    pub graph_guard: String,
    pub requires_confirmation: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ExecutionView {
    pub quote_id: String,
    pub venue: String,
    pub chain: String,
    pub transactions: Vec<TransactionReference>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct TransactionReference {
    /// Action category like cancel, approval, swap, or sui_digest.
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QuoteDraft {
    pub venue: String,
    pub chain: String,
    pub token_in: String,
    pub token_out: String,
    pub amount_in: String,
    pub expected_amount_out: String,
    pub minimum_amount_out: String,
    pub graph_guard: String,
    pub plan: ExecutionPlan,
}

// Stored plans must deserialize after restart. The JSON quote prevents `Eq`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum ExecutionPlan {
    Uniswap {
        chain_name: String,
        chain_id: u64,
        input_token: String,
        input_amount: String,
        quote: Value,
    },
    Cetus {
        pool_id: String,
        a2b: bool,
        input_amount: u64,
        min_amount_out: u64,
    },
}

#[derive(Clone, Debug)]
pub struct StoredQuote {
    pub id: String,
    pub expires_at_unix: u64,
    pub draft: QuoteDraft,
}

impl StoredQuote {
    pub fn view(&self) -> QuoteView {
        QuoteView {
            quote_id: self.id.clone(),
            venue: self.draft.venue.clone(),
            chain: self.draft.chain.clone(),
            token_in: self.draft.token_in.clone(),
            token_out: self.draft.token_out.clone(),
            amount_in: self.draft.amount_in.clone(),
            expected_amount_out: self.draft.expected_amount_out.clone(),
            minimum_amount_out: self.draft.minimum_amount_out.clone(),
            expires_at_unix: self.expires_at_unix,
            graph_guard: self.draft.graph_guard.clone(),
            requires_confirmation: true,
        }
    }

    pub fn expired(&self) -> bool {
        unix_now() >= self.expires_at_unix
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn unix_now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// Parses a positive decimal amount into `u64` base units.
pub fn parse_units(value: &str, decimals: u8) -> anyhow::Result<u64> {
    let raw = parse_units_string(value, decimals)?;
    raw.parse::<u64>()
        .map_err(|_| anyhow::anyhow!("amount must fit in a non-zero u64 base-unit value"))
}

/// Parses a decimal amount into base units without floating-point loss.
pub fn parse_units_string(value: &str, decimals: u8) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') || value.starts_with('+') {
        anyhow::bail!("amount must be a positive decimal string");
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        anyhow::bail!("amount must be a decimal string");
    }
    if fraction.len() > usize::from(decimals) {
        anyhow::bail!("amount has more than {decimals} decimal places");
    }
    let whole = if whole.is_empty() { "0" } else { whole };
    let raw = format!("{whole}{fraction:0<width$}", width = usize::from(decimals));
    let raw = raw.trim_start_matches('0');
    if raw.is_empty() {
        anyhow::bail!("amount must be greater than zero");
    }
    const U256_MAX: &str =
        "115792089237316195423570985008687907853269984665640564039457584007913129639935";
    if raw.len() > U256_MAX.len() || (raw.len() == U256_MAX.len() && raw > U256_MAX) {
        anyhow::bail!("amount exceeds uint256");
    }
    Ok(raw.to_string())
}

pub fn format_units(raw: u64, decimals: u8) -> String {
    format_units_string(&raw.to_string(), decimals).expect("u64 is valid decimal base units")
}

/// Formats decimal base units without floating-point math.
pub fn format_units_string(raw: &str, decimals: u8) -> anyhow::Result<String> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        anyhow::bail!("base units must be an unsigned decimal integer");
    }
    let raw = raw.trim_start_matches('0');
    let raw = if raw.is_empty() { "0" } else { raw };
    if decimals == 0 {
        return Ok(raw.to_string());
    }
    let decimals = usize::from(decimals);
    let padded;
    let digits = if raw.len() <= decimals {
        padded = format!("{:0>width$}", raw, width = decimals + 1);
        padded.as_str()
    } else {
        raw
    };
    let split = digits.len() - decimals;
    let fraction = digits[split..].trim_end_matches('0');
    if fraction.is_empty() {
        Ok(digits[..split].to_string())
    } else {
        Ok(format!("{}.{}", &digits[..split], fraction))
    }
}

/// Applies up to 5000 basis points of slippage.
pub fn apply_slippage(raw: u64, slippage_bps: u16) -> anyhow::Result<u64> {
    if slippage_bps > 5_000 {
        anyhow::bail!("slippage_bps must not exceed 5000");
    }
    Ok(((u128::from(raw) * u128::from(10_000 - slippage_bps)) / 10_000) as u64)
}

/// Applies up to 10000 basis points of slippage to decimal base units.
pub fn apply_slippage_string(raw: &str, slippage_bps: u16) -> anyhow::Result<String> {
    if slippage_bps > 10_000 {
        anyhow::bail!("slippage_bps must not exceed 10000");
    }
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        anyhow::bail!("base units must be an unsigned decimal integer");
    }
    let multiplier = u32::from(10_000 - slippage_bps);
    let mut carry = 0_u32;
    let mut reversed = Vec::with_capacity(raw.len() + 4);
    for digit in raw.bytes().rev() {
        let product = u32::from(digit - b'0') * multiplier + carry;
        reversed.push((product % 10) as u8 + b'0');
        carry = product / 10;
    }
    while carry > 0 {
        reversed.push((carry % 10) as u8 + b'0');
        carry /= 10;
    }
    reversed.reverse();
    let product = String::from_utf8(reversed).expect("decimal multiplication emits ASCII");
    let quotient = if product.len() <= 4 {
        "0"
    } else {
        product[..product.len() - 4].trim_start_matches('0')
    };
    Ok(if quotient.is_empty() { "0" } else { quotient }.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        apply_slippage, apply_slippage_string, format_units, format_units_string, parse_units,
        parse_units_string,
    };

    #[test]
    fn converts_token_units_without_float_math() {
        assert_eq!(parse_units("1.25", 6).unwrap(), 1_250_000);
        assert_eq!(format_units(1_250_000, 6), "1.25");
        assert!(parse_units("0.0000001", 6).is_err());
        assert_eq!(
            parse_units_string("1.25", 18).unwrap(),
            "1250000000000000000"
        );
        assert_eq!(
            format_units_string("1250000000000000000", 18).unwrap(),
            "1.25"
        );
    }

    #[test]
    fn applies_floor_slippage() {
        assert_eq!(apply_slippage(1_000_000, 50).unwrap(), 995_000);
        assert_eq!(apply_slippage_string("1000000", 50).unwrap(), "995000");
    }
}
