pub mod constants;
pub mod pool;
pub mod swap_math;

use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use constants::NetworkConstants;
use sui_rpc::Client;
use sui_rpc::field::{FieldMask, FieldMaskUtil};
use sui_rpc::proto::sui::rpc::v2::GetObjectRequest;
use sui_sdk_types::{Address, TypeTag};
use sui_transaction_builder::{Function, ObjectInput, TransactionBuilder};
use tempo_agentic_config::SuiConfig;
use tempo_agentic_domain::{
    ChainFamily, ExecStep, ExecutionPlan, QuoteDraft, QuoteTradeRequest, Signer, TradeVenue,
    TxContext, UnsignedTx, normalize_coin_type, validate_coin_type,
};
const CLOCK_OBJECT_ID: &str = "0x6";
const MAX_TICKS_FETCHED: usize = 20_000;

/// Cetus CLMM venue built directly from on-chain state.
pub struct CetusVenue {
    rpc_url: String,
    constants: NetworkConstants,
    signer: Arc<dyn Signer>,
}

impl CetusVenue {
    pub fn new(config: &SuiConfig, signer: Arc<dyn Signer>) -> Result<Self> {
        Ok(Self {
            rpc_url: config.rpc_url.clone(),
            constants: constants::for_network(config.network)?,
            signer,
        })
    }

    fn client(&self) -> Result<Client> {
        Client::new(self.rpc_url.as_str()).context("failed to build Sui RPC client")
    }

    fn sender(&self) -> Result<Address> {
        let address = self.signer.address(ChainFamily::Sui)?;
        Address::from_str(address)
            .with_context(|| format!("the vault's Sui address is unusable: {address}"))
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
        let a2b =
            normalize_coin_type(&discovered.coin_type_a) == normalize_coin_type(&request.token_in);
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

    async fn steps(&self, _plan: &ExecutionPlan) -> Result<Vec<ExecStep>> {
        // Sui swaps need no allowance step.
        Ok(vec![ExecStep::Swap])
    }

    async fn build(
        &self,
        plan: &ExecutionPlan,
        step: ExecStep,
        ctx: &TxContext,
    ) -> Result<UnsignedTx> {
        let ExecutionPlan::Cetus {
            pool_id,
            a2b,
            input_amount,
            min_amount_out,
        } = plan
        else {
            bail!("Cetus received a Uniswap execution plan");
        };
        if step != ExecStep::Swap {
            bail!(
                "Cetus plans only ever have a swap step, not {}",
                step.as_str()
            );
        }
        let TxContext::Sui {
            gas_price,
            gas_budget,
        } = ctx
        else {
            bail!("Cetus needs Sui chain state, not another family's");
        };

        let mut client = self.client()?;
        let (state, _) = pool::fetch_pool_state(&mut client, pool_id).await?;
        let _ = state; // Validates existence and pause state.

        let sender_address = self.sender()?;

        // Re-read coin types from the quoted pool.
        let discovered_pool = pool_id.clone();
        let (coin_type_a, coin_type_b) = pool_coin_types(&mut client, &discovered_pool).await?;

        let mut builder = TransactionBuilder::new();
        builder.set_sender(sender_address);
        // Pin gas for identical rebroadcasts.
        builder.set_gas_price(*gas_price);
        builder.set_gas_budget(*gas_budget);

        let (coin_a_arg, coin_b_arg) = if *a2b {
            let coin_a = pool::fund_exact_coin(
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
            let coin_b = pool::fund_exact_coin(
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
        Ok(UnsignedTx::Sui(Box::new(transaction)))
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

fn parse_type_tag(coin_type: &str) -> Result<TypeTag> {
    TypeTag::from_str(coin_type).with_context(|| format!("invalid coin type {coin_type}"))
}
