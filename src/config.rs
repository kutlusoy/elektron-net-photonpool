use anyhow::{bail, Result};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config {
    pub node: NodeCfg,
    pub pool: PoolCfg,
    #[serde(default)]
    pub api: ApiCfg,
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
}

impl Default for ApiCfg {
    fn default() -> Self {
        Self { listen: d_listen(), ui_dir: d_ui() }
    }
}

fn d_poll() -> u64 { 100 }
fn d_refresh() -> u64 { 20 }
fn d_spot() -> usize { 4 }
fn d_listen() -> String { "127.0.0.1:8081".into() }
fn d_ui() -> String { "ui".into() }

pub struct Network {
    pub hrp: &'static str,
    pub chain: &'static str,
    pub min_height: u32,
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let cfg: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
        if cfg.pool.payout_addresses.is_empty() {
            bail!("pool.payout_addresses must not be empty");
        }
        cfg.network()?;
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
