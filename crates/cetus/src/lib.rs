pub mod constants;
pub mod pool;
pub mod swap_math;

use std::str::FromStr;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use constants::NetworkConstants;
use sui_crypto::SuiSigner;
use sui_crypto::simple::SimpleKeypair;
use sui_rpc::Client;
use sui_rpc::field::{FieldMask, FieldMaskUtil};
use sui_rpc::proto::sui::rpc::v2::{
    ExecuteTransactionRequest, GetObjectRequest, ListOwnedObjectsRequest,
};
use sui_sdk_types::{Address, TypeTag};
use sui_transaction_builder::{Function, ObjectInput, TransactionBuilder};
use tempo_agentic_config::SuiConfig;
use tempo_agentic_domain::{
    ExecutionPlan, QuoteDraft, QuoteTradeRequest, TradeVenue, TransactionReference, VenueExecution,
};

const CLOCK_OBJECT_ID: &str = "0x6";
const MAX_TICKS_FETCHED: usize = 20_000;

/// Sui trade venue that quotes and swaps directly against a Cetus CLMM pool.
///
/// Route discovery, swap-output math, and transaction construction all happen locally against
/// on-chain state (see [`pool`] and [`swap_math`]) — there is no dependency on Cetus's REST
/// aggregator, which is unreliable on testnet.
pub struct CetusVenue {
    rpc_url: String,
    constants: NetworkConstants,
    keystore_path: String,
}

impl CetusVenue {
    pub fn new(config: &SuiConfig) -> Result<Self> {
        let keystore_path = config
            .keystore_path
            .clone()
            .context("sui.keystore_path is required to run the Cetus venue")?;
        Ok(Self {
            rpc_url: config.rpc_url.clone(),
            constants: constants::for_network(config.network)?,
            keystore_path,
        })
    }

    fn client(&self) -> Result<Client> {
        Client::new(self.rpc_url.as_str()).context("failed to build Sui RPC client")
    }

    fn load_signer(&self) -> Result<SimpleKeypair> {
        let content = std::fs::read_to_string(&self.keystore_path)
            .with_context(|| format!("cannot read Sui keystore {}", self.keystore_path))?;
        let keys: Vec<String> =
            serde_json::from_str(&content).context("invalid JSON in Sui keystore")?;
        keys.iter()
            .find_map(|key| SimpleKeypair::from_base64(key).ok())
            .context("Sui keystore contains no usable keypair")
    }

    async fn candidate(
        &self,
        client: &mut Client,
        request: &QuoteTradeRequest,
    ) -> Result<QuoteDraft> {
        validate_coin_type(&request.token_in)?;
        validate_coin_type(&request.token_out)?;

        let discovered = pool::discover_pool(
            client,
            self.constants.clmm_pools_handle,
            &request.token_in,
            &request.token_out,
        )
        .await?;

        let (state, ticks_handle) = pool::fetch_pool_state(client, &discovered.pool_id).await?;
        let ticks = pool::fetch_ticks(client, &ticks_handle, MAX_TICKS_FETCHED).await?;

        // `a2b` follows Cetus's own convention: swapping from `coin_type_a` to `coin_type_b`.
        let a2b = coin_types_match(&discovered.coin_type_a, &request.token_in);
        let input_decimals = pool::fetch_coin_decimals(client, &request.token_in).await?;
        let output_decimals = pool::fetch_coin_decimals(client, &request.token_out).await?;
        let input_amount: u128 =
            tempo_agentic_domain::parse_units_string(&request.amount, input_decimals)?
                .parse()
                .context("input amount does not fit expected base-unit precision")?;
        let input_amount: u64 = input_amount
            .try_into()
            .context("input amount exceeds u64 base units")?;

        let result = swap_math::compute_swap(a2b, u128::from(input_amount), state, &ticks)?;
        let amount_out: u64 = result
            .amount_out
            .try_into()
            .context("computed output exceeds u64 base units")?;

        let minimum_out = tempo_agentic_domain::apply_slippage(amount_out, request.slippage_bps)?;

        Ok(QuoteDraft {
            venue: "cetus".into(),
            chain: "sui".into(),
            token_in: request.token_in.clone(),
            token_out: request.token_out.clone(),
            amount_in: request.amount.clone(),
            expected_amount_out: tempo_agentic_domain::format_units(amount_out, output_decimals),
            minimum_amount_out: tempo_agentic_domain::format_units(minimum_out, output_decimals),
            graph_guard: "cetus quotes are computed directly from on-chain pool state; no market-data guard applies".into(),
            plan: ExecutionPlan::Cetus {
                pool_id: discovered.pool_id,
                a2b,
                input_amount,
                min_amount_out: minimum_out,
            },
        })
    }

