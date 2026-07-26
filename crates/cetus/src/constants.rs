use tempo_agentic_config::SuiNetwork;

pub const MIN_SQRT_PRICE: u128 = 4_295_048_016;
pub const MAX_SQRT_PRICE: u128 = 79_226_673_515_401_279_992_447_579_055;
pub const FEE_RATE_DENOMINATOR: u64 = 1_000_000;

#[derive(Clone, Copy, Debug)]
pub struct NetworkConstants {
    /// The `clmm_pool` package, defining the `Pool<A, B>` object type.
    pub clmm_pool_package_id: &'static str,
    /// The `integrate` package, exposing the client-facing swap entrypoints.
    pub integrate_package_id: &'static str,
    /// The `cetus_config` package.
    pub cetus_config_package_id: &'static str,
    /// Shared `GlobalConfig` object required by swap calls.
    pub global_config_id: &'static str,
    pub clmm_pools_handle: &'static str,
}

pub const TESTNET: NetworkConstants = NetworkConstants {
    clmm_pool_package_id: "0x5372d555ac734e272659136c2a0cd3227f9b92de67c80dc11250307268af2db8",
    integrate_package_id: "0xab2d58dd28ff0dc19b18ab2c634397b785a38c342a8f5065ade5f53f9dbffa1c",
    cetus_config_package_id: "0x2933975c3f74ef7c31f512edead6c6ce3f58f8e8fdbea78770ec8d5abd8ff700",
    global_config_id: "0xc6273f844b4bc258952c4e477697aa12c918c8e08106fac6b934811298c9820a",
    clmm_pools_handle: "0x51f8de2366af49a51ee81184eb28ca24739d3d48c8158d063dab6700c0b65413",
};

/// Returns Cetus IDs; currently only testnet is configured.
pub fn for_network(network: SuiNetwork) -> anyhow::Result<NetworkConstants> {
    match network {
        SuiNetwork::Testnet => Ok(TESTNET),
        SuiNetwork::Mainnet => anyhow::bail!("Cetus venue does not yet support Sui mainnet"),
    }
}
