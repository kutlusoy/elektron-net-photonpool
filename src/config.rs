use anyhow::{bail, Result};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config {
    pub node: NodeCfg,
    pub pool: PoolCfg,
    #[serde(default)]
    pub api: ApiCfg,
    #[serde(default)]
    pub stratum: StratumCfg,
    #[serde(default)]
    pub payout: PayoutCfg,
    #[serde(default)]
    pub ledger: LedgerCfg,
    #[serde(default)]
    pub public: PublicCfg,
    #[serde(default)]
    pub photon: PhotonCfg,
}

/// The public pool page and its read-only API, served on its own listener.
#[derive(Deserialize, Clone)]
pub struct PublicCfg {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "d_pub_listen")]
    pub listen: String,
    #[serde(default = "d_pub_name")]
    pub name: String,
    /// Host name shown in the connection instructions.
    #[serde(default)]
    pub stratum_host: String,
    /// Take the client address from X-Forwarded-For (only behind a trusted reverse proxy).
    #[serde(default)]
    pub trust_proxy: bool,
    #[serde(default = "d_pub_rate")]
    pub rate_per_min: u32,
}

impl Default for PublicCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}

fn d_pub_listen() -> String { "127.0.0.1:8082".into() }
fn d_pub_name() -> String { "Photon Pool".into() }
fn d_pub_rate() -> u32 { 120 }

#[derive(Deserialize, Clone)]
pub struct LedgerCfg {
    #[serde(default = "d_db")]
    pub path: String,
    /// Share history older than this many days is deleted.
    #[serde(default = "d_keep")]
    pub keep_days: i64,
}

impl Default for LedgerCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}

fn d_db() -> String { "photond.db".into() }
fn d_keep() -> i64 { 14 }

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Solo,
    Pplns,
    Pps,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Solo => "solo",
            Mode::Pplns => "pplns",
            Mode::Pps => "pps",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "solo" => Some(Mode::Solo),
            "pplns" => Some(Mode::Pplns),
            "pps" => Some(Mode::Pps),
            _ => None,
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct PayoutCfg {
    #[serde(default = "d_default_mode")]
    pub default_mode: Mode,
    /// Modes this pool offers.
    #[serde(default = "d_modes")]
    pub modes: Vec<Mode>,
    /// Minimum balance before a payout is made, in leptons (1 coin = 100,000,000 leptons).
    #[serde(default = "d_min_payout")]
    pub min_payout_leptons: i64,
    /// Fee in percent per mode. Solo miners are paid directly in the coinbase and carry no fee.
    #[serde(default)]
    pub fee_pct: FeeCfg,
    #[serde(default)]
    pub pplns: PplnsCfg,
    #[serde(default)]
    pub pps: PpsCfg,
    /// Maximum number of distinct solo accounts that get their own candidate per epoch.
    #[serde(default = "d_max_solo")]
    pub max_solo_accounts: usize,
    /// Log what would be paid without sending anything. Turn off to pay for real.
    #[serde(default = "d_true")]
    pub dry_run: bool,
    #[serde(default = "d_interval")]
    pub interval_secs: u64,
    /// Name of the node wallet used for payouts and PPS treasury checks. Empty disables wallet features.
    #[serde(default)]
    pub wallet: String,
    /// Pool payout addresses to provision from the wallet (label "photon-pool") in addition to `pool.payout_addresses`.
    #[serde(default)]
    pub wallet_addresses: usize,
    /// Environment variable holding the wallet passphrase (the wallet is unlocked only while paying).
    #[serde(default = "d_pass_env")]
    pub wallet_passphrase_env: String,
    #[serde(default = "d_maturity_poll")]
    pub maturity_poll_secs: u64,
    #[serde(default = "d_max_outputs")]
    pub max_outputs_per_tx: usize,
    /// Fixed fee rate in sat/vB for payout transactions. Use it when the node cannot estimate fees (small or young networks).
    #[serde(default)]
    pub fee_rate: Option<f64>,
}

