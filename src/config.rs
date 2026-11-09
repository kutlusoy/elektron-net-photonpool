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
    #[serde(default)]
    pub registry: RegistryCfg,
}

/// Block attribution in the Elektron Net registry (explorers show which pool found a block).
#[derive(Deserialize, Clone)]
pub struct RegistryCfg {
    /// Report found blocks to the explorers listed in the registry (mainnet only, unless `url` is changed).
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// Base url of the registry files (raw content, no trailing slash).
    #[serde(default = "d_reg_url")]
    pub url: String,
    /// The Name of this pool's line in pools.txt; empty uses [public] name.
    #[serde(default)]
    pub name: String,
}
impl Default for RegistryCfg { fn default() -> Self { Self { enabled: true, url: d_reg_url(), name: String::new() } } }
fn d_reg_url() -> String { crate::registry::DEFAULT_URL.into() }

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
    /// Pool statistics (one sample per minute: hash rate, miners, shares) are kept for this many days and then deleted; the public pages draw their history from them.
    #[serde(default = "d_stats_keep")]
    pub stats_keep_days: i64,
    /// Hash rate and working connections per device type are stored once per this many minutes (the pool-wide history is stored every minute).
    #[serde(default = "d_dev_slot")]
    pub device_history_minutes: i64,
}

impl Default for LedgerCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}

fn d_db() -> String { "photond.db".into() }
fn d_keep() -> i64 { 14 }
fn d_stats_keep() -> i64 { 60 }
fn d_dev_slot() -> i64 { 5 }

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
    /// Fixed fee rate in sat/vB for payout transactions; wins over everything else.
    #[serde(default)]
    pub fee_rate: Option<f64>,
    /// Without a fixed rate: ask the explorer (api explorer_url, /api/v1/fees/recommended) for the fee rate.
    #[serde(default = "d_true")]
    pub fee_from_explorer: bool,
    /// Fee rate in sat/vB when the explorer cannot be asked; 0 lets the node estimate (which fails on a chain without estimates).
    #[serde(default = "d_fee_fallback")]
    pub fee_rate_fallback: f64,
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
    /// Use only as many of the pool's payout addresses as the work demand needs (each one is a candidate with its own work units); more are added when miners use them up.
    #[serde(default = "d_true")]
    pub autoscale_candidates: bool,
    /// Fewest candidates in use while autoscaling.
    #[serde(default = "d_min_cand")]
    pub min_candidates: usize,
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
    /// Folder with audio files for the optional background music of the web pages. Empty: "music" inside ui_dir.
    #[serde(default)]
    pub music_dir: String,
}

