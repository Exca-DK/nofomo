//! Creates and seeds a Cetus testnet pool.

use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Parser;
use sui_rpc::Client;
use sui_rpc::proto::sui::rpc::v2::GetCoinInfoRequest;
use sui_sdk_types::{Address, TypeTag};
use sui_transaction_builder::{Function, ObjectInput, TransactionBuilder};
use tempo_agentic_cetus::{constants, pool};
use tempo_agentic_chain::SuiChainClient;
use tempo_agentic_config::Config;
use tempo_agentic_domain::{ChainClient, ChainFamily, SignedTx, Signer, UnsignedTx};
use tempo_agentic_vault::{Vault, VaultSigner};

const CLOCK_OBJECT_ID: &str = "0x6";

#[derive(Parser)]
#[command(
    name = "create-pool",
    about = "Open and seed a Cetus pool on Sui testnet"
)]
struct Cli {
    #[arg(long, env = "TEMPO_AGENTIC_CONFIG", default_value = "config.json")]
    config: String,
    /// Configured symbol or Move type.
    #[arg(long)]
    coin_a: String,
    /// Configured symbol or Move type.
    #[arg(long)]
    coin_b: String,
    /// Overrides the configured pool registry.
    #[arg(long)]
    pools: Option<String>,
    /// Base units of `coin_a` to seed the pool with.
    #[arg(long)]
    amount_a: u64,
    /// Base units of `coin_b` to seed the pool with.
    #[arg(long)]
    amount_b: u64,
    /// Price of one whole `coin_a` denominated in whole `coin_b`.
    #[arg(long)]
    price: f64,
    #[arg(long, default_value_t = 60)]
    tick_spacing: u32,
    /// Print the transaction instead of sending it.
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(&cli.config)?;
    if !config.sui.enabled {
        bail!("enable sui in {} first", cli.config);
    }

    let coin_a = resolve_coin(&config, &cli.coin_a)?;
    let coin_b = resolve_coin(&config, &cli.coin_b)?;
    if coin_a == coin_b {
        bail!("coin_a and coin_b must differ");
    }

    let vault = load_vault(&config)?;
    let sender_address = vault.address(ChainFamily::Sui)?;
    let sender = Address::from_str(sender_address)
        .with_context(|| format!("the vault's Sui address is unusable: {sender_address}"))?;

    let constants = constants::for_network(config.sui.network)?;
    let mut client =
        Client::new(config.sui.rpc_url.as_str()).context("failed to build Sui RPC client")?;

    let decimals_a = pool::fetch_coin_decimals(&mut client, &coin_a).await?;
    let decimals_b = pool::fetch_coin_decimals(&mut client, &coin_b).await?;
    let metadata_a = fetch_metadata_id(&mut client, &coin_a).await?;
    let metadata_b = fetch_metadata_id(&mut client, &coin_b).await?;
    let sqrt_price = sqrt_price_x64(cli.price, decimals_a, decimals_b)?;

    println!("sender      {sender}");
    println!("coin_a      {coin_a} ({decimals_a} decimals)");
    println!("coin_b      {coin_b} ({decimals_b} decimals)");
    println!(
        "price       1 {} = {} {}",
        cli.coin_a, cli.price, cli.coin_b
    );
    println!("sqrt_price  {sqrt_price}");
    println!(
        "registry    {}",
        cli.pools.as_deref().unwrap_or(constants.clmm_pools_id)
    );

    let mut builder = TransactionBuilder::new();
    builder.set_sender(sender);

    let funded_a =
        pool::fund_exact_coin(&mut client, &mut builder, sender, &coin_a, cli.amount_a).await?;
    let funded_b =
        pool::fund_exact_coin(&mut client, &mut builder, sender, &coin_b, cli.amount_b).await?;

    // Use a full-range development position.
    let (tick_lower, tick_upper) = full_range_ticks(cli.tick_spacing);

    let global_config = builder.object(
        ObjectInput::new(
            constants
                .global_config_id
                .parse()
                .context("invalid global config id")?,
        )
        .as_shared()
        .with_mutable(false),
    );
    // Use the venue's registry by default.
    let pools_id = cli.pools.as_deref().unwrap_or(constants.clmm_pools_id);
    let pools = builder.object(
        ObjectInput::new(pools_id.parse().context("invalid pools object id")?)
            .as_shared()
            .with_mutable(true),
    );
    let clock = builder.object(
        ObjectInput::new(CLOCK_OBJECT_ID.parse().context("invalid clock object id")?)
            .as_shared()
            .with_mutable(false),
    );
    let metadata_a_arg = builder.object(ObjectInput::new(
        metadata_a.parse().context("invalid coin_a metadata id")?,
    ));
    let metadata_b_arg = builder.object(ObjectInput::new(
        metadata_b.parse().context("invalid coin_b metadata id")?,
    ));

    let tick_spacing_arg = builder.pure(&cli.tick_spacing);
    let sqrt_price_arg = builder.pure(&sqrt_price);
    let url_arg = builder.pure(&String::new());
    let tick_lower_arg = builder.pure(&tick_lower);
    let tick_upper_arg = builder.pure(&tick_upper);
    let fix_amount_a = builder.pure(&true);