#[derive(Deserialize, Clone)]
pub struct FeeCfg {
    #[serde(default = "d_fee_pplns")]
    pub pplns: f64,
    #[serde(default = "d_fee_pps")]
    pub pps: f64,
}

#[derive(Deserialize, Clone)]
pub struct PplnsCfg {
    /// Window size as a multiple of the network difficulty (difficulty-weighted shares).
    #[serde(default = "d_window")]
    pub window_factor: f64,
}

#[derive(Deserialize, Clone)]
pub struct PpsCfg {
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// PPS is paused while the pool wallet holds less than this many coins above all liabilities.
    #[serde(default = "d_reserve")]
    pub reserve_min: f64,
    /// Largest share of the pool hashrate that may mine in PPS mode.
    #[serde(default = "d_pps_share")]
    pub max_hashrate_share: f64,
}

impl Default for PayoutCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}
impl Default for FeeCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}
impl Default for PplnsCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}
impl Default for PpsCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}

impl PayoutCfg {
    pub fn fee(&self, mode: Mode) -> f64 {
        match mode {
            Mode::Solo => 0.0,
            Mode::Pplns => self.fee_pct.pplns,
            Mode::Pps => self.fee_pct.pps,
        }
    }

    pub fn offers(&self, mode: Mode) -> bool {
        self.modes.contains(&mode) && (mode != Mode::Pps || self.pps.enabled)
    }
}

fn d_default_mode() -> Mode { Mode::Pplns }
fn d_modes() -> Vec<Mode> { vec![Mode::Solo, Mode::Pplns, Mode::Pps] }
fn d_min_payout() -> i64 { 100_000 }
fn d_fee_pplns() -> f64 { 1.0 }
fn d_fee_pps() -> f64 { 2.0 }
fn d_window() -> f64 { 2.0 }
fn d_reserve() -> f64 { 5.0 }
fn d_pps_share() -> f64 { 0.25 }
fn d_max_solo() -> usize { 1000 }
fn d_interval() -> u64 { 600 }
fn d_pass_env() -> String { "PHOTON_WALLET_PASSPHRASE".into() }
fn d_maturity_poll() -> u64 { 30 }
fn d_max_outputs() -> usize { 100 }

#[derive(Deserialize)]
pub struct NodeCfg {
    pub rpc_url: String,
    pub auth: String,
    #[serde(default = "d_poll")]
    pub poll_ms: u64,
}

#[derive(Deserialize)]
pub struct PoolCfg {
    pub network: String,
    #[serde(default)]
    pub payout_addresses: Vec<String>,
    #[serde(default = "d_refresh")]
    pub refresh_secs: u64,
    #[serde(default = "d_spot")]
    pub spot_check: usize,
    /// How candidates are fetched: "auto" (batch attestation when the node supports it, else one getblocktemplate call per address), "batch" or "gbt".
    #[serde(default = "d_backend")]
    pub backend: String,
}

#[derive(Deserialize)]
pub struct ApiCfg {
    #[serde(default = "d_listen")]
    pub listen: String,
    #[serde(default = "d_ui")]
    pub ui_dir: String,
    /// Base URL of a mempool-style block explorer, used to link found blocks. Empty disables links.
    #[serde(default)]
    pub explorer_url: String,
}

impl Default for ApiCfg {
    fn default() -> Self {
        Self { listen: d_listen(), ui_dir: d_ui(), explorer_url: String::new() }
    }
}

#[derive(Deserialize, Clone)]
pub struct StratumCfg {
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default = "d_s_listen")]
    pub listen: String,
    #[serde(default = "d_s_diff")]
    pub initial_difficulty: f64,
    #[serde(default = "d_s_min_diff")]
    pub min_difficulty: f64,
    #[serde(default = "d_s_max_diff")]
    pub max_difficulty: f64,
    #[serde(default = "d_s_share_secs")]
    pub share_secs: f64,
    #[serde(default = "d_s_max_conn")]
    pub max_connections: usize,
    #[serde(default = "d_s_idle")]
    pub idle_secs: u64,
    #[serde(default = "d_s_min_job")]
    pub min_job_interval_ms: u64,
    #[serde(default = "d_s_max_job")]
    pub max_job_interval_ms: u64,
    #[serde(default = "d_true")]
    pub require_address: bool,
    /// Extra listeners with their own default mode. When empty, `listen` is used with the global default mode.
    #[serde(default)]
    pub ports: Vec<PortCfg>,
}