impl Default for ApiCfg {
    fn default() -> Self {
        Self { listen: d_listen(), ui_dir: d_ui(), explorer_url: String::new(), music_dir: String::new() }
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
    /// Most jobs per second the daemon sends in total. The shortest interval of one connection grows with the number of connections so that this budget is
    /// never exceeded: a few fast miners get the shortest interval, a big farm a longer one.
    #[serde(default = "d_s_budget")]
    pub max_jobs_per_sec: u64,
    #[serde(default = "d_s_max_job")]
    pub max_job_interval_ms: u64,
    /// How many of the granted version bits a miner really sweeps per job (2^(32+n) hashes). Sets how often fresh work is sent.
    #[serde(default = "d_s_space")]
    pub version_space_bits: u32,
    /// Miners whose user agent contains one of these words (case-insensitive) get every fresh job marked clean. "*" matches every miner.
    #[serde(default = "d_s_clean_agents")]
    pub clean_work_agents: Vec<String>,
    /// Miners whose user agent contains one of these words (case-insensitive) get a non-empty extranonce1 in the subscribe answer. Some hobby firmware aborts on an empty one;
    /// the value is a plain label for the connection and never part of the coinbase (extranonce2_size stays 0).
    #[serde(default = "d_s_label_agents")]
    pub label_extranonce_agents: Vec<String>,
    /// Miners whose user agent contains one of these words (case-insensitive) get extranonce space: extranonce1 (4 bytes, a label of the connection) and
    /// extranonce2_size 4, which the pool places behind the attestation hash inside the attestation output (the node ignores bytes there). Firmware derived from
    /// cgminer (Antminer, Whatsminer, Avalon) refuses a size of 0.
    #[serde(default = "d_s_en_agents")]
    pub extranonce_agents: Vec<String>,
    /// Learn the job interval of every connection from its duplicate and stale shares: faster when the miner repeats work, slower again when it does not.
    #[serde(default = "d_true")]
    pub adaptive_jobs: bool,
    /// Shortest job interval the learning may reach, in milliseconds.
    #[serde(default = "d_s_adapt_floor")]
    pub adaptive_min_interval_ms: u64,
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
fn d_fee_fallback() -> f64 { 2.0 }
fn d_s_listen() -> String { "0.0.0.0:3333".into() }
fn d_s_diff() -> f64 { 1024.0 }
fn d_s_min_diff() -> f64 { 0.0001 }
fn d_s_max_diff() -> f64 { 1.0e12 }
fn d_s_share_secs() -> f64 { 10.0 }
fn d_s_max_conn() -> usize { 200_000 }
fn d_s_idle() -> u64 { 300 }
fn d_s_min_job() -> u64 { 200 }
fn d_s_budget() -> u64 { 50_000 }
fn d_s_max_job() -> u64 { 30_000 }
fn d_s_space() -> u32 { 5 }
fn d_min_cand() -> usize { 16 }
fn d_s_adapt_floor() -> u64 { 50 }
fn d_s_en_agents() -> Vec<String> { ["cgminer", "bmminer", "antminer", "whatsminer", "btminer", "bfgminer", "sgminer", "cpuminer", "avalon", "bosminer", "braiins", "vnish"].iter().map(|s| s.to_string()).collect() }
fn d_s_label_agents() -> Vec<String> { vec!["nerdminer".into(), "han_solominer".into()] }
fn d_s_clean_agents() -> Vec<String> { vec!["bitaxe".into(), "esp-miner".into()] }

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
        // The Docker setup knows where its node is; these override a config copied from the bare-metal example.
        if let Ok(v) = std::env::var("PHOTOND_NODE_RPC_URL") { if !v.is_empty() { cfg.node.rpc_url = v; } }
        if let Ok(v) = std::env::var("PHOTOND_NODE_AUTH") { if !v.is_empty() { cfg.node.auth = v; } }
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
fn d_ph_nmax() -> u16 { 200 }
fn d_ph_window() -> f64 { 2.0 }
fn d_ph_conn() -> usize { 1000 }
fn d_ph_conn_ip() -> usize { 32 }
fn d_ph_chan() -> usize { 64 }
fn d_ph_bound() -> f64 { 100.0 }
fn d_ph_check_bytes() -> usize { 8 * 1024 * 1024 }
fn d_ph_idle() -> u64 { 300 }
fn d_ph_budget() -> u32 { 8 }
fn d_ph_conc() -> usize { 2 }

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn root(p: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(p)
    }

    /// Field names of `pub struct <name> { ... }` in a source file.
    fn fields(src: &str, name: &str) -> Vec<String> {
        let start = src.find(&format!("pub struct {name} {{")).unwrap_or_else(|| panic!("struct {name} not found"));
        src[start..]
            .lines()
            .skip(1)
            .take_while(|l| *l != "}")
            .filter_map(|l| l.trim().strip_prefix("pub ").and_then(|r| r.split(':').next()).map(str::to_string))
            .collect()
    }

    /// True when the file has the option as `name = ...`, commented out or not, or as a `[name]` section.
    fn documents(text: &str, name: &str) -> bool {
        text.lines().any(|l| {
            let l = l.trim().trim_start_matches('#').trim();
            l.starts_with(&format!("{name} =")) || l.starts_with(&format!("{name}=")) || l.starts_with(&format!("[{name}]")) || l.starts_with(&format!("[[stratum.{name}]]")) || l.contains(&format!("[payout.{name}]"))
        })
    }

    #[test]
    fn the_example_configs_load() {
        Config::load(&root("photond.example.toml")).unwrap();
        Config::load(&root("docker/photond.docker.toml")).unwrap();
        let gw: crate::photon::gateway::GatewayCfg = toml::from_str(&std::fs::read_to_string(root("gateway.example.toml")).unwrap()).unwrap();
        assert_eq!(gw.max_outputs, 200);
    }

    #[test]
    fn every_pool_option_is_documented_in_the_example() {
        let text = std::fs::read_to_string(root("photond.example.toml")).unwrap();
        let src = include_str!("config.rs");
        let mut missing = Vec::new();
        for s in ["NodeCfg", "PoolCfg", "ApiCfg", "StratumCfg", "PortCfg", "PayoutCfg", "FeeCfg", "PplnsCfg", "PpsCfg", "PublicCfg", "LedgerCfg", "PhotonCfg"] {
            for f in fields(src, s) {
                if !documents(&text, &f) {
                    missing.push(format!("{s}.{f}"));
                }
            }
        }
        assert!(missing.is_empty(), "options missing in photond.example.toml: {missing:?}");
    }

    #[test]
    fn every_gateway_option_is_documented_in_the_example() {
        let text = std::fs::read_to_string(root("gateway.example.toml")).unwrap();
        let mut missing = Vec::new();
        for f in fields(include_str!("photon/gateway.rs"), "GatewayCfg") {
            if !documents(&text, &f) {
                missing.push(format!("GatewayCfg.{f}"));
            }
        }
        for f in fields(include_str!("photon/gateway_stratum.rs"), "StratumGwCfg") {
            if !documents(&text, &f) {
                missing.push(format!("stratum.{f}"));
            }
        }
        assert!(missing.is_empty(), "options missing in gateway.example.toml: {missing:?}");
    }

    #[test]
    fn the_defaults_written_in_the_example_are_the_real_defaults() {
        let example = Config::load(&root("photond.example.toml")).unwrap();
        let d: Config = toml::from_str("[node]\nrpc_url = \"x\"\nauth = \"y\"\n[pool]\nnetwork = \"mainnet\"\n").unwrap();
        macro_rules! same {
            ($($($a:ident).+),+ $(,)?) => { $( assert_eq!(format!("{:?}", example.$($a).+), format!("{:?}", d.$($a).+), stringify!($($a).+)); )+ };
        }
        same!(node.poll_ms, pool.refresh_secs, pool.spot_check, pool.backend, pool.autoscale_candidates, pool.min_candidates);
        same!(stratum.enabled, stratum.listen, stratum.initial_difficulty, stratum.min_difficulty, stratum.max_difficulty, stratum.share_secs, stratum.max_connections, stratum.idle_secs, stratum.min_job_interval_ms, stratum.max_jobs_per_sec, stratum.max_job_interval_ms, stratum.version_space_bits, stratum.clean_work_agents, stratum.label_extranonce_agents, stratum.extranonce_agents, stratum.adaptive_jobs, stratum.adaptive_min_interval_ms, stratum.require_address);
        same!(payout.default_mode, payout.modes, payout.min_payout_leptons, payout.max_solo_accounts, payout.dry_run, payout.interval_secs, payout.wallet, payout.wallet_addresses, payout.wallet_passphrase_env, payout.maturity_poll_secs, payout.max_outputs_per_tx, payout.fee_rate, payout.fee_from_explorer, payout.fee_rate_fallback);
        same!(payout.fee_pct.pplns, payout.fee_pct.pps, payout.pplns.window_factor, payout.pps.enabled, payout.pps.reserve_min, payout.pps.max_hashrate_share);
        same!(registry.enabled, registry.url, registry.name);
        same!(public.enabled, public.listen, public.name, public.stratum_host, public.trust_proxy, public.rate_per_min);
        same!(ledger.path, ledger.keep_days, ledger.stats_keep_days, ledger.device_history_minutes, api.listen, api.ui_dir);
        same!(photon.enabled, photon.listen, photon.key_file, photon.fee_pct, photon.max_outputs, photon.window_factor, photon.initial_difficulty, photon.min_difficulty, photon.max_difficulty, photon.share_secs, photon.max_connections, photon.max_connections_per_ip, photon.max_channels_per_connection, photon.max_reward_over_subsidy_pct, photon.max_check_bytes, photon.idle_secs, photon.max_checks_per_tip, photon.max_concurrent_checks);
    }
}
