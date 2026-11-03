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
}

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
    pub payout_addresses: Vec<String>,
    #[serde(default = "d_refresh")]
    pub refresh_secs: u64,
    #[serde(default = "d_spot")]
    pub spot_check: usize,
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
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let mut cfg: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
        if cfg.pool.payout_addresses.is_empty() {
            bail!("pool.payout_addresses must not be empty");
        }
        cfg.network()?;
        cfg.api.normalize_explorer()?;
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