#[derive(Deserialize, Clone)]
pub struct PortCfg {
    pub listen: String,
    pub mode: Mode,
    /// Whether the miner may choose another mode with `m=<mode>` in the password field.
    #[serde(default)]
    pub allow_override: bool,
}

impl Default for StratumCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}

fn d_true() -> bool { true }
fn d_s_listen() -> String { "0.0.0.0:3333".into() }
fn d_s_diff() -> f64 { 1024.0 }
fn d_s_min_diff() -> f64 { 0.0001 }
fn d_s_max_diff() -> f64 { 1.0e12 }
fn d_s_share_secs() -> f64 { 10.0 }
fn d_s_max_conn() -> usize { 200_000 }
fn d_s_idle() -> u64 { 300 }
fn d_s_min_job() -> u64 { 200 }
fn d_s_max_job() -> u64 { 30_000 }

fn d_poll() -> u64 { 100 }
fn d_refresh() -> u64 { 20 }
fn d_spot() -> usize { 4 }
fn d_backend() -> String { "auto".into() }
fn d_listen() -> String { "127.0.0.1:8081".into() }
fn d_ui() -> String { "ui".into() }

impl ApiCfg {
    fn normalize_explorer(&mut self) -> Result<()> {
        let url = self.explorer_url.trim().trim_end_matches('/').to_string();
        if !url.is_empty() && !(url.starts_with("https://") || url.starts_with("http://")) {
            bail!("api.explorer_url must start with http:// or https://");
        }
        if url.contains(['"', '\'', '<', '>', ' ']) {
            bail!("api.explorer_url contains invalid characters");
        }
        self.explorer_url = url;
        Ok(())
    }
}

pub struct Network {
    pub hrp: &'static str,
    pub chain: &'static str,
    pub min_height: u32,
}

impl Config {
    /// Effective listeners: the configured ports, or `listen` with the default mode.
    pub fn stratum_ports(&self) -> Vec<PortCfg> {
        if self.stratum.ports.is_empty() {
            vec![PortCfg { listen: self.stratum.listen.clone(), mode: self.payout.default_mode, allow_override: true }]
        } else {
            self.stratum.ports.clone()
        }
    }

    pub fn load(path: &std::path::Path) -> Result<Self> {
        let mut cfg: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
        if cfg.pool.payout_addresses.is_empty() && !(cfg.payout.wallet_addresses > 0 && !cfg.payout.wallet.is_empty()) {
            bail!("set pool.payout_addresses, or payout.wallet together with payout.wallet_addresses");
        }
        cfg.network()?;
        if !matches!(cfg.pool.backend.as_str(), "auto" | "batch" | "gbt") {
            bail!("pool.backend must be auto, batch or gbt");
        }
        cfg.api.normalize_explorer()?;
        if cfg.payout.modes.is_empty() {
            bail!("payout.modes must offer at least one mode");
        }
        if !cfg.payout.offers(cfg.payout.default_mode) {
            bail!("payout.default_mode {} is not offered", cfg.payout.default_mode.as_str());
        }
        for p in cfg.stratum_ports() {
            if !cfg.payout.offers(p.mode) {
                bail!("stratum port {} uses mode {} which is not offered", p.listen, p.mode.as_str());
            }
        }
        for f in [cfg.payout.fee_pct.pplns, cfg.payout.fee_pct.pps] {
            if !(0.0..=50.0).contains(&f) {
                bail!("payout.fee_pct values must be between 0 and 50");
            }
        }
        if cfg.payout.pplns.window_factor <= 0.0 {
            bail!("payout.pplns.window_factor must be positive");
        }
        Ok(cfg)
    }