    /// Finds owned coin objects of `coin_type` summing to at least `amount`, merges them if
    /// needed, and splits off a coin argument holding exactly `amount`.
    async fn fund_exact_coin(
        &self,
        client: &mut Client,
        builder: &mut TransactionBuilder,
        owner: Address,
        coin_type: &str,
        amount: u64,
    ) -> Result<sui_transaction_builder::Argument> {
        let mut request = ListOwnedObjectsRequest::default();
        request.owner = Some(owner.to_string());
        request.object_type = Some(format!("0x2::coin::Coin<{coin_type}>"));
        request.page_size = Some(50);
        request.read_mask = Some(FieldMask::from_paths(["object_id", "balance"]));
        let response = client
            .state_client()
            .list_owned_objects(request)
            .await
            .with_context(|| format!("failed to list owned {coin_type} coins"))?
            .into_inner();

        let mut total = 0u64;
        let mut coin_ids = Vec::new();
        for object in response.objects {
            let Some(id) = object.object_id else {
                continue;
            };
            coin_ids.push(id);
            total = total.saturating_add(object.balance.unwrap_or_default());
            if total >= amount {
                break;
            }
        }
        if total < amount {
            bail!("insufficient {coin_type} balance: have {total}, need {amount}");
        }

        let mut coin_args = Vec::new();
        for id in &coin_ids {
            let address =
                Address::from_str(id).with_context(|| format!("invalid coin object id {id}"))?;
            coin_args.push(builder.object(ObjectInput::new(address)));
        }
        let primary = coin_args[0];
        if coin_args.len() > 1 {
            builder.merge_coins(primary, coin_args[1..].to_vec());
        }
        let amount_arg = builder.pure(&amount);
        let split = builder.split_coins(primary, vec![amount_arg]);
        Ok(split[0])
    }

    fn zero_coin(
        &self,
        builder: &mut TransactionBuilder,
        coin_type: &str,
    ) -> Result<sui_transaction_builder::Argument> {
        let type_tag = parse_type_tag(coin_type)?;
        let function = Function::new(
            "0x2".parse().context("invalid coin package address")?,
            "coin".parse().context("invalid coin module name")?,
            "zero".parse().context("invalid coin::zero function name")?,
        )
        .with_type_args(vec![type_tag]);
        Ok(builder.move_call(function, vec![]))
    }
}