    let function = Function::new(
        constants
            .clmm_pool_package_id
            .parse()
            .context("invalid clmm pool package id")?,
        "pool_creator".parse().context("invalid module name")?,
        "create_pool_v2".parse().context("invalid function name")?,
    )
    .with_type_args(vec![type_tag(&coin_a)?, type_tag(&coin_b)?]);

    let created = builder.move_call(
        function,
        vec![
            global_config,
            pools,
            tick_spacing_arg,
            sqrt_price_arg,
            url_arg,
            tick_lower_arg,
            tick_upper_arg,
            funded_a,
            funded_b,
            metadata_a_arg,
            metadata_b_arg,
            fix_amount_a,
            clock,
        ],
    );

    // Transfer the position and unused coins.
    let returned = created.to_nested(3);
    let recipient = builder.pure(&sender);
    builder.transfer_objects(returned, recipient);

    let transaction = builder
        .build(&mut client)
        .await
        .context("failed to build the pool creation transaction")?;

    if cli.dry_run {
        println!("\ndry run: digest would be {}", transaction.digest());
        return Ok(());
    }

    let SignedTx::Sui(signed) = vault
        .sign(&UnsignedTx::Sui(Box::new(transaction)))
        .await
        .context("failed to sign the pool creation transaction")?
    else {
        bail!("the vault answered a Sui transaction with a signature for another chain");
    };

    let node = SuiChainClient::new(&config.sui.rpc_url, config.sui.gas_budget)?;
    let digest = node.broadcast(&SignedTx::Sui(signed)).await?;
    println!("\nsent {digest}");
    println!("find the pool id in the transaction's created objects, then put it in the config");
    Ok(())
}

fn resolve_coin(config: &Config, name: &str) -> Result<String> {
    if let Some((_, coin)) = config
        .sui
        .coins
        .iter()
        .find(|(symbol, _)| symbol.eq_ignore_ascii_case(name))
    {
        return Ok(coin.coin_type.clone());
    }
    tempo_agentic_domain::validate_coin_type(name)
        .with_context(|| format!("{name} is neither a configured symbol nor a Move type"))?;
    Ok(name.to_string())
}

fn load_vault(config: &Config) -> Result<Vault> {
    let path = config
        .keys
        .sui
        .as_deref()
        .context("set keys.sui in the config first")?;
    let mut vault = Vault::new();
    vault.add(VaultSigner::load(
        ChainFamily::Sui,
        std::path::Path::new(path),
    )?);
    Ok(vault)
}

async fn fetch_metadata_id(client: &mut Client, coin_type: &str) -> Result<String> {
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
        .and_then(|metadata| metadata.id)
        .with_context(|| format!("{coin_type} has no published CoinMetadata object"))
}

fn type_tag(coin_type: &str) -> Result<TypeTag> {
    TypeTag::from_str(coin_type).with_context(|| format!("invalid coin type {coin_type}"))
}

/// Cetus stores the price as a Q64.64 square root, scaled by the decimal gap.
fn sqrt_price_x64(price: f64, decimals_a: u8, decimals_b: u8) -> Result<u128> {
    if !price.is_finite() || price <= 0.0 {
        bail!("price must be a positive number");
    }
    let scale = 10f64.powi(i32::from(decimals_b) - i32::from(decimals_a));
    let raw = (price * scale).sqrt() * 2f64.powi(64);
    if !raw.is_finite() || raw <= 0.0 || raw >= u128::MAX as f64 {
        bail!("price {price} does not fit Cetus's Q64.64 sqrt price");
    }
    Ok(raw as u128)
}

/// The widest range the spacing allows, as ticks in Cetus's u32 encoding.
fn full_range_ticks(tick_spacing: u32) -> (u32, u32) {
    const MAX_TICK: i32 = 443_636;
    let usable = MAX_TICK - MAX_TICK % tick_spacing as i32;
    ((-usable) as u32, usable as u32)
}

#[cfg(test)]
mod tests {
    use super::{full_range_ticks, sqrt_price_x64};

    #[test]
    fn a_price_of_one_is_the_q64_unit() {
        assert_eq!(sqrt_price_x64(1.0, 9, 9).unwrap(), 1u128 << 64);
    }

    #[test]
    fn the_decimal_gap_scales_the_price() {
        let scaled = sqrt_price_x64(1.0, 8, 6).unwrap();
        let unscaled = sqrt_price_x64(1.0, 6, 6).unwrap();
        assert!(scaled < unscaled, "more decimals must lower the raw price");
    }

    #[test]
    fn a_nonsense_price_is_refused() {
        assert!(sqrt_price_x64(0.0, 9, 9).is_err());
        assert!(sqrt_price_x64(-1.0, 9, 9).is_err());
        assert!(sqrt_price_x64(f64::NAN, 9, 9).is_err());
    }

    #[test]
    fn the_full_range_lands_on_the_spacing_grid() {
        let (lower, upper) = full_range_ticks(60);
        assert_eq!(upper % 60, 0);
        assert_eq!((lower as i32).unsigned_abs() % 60, 0);
        assert_eq!(lower as i32, -(upper as i32));
    }
}
