use std::cmp::Ordering;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// Reads an API transaction value from decimal or hex.
pub(crate) fn decimal_value(transaction: &Value) -> Result<String> {
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
pub(crate) fn api_gas_limit(transaction: &Value) -> Result<Option<u64>> {
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

pub(crate) fn validate_decimal_integer(value: &str) -> Result<()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("expected an unsigned decimal integer");
    }
    Ok(())
}

pub(crate) fn same_quantity(left: &str, right: &str) -> Result<bool> {
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

pub(crate) fn compare_decimal_integers(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

pub(crate) fn compare_scaled_amounts(
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

pub(crate) fn slippage_percent_json(slippage_bps: u16) -> Result<Value> {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        api_gas_limit, compare_scaled_amounts, decimal_value, hex_to_decimal, slippage_percent_json,
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
    fn ranks_scaled_outputs_exactly_and_encodes_bps() {
        assert!(compare_scaled_amounts("1000001", 6, "1000000000000000000", 18).is_gt());
        assert_eq!(slippage_percent_json(1).unwrap(), json!(0.01));
        assert_eq!(slippage_percent_json(50).unwrap(), json!(0.5));
    }
}