    pub fn network(&self) -> Result<Network> {
        Ok(match self.pool.network.as_str() {
            "mainnet" => Network { hrp: "be", chain: "main", min_height: 170_000 },
            "testnet" => Network { hrp: "tb", chain: "test", min_height: 260 },
            "regtest" => Network { hrp: "bcrt", chain: "regtest", min_height: 110 },
            other => bail!("unknown network {other}"),
        })
    }
}


/// Photon protocol listener (gateways with their own node; non-custodial coinbase payouts).
#[derive(Deserialize, Clone)]
pub struct PhotonCfg {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "d_ph_listen")]
    pub listen: String,
    /// File with the hex-encoded Noise static private key; created on first start. Gateways pin the matching public key.
    #[serde(default = "d_ph_key")]
    pub key_file: String,
    /// Output that receives the pool fee; empty uses the first payout address.
    #[serde(default)]
    pub fee_address: String,
    #[serde(default = "d_ph_fee")]
    pub fee_pct: f64,
    #[serde(default = "d_ph_nmax")]
    pub max_outputs: u16,
    /// Smallest payout part in parts per million of the reward.
    #[serde(default = "d_ph_minppm")]
    pub min_ppm: u32,
    /// Smallest payout output in leptons (measured at the subsidy). Smaller credits get no output; keeps the UTXO set free of dust.
    #[serde(default = "d_ph_minlep")]
    pub min_output_leptons: u64,
    #[serde(default = "d_ph_window")]
    pub window_factor: f64,
    #[serde(default = "d_s_diff")]
    pub initial_difficulty: f64,
    #[serde(default = "d_s_min_diff")]
    pub min_difficulty: f64,
    #[serde(default = "d_s_max_diff")]
    pub max_difficulty: f64,
    /// Target seconds between shares of one channel.
    #[serde(default = "d_s_share_secs")]
    pub share_secs: f64,
    #[serde(default = "d_ph_conn")]
    pub max_connections: usize,
    /// Connections from one address (IPv6: one /64).
    #[serde(default = "d_ph_conn_ip")]
    pub max_connections_per_ip: usize,
    #[serde(default = "d_ph_chan")]
    pub max_channels_per_connection: usize,
    /// Announced coinbase value may exceed the subsidy by this many percent (fees).
    #[serde(default = "d_ph_bound")]
    pub max_reward_over_subsidy_pct: f64,
    /// Upper bound of the transaction data accepted for one spot check.
    #[serde(default = "d_ph_check_bytes")]
    pub max_check_bytes: usize,
    #[serde(default = "d_ph_idle")]
    pub idle_secs: u64,
    /// Attestation spot checks the pool starts per tip, over all gateways. Each one costs the node a full block validation.
    #[serde(default = "d_ph_budget")]
    pub max_checks_per_tip: u32,
    /// Spot checks validated by the node at the same time.
    #[serde(default = "d_ph_conc")]
    pub max_concurrent_checks: usize,
}

impl Default for PhotonCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}

fn d_ph_listen() -> String { "0.0.0.0:3340".into() }
fn d_ph_key() -> String { "photon.key".into() }
fn d_ph_fee() -> f64 { 1.0 }
fn d_ph_nmax() -> u16 { 128 }
fn d_ph_minppm() -> u32 { 100 }
fn d_ph_minlep() -> u64 { 100_000 }
fn d_ph_window() -> f64 { 2.0 }
fn d_ph_conn() -> usize { 1000 }
fn d_ph_conn_ip() -> usize { 32 }
fn d_ph_chan() -> usize { 64 }
fn d_ph_bound() -> f64 { 100.0 }
fn d_ph_check_bytes() -> usize { 8 * 1024 * 1024 }
fn d_ph_idle() -> u64 { 300 }
fn d_ph_budget() -> u32 { 8 }
fn d_ph_conc() -> usize { 2 }
