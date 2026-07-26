use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::amount::same_quantity;

pub(crate) fn validate_transaction(
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

pub(crate) fn validate_quote(
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

pub(crate) fn validate_approval_calldata(
    transaction: &Value,
    expected_spender: &str,
) -> Result<()> {
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
        // Name both: the usual cause is Uniswap moving the contract, and the
        // addresses are the whole diagnosis.
        bail!("approval calldata targets 0x{spender}, not the expected {expected_spender}");
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

pub(crate) fn string_field<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("transaction has no {field}"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::PROXY_APPROVAL_ADDRESS;

    use super::validate_transaction;

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
}
