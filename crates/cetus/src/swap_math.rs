use anyhow::{Result, bail};
use ethnum::U256;

use crate::constants::{FEE_RATE_DENOMINATOR, MAX_SQRT_PRICE, MIN_SQRT_PRICE};

/// A single initialized tick crossed while walking a swap through a pool.
#[derive(Clone, Copy, Debug)]
pub struct TickData {
    pub index: i32,
    pub sqrt_price: u128,
    /// Signed liquidity delta applied when the swap crosses this tick.
    pub liquidity_net: i128,
}

/// The subset of on-chain pool state the swap loop needs.
#[derive(Clone, Copy, Debug)]
pub struct PoolState {
    pub current_sqrt_price: u128,
    pub current_tick_index: i32,
    pub liquidity: u128,
    /// Fee numerator; see [`FEE_RATE_DENOMINATOR`] for the denominator.
    pub fee_rate: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SwapResult {
    pub amount_in: u128,
    pub amount_out: u128,
    pub fee_amount: u128,
    pub next_sqrt_price: u128,
    pub cross_tick_count: u32,
}

fn u256(value: u128) -> U256 {
    U256::from(value)
}

fn to_u128(value: U256, context: &str) -> Result<u128> {
    value
        .try_into()
        .map_err(|_| anyhow::anyhow!("{context} overflowed u128"))
}

fn div_round_up_if(numerator: U256, denominator: U256, round_up: bool) -> Result<U256> {
    if denominator == U256::ZERO {
        bail!("division by zero in CLMM swap math");
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    Ok(if round_up && remainder != U256::ZERO {
        quotient + U256::ONE
    } else {
        quotient
    })
}

fn get_delta_a(
    sqrt_price0: u128,
    sqrt_price1: u128,
    liquidity: u128,
    round_up: bool,
) -> Result<u128> {
    let diff = sqrt_price0.abs_diff(sqrt_price1);
    let numerator = (u256(liquidity) * u256(diff)) << 64u32;
    let denominator = u256(sqrt_price0) * u256(sqrt_price1);
    to_u128(
        div_round_up_if(numerator, denominator, round_up)?,
        "delta_a",
    )
}

fn get_delta_b(
    sqrt_price0: u128,
    sqrt_price1: u128,
    liquidity: u128,
    round_up: bool,
) -> Result<u128> {
    let diff = sqrt_price0.abs_diff(sqrt_price1);
    if liquidity == 0 || diff == 0 {
        return Ok(0);
    }
    let product = u256(liquidity) * u256(diff);
    let shifted = product >> 64u32;
    let remainder = product & U256::from(u64::MAX);
    let result = if round_up && remainder > U256::ZERO {
        shifted + U256::ONE
    } else {
        shifted
    };
    to_u128(result, "delta_b")
}

fn check_sqrt_price_bounds(sqrt_price: u128) -> Result<u128> {
    if !(MIN_SQRT_PRICE..=MAX_SQRT_PRICE).contains(&sqrt_price) {
        bail!("next sqrt price {sqrt_price} is out of CLMM bounds");
    }
    Ok(sqrt_price)
}

fn get_next_sqrt_price_a_up(
    sqrt_price: u128,
    liquidity: u128,
    amount: u128,
    by_amount_in: bool,
) -> Result<u128> {
    if amount == 0 {
        return Ok(sqrt_price);
    }
    let numerator = (u256(sqrt_price) * u256(liquidity)) << 64u32;
    let liquidity_shl64 = u256(liquidity) << 64u32;
    let product = u256(sqrt_price) * u256(amount);
    let denominator = if by_amount_in {
        liquidity_shl64 + product
    } else {
        if liquidity_shl64 <= product {
            bail!("insufficient liquidity for requested output amount");
        }
        liquidity_shl64 - product
    };
    let next = to_u128(
        div_round_up_if(numerator, denominator, true)?,
        "next_sqrt_price_a",
    )?;
    check_sqrt_price_bounds(next)
}

fn get_next_sqrt_price_b_down(
    sqrt_price: u128,
    liquidity: u128,
    amount: u128,
    by_amount_in: bool,
) -> Result<u128> {
    let numerator = u256(amount) << 64u32;
    let delta = to_u128(
        div_round_up_if(numerator, u256(liquidity), !by_amount_in)?,
        "delta_sqrt_price",
    )?;
    let next = if by_amount_in {
        sqrt_price
            .checked_add(delta)
            .ok_or_else(|| anyhow::anyhow!("sqrt price overflow"))?
    } else {
        sqrt_price
            .checked_sub(delta)
            .ok_or_else(|| anyhow::anyhow!("sqrt price underflow"))?
    };
    check_sqrt_price_bounds(next)
}

fn get_next_sqrt_price_from_input(
    sqrt_price: u128,
    liquidity: u128,
    amount: u128,
    a_to_b: bool,
) -> Result<u128> {
    if a_to_b {
        get_next_sqrt_price_a_up(sqrt_price, liquidity, amount, true)
    } else {
        get_next_sqrt_price_b_down(sqrt_price, liquidity, amount, true)
    }
}

fn get_next_sqrt_price_from_output(
    sqrt_price: u128,
    liquidity: u128,
    amount: u128,
    a_to_b: bool,
) -> Result<u128> {
    if a_to_b {
        get_next_sqrt_price_b_down(sqrt_price, liquidity, amount, false)
    } else {
        get_next_sqrt_price_a_up(sqrt_price, liquidity, amount, false)
    }
}

fn get_delta_up_from_input(
    current_sqrt_price: u128,
    target_sqrt_price: u128,
    liquidity: u128,
    a_to_b: bool,
) -> Result<u128> {
    if liquidity == 0 || current_sqrt_price == target_sqrt_price {
        return Ok(0);
    }
    if a_to_b {
        get_delta_a(current_sqrt_price, target_sqrt_price, liquidity, true)
    } else {
        get_delta_b(current_sqrt_price, target_sqrt_price, liquidity, true)
    }
}

fn get_delta_down_from_output(
    current_sqrt_price: u128,
    target_sqrt_price: u128,
    liquidity: u128,
    a_to_b: bool,
) -> Result<u128> {
    if liquidity == 0 || current_sqrt_price == target_sqrt_price {
        return Ok(0);
    }
    if a_to_b {
        get_delta_b(current_sqrt_price, target_sqrt_price, liquidity, false)
    } else {
        get_delta_a(current_sqrt_price, target_sqrt_price, liquidity, false)
    }
}

struct StepResult {
    amount_in: u128,
    amount_out: u128,
    next_sqrt_price: u128,
    fee_amount: u128,
}

fn mul_div_floor(a: u128, b: u64, denominator: u64) -> Result<u128> {
    to_u128(
        u256(a) * u256(u128::from(b)) / u256(u128::from(denominator)),
        "mul_div_floor",
    )
}

fn mul_div_ceil(a: u128, b: u64, denominator: u64) -> Result<u128> {
    let numerator = u256(a) * u256(u128::from(b));
    let denominator = u256(u128::from(denominator));
    to_u128(
        div_round_up_if(numerator, denominator, true)?,
        "mul_div_ceil",
    )
}

fn compute_swap_step(
    current_sqrt_price: u128,
    target_sqrt_price: u128,
    liquidity: u128,
    amount: u128,
    fee_rate: u64,
    by_amount_in: bool,
) -> Result<StepResult> {
    if liquidity == 0 {
        return Ok(StepResult {
            amount_in: 0,
            amount_out: 0,
            next_sqrt_price: target_sqrt_price,
            fee_amount: 0,
        });
    }
    let a_to_b = current_sqrt_price >= target_sqrt_price;

    if by_amount_in {
        let amount_remain = mul_div_floor(
            amount,
            FEE_RATE_DENOMINATOR
                .checked_sub(fee_rate)
                .ok_or_else(|| anyhow::anyhow!("fee_rate exceeds denominator"))?,
            FEE_RATE_DENOMINATOR,
        )?;
        let max_amount_in =
            get_delta_up_from_input(current_sqrt_price, target_sqrt_price, liquidity, a_to_b)?;
        let (amount_in, fee_amount, next_sqrt_price) = if max_amount_in > amount_remain {
            (
                amount_remain,
                amount
                    .checked_sub(amount_remain)
                    .ok_or_else(|| anyhow::anyhow!("fee amount underflow"))?,
                get_next_sqrt_price_from_input(
                    current_sqrt_price,
                    liquidity,
                    amount_remain,
                    a_to_b,
                )?,
            )
        } else {
            (
                max_amount_in,
                mul_div_ceil(
                    max_amount_in,
                    fee_rate,
                    FEE_RATE_DENOMINATOR
                        .checked_sub(fee_rate)
                        .ok_or_else(|| anyhow::anyhow!("fee_rate exceeds denominator"))?,
                )?,
                target_sqrt_price,
            )
        };
        let amount_out =
            get_delta_down_from_output(current_sqrt_price, next_sqrt_price, liquidity, a_to_b)?;
        Ok(StepResult {
            amount_in,
            amount_out,
            next_sqrt_price,
            fee_amount,
        })
    } else {
        let max_amount_out =
            get_delta_down_from_output(current_sqrt_price, target_sqrt_price, liquidity, a_to_b)?;
        let (amount_out, next_sqrt_price) = if max_amount_out > amount {
            (
                amount,
                get_next_sqrt_price_from_output(current_sqrt_price, liquidity, amount, a_to_b)?,
            )
        } else {
            (max_amount_out, target_sqrt_price)
        };
        let amount_in =
            get_delta_up_from_input(current_sqrt_price, next_sqrt_price, liquidity, a_to_b)?;
        let fee_amount = mul_div_ceil(
            amount_in,
            fee_rate,
            FEE_RATE_DENOMINATOR
                .checked_sub(fee_rate)
                .ok_or_else(|| anyhow::anyhow!("fee_rate exceeds denominator"))?,
        )?;
        Ok(StepResult {
            amount_in,
            amount_out,
            next_sqrt_price,
            fee_amount,
        })
    }
}

pub fn compute_swap(
    a_to_b: bool,
    amount: u128,
    pool: PoolState,
    ticks: &[TickData],
) -> Result<SwapResult> {
    let sqrt_price_limit = if a_to_b {
        MIN_SQRT_PRICE
    } else {
        MAX_SQRT_PRICE
    };

    let mut remaining = amount;
    let mut liquidity = pool.liquidity;
    let mut sqrt_price = pool.current_sqrt_price;
    let mut result = SwapResult {
        next_sqrt_price: sqrt_price,
        ..Default::default()
    };

    let ordered: Box<dyn Iterator<Item = &TickData>> = if a_to_b {
        Box::new(ticks.iter().rev())
    } else {
        Box::new(ticks.iter())
    };

    for tick in ordered {
        if a_to_b && pool.current_tick_index < tick.index {
            continue;
        }
        if !a_to_b && pool.current_tick_index >= tick.index {
            continue;
        }
        if remaining == 0 {
            break;
        }

        let target_sqrt_price = if (a_to_b && sqrt_price_limit > tick.sqrt_price)
            || (!a_to_b && sqrt_price_limit < tick.sqrt_price)
        {
            sqrt_price_limit
        } else {
            tick.sqrt_price
        };

        let step = compute_swap_step(
            sqrt_price,
            target_sqrt_price,
            liquidity,
            remaining,
            pool.fee_rate,
            true,
        )?;

        if step.amount_in != 0 {
            remaining = remaining
                .checked_sub(step.amount_in + step.fee_amount)
                .ok_or_else(|| {
                    anyhow::anyhow!("swap consumed more than the remaining input amount")
                })?;
        }
        result.amount_in += step.amount_in;
        result.amount_out += step.amount_out;
        result.fee_amount += step.fee_amount;

        if step.next_sqrt_price == tick.sqrt_price {
            liquidity = if a_to_b {
                (liquidity as i128 + tick.liquidity_net) as u128
            } else {
                (liquidity as i128 - tick.liquidity_net) as u128
            };
            sqrt_price = tick.sqrt_price;
        } else {
            sqrt_price = step.next_sqrt_price;
        }
        result.cross_tick_count += 1;

        if remaining == 0 {
            break;
        }
    }

    if remaining != 0 {
        bail!(
            "insufficient liquidity in fetched tick range: {remaining} of {amount} input unfilled after crossing {} ticks",
            result.cross_tick_count
        );
    }

    result.amount_in += result.fee_amount;
    result.next_sqrt_price = sqrt_price;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_pool() -> PoolState {
        PoolState {
            current_sqrt_price: 1u128 << 64, // price == 1.0
            current_tick_index: 0,
            liquidity: 1_000_000_000_000,
            fee_rate: 2_500, // 0.25%
        }
    }

    #[test]
    fn swaps_within_a_single_tick_range_without_crossing() {
        let pool = flat_pool();
        let ticks = [
            TickData {
                index: -100,
                sqrt_price: MIN_SQRT_PRICE,
                liquidity_net: 0,
            },
            TickData {
                index: 100,
                sqrt_price: MAX_SQRT_PRICE,
                liquidity_net: 0,
            },
        ];
        let result = compute_swap(true, 1_000_000, pool, &ticks).unwrap();
        assert!(result.amount_out > 0);
        assert!(result.amount_out < 1_000_000);
        assert_eq!(result.cross_tick_count, 1);
        assert!(result.fee_amount > 0);
    }

    #[test]
    fn rejects_a_swap_that_exhausts_the_fetched_tick_window() {
        let pool = flat_pool();
        let ticks = [TickData {
            index: 1,
            sqrt_price: pool.current_sqrt_price + 1,
            liquidity_net: -1_000_000_000_000,
        }];
        let result = compute_swap(false, u128::MAX / 2, pool, &ticks);
        assert!(result.is_err());
    }

    #[test]
    fn delta_helpers_match_reference_rounding() {
        let sqrt_price0 = 1u128 << 64;
        let sqrt_price1 = 2u128 << 64;
        assert_eq!(
            get_delta_b(sqrt_price0, sqrt_price1, 1000, false).unwrap(),
            1000
        );
        assert_eq!(
            get_delta_b(sqrt_price0, sqrt_price0, 1000, false).unwrap(),
            0
        );
    }
}