#[async_trait]
impl TradeVenue for CetusVenue {
    fn name(&self) -> &'static str {
        "cetus"
    }

    async fn quote(&self, request: &QuoteTradeRequest) -> Result<QuoteDraft> {
        if request.token_in.eq_ignore_ascii_case(&request.token_out) {
            bail!("token_in and token_out must differ");
        }
        let mut client = self.client()?;
        self.candidate(&mut client, request).await
    }

    async fn execute(&self, plan: &ExecutionPlan) -> Result<VenueExecution> {
        let ExecutionPlan::Cetus {
            pool_id,
            a2b,
            input_amount,
            min_amount_out,
        } = plan
        else {
            bail!("Cetus received a Uniswap execution plan");
        };

        let mut client = self.client()?;
        let (state, _) = pool::fetch_pool_state(&mut client, pool_id).await?;
        let _ = state; // pool existence/pause already validated by fetch_pool_state

        let signer = self.load_signer()?;
        let sender = signer.verifying_key().derive_address();
        let sender_address = Address::from_str(&sender.to_string())
            .context("failed to parse derived Sui address")?;

        // Re-discover the pool's coin types from the pool object itself so execute() does not
        // need to trust caller-supplied types beyond what quote() already fixed via `pool_id`.
        let discovered_pool = pool_id.clone();
        let (coin_type_a, coin_type_b) = pool_coin_types(&mut client, &discovered_pool).await?;

        let mut builder = TransactionBuilder::new();
        builder.set_sender(sender_address);

        let (coin_a_arg, coin_b_arg) = if *a2b {
            let coin_a = self
                .fund_exact_coin(
                    &mut client,
                    &mut builder,
                    sender_address,
                    &coin_type_a,
                    *input_amount,
                )
                .await?;
            let coin_b = self.zero_coin(&mut builder, &coin_type_b)?;
            (coin_a, coin_b)
        } else {
            let coin_b = self
                .fund_exact_coin(
                    &mut client,
                    &mut builder,
                    sender_address,
                    &coin_type_b,
                    *input_amount,
                )
                .await?;
            let coin_a = self.zero_coin(&mut builder, &coin_type_a)?;
            (coin_a, coin_b)
        };

        let function_name = if *a2b { "swap_a2b" } else { "swap_b2a" };
        let function = Function::new(
            self.constants
                .integrate_package_id
                .parse()
                .context("invalid integrate package id")?,
            "pool_script_v2".parse().context("invalid module name")?,
            function_name
                .parse()
                .context("invalid swap function name")?,
        )
        .with_type_args(vec![
            parse_type_tag(&coin_type_a)?,
            parse_type_tag(&coin_type_b)?,
        ]);

        let global_config = builder.object(
            ObjectInput::new(
                self.constants
                    .global_config_id
                    .parse()
                    .context("invalid global config id")?,
            )
            .as_shared()
            .with_mutable(false),
        );
        let pool_arg = builder.object(
            ObjectInput::new(pool_id.parse().context("invalid pool id")?)
                .as_shared()
                .with_mutable(true),
        );
        let by_amount_in = builder.pure(&true);
        let amount_arg = builder.pure(input_amount);
        let amount_limit_arg = builder.pure(min_amount_out);
        let sqrt_price_limit = if *a2b {
            constants::MIN_SQRT_PRICE
        } else {
            constants::MAX_SQRT_PRICE
        };
        let sqrt_price_limit_arg = builder.pure(&sqrt_price_limit);
        let clock_arg = builder.object(
            ObjectInput::new(CLOCK_OBJECT_ID.parse().context("invalid clock object id")?)
                .as_shared()
                .with_mutable(false),
        );

        builder.move_call(
            function,
            vec![
                global_config,
                pool_arg,
                coin_a_arg,
                coin_b_arg,
                by_amount_in,
                amount_arg,
                amount_limit_arg,
                sqrt_price_limit_arg,
                clock_arg,
            ],
        );

        let transaction = builder
            .build(&mut client)
            .await
            .context("failed to build Cetus swap transaction")?;
        let signature = signer
            .sign_transaction(&transaction)
            .map_err(|error| anyhow::anyhow!("failed to sign Cetus swap transaction: {error}"))?;

        let mut request = ExecuteTransactionRequest::default();
        request.transaction = Some(transaction.clone().into());
        request.signatures = vec![signature.into()];
        request.read_mask = Some(FieldMask::from_paths(["digest", "effects.status"]));

        let response = client
            .execute_transaction_and_wait_for_checkpoint(
                request,
                std::time::Duration::from_secs(30),
            )
            .await
            .map_err(|error| anyhow::anyhow!("failed to execute Cetus swap transaction: {error}"))?
            .into_inner();

        let executed = response
            .transaction
            .context("Cetus swap execution response had no transaction")?;
        let success = executed
            .effects
            .as_ref()
            .and_then(|effects| effects.status.as_ref())
            .and_then(|status| status.success)
            .unwrap_or(false);
        if !success {
            bail!("Cetus swap transaction reverted");
        }

        Ok(VenueExecution {
            venue: "cetus".into(),
            chain: "sui".into(),
            transactions: vec![TransactionReference {
                kind: "sui_digest".into(),
                id: executed.digest.unwrap_or_default(),
            }],
        })
    }
}

async fn pool_coin_types(client: &mut Client, pool_id: &str) -> Result<(String, String)> {
    let mut request = GetObjectRequest::default();
    request.object_id = Some(pool_id.to_string());
    request.read_mask = Some(FieldMask::from_paths(["object_type"]));
    let response = client
        .ledger_client()
        .get_object(request)
        .await
        .with_context(|| format!("failed to re-fetch Cetus pool {pool_id}"))?
        .into_inner();
    let object_type = response
        .object
        .and_then(|object| object.object_type)
        .with_context(|| format!("Cetus pool {pool_id} has no Move type"))?;
    pool::parse_pool_type_params(&object_type)
        .with_context(|| format!("could not parse coin types from pool type {object_type}"))
}

fn coin_types_match(a: &str, b: &str) -> bool {
    let normalize = |t: &str| {
        let (address, rest) = t.split_once("::").unwrap_or((t, ""));
        format!(
            "{}::{rest}",
            address.trim_start_matches("0x").trim_start_matches('0')
        )
    };
    normalize(a) == normalize(b)
}

fn validate_coin_type(value: &str) -> Result<()> {
    let parts: Vec<&str> = value.split("::").collect();
    if parts.len() < 3 || !parts[0].starts_with("0x") {
        bail!("{value} is not a fully-qualified Sui coin type");
    }
    Ok(())
}

fn parse_type_tag(coin_type: &str) -> Result<TypeTag> {
    TypeTag::from_str(coin_type).with_context(|| format!("invalid coin type {coin_type}"))
}
