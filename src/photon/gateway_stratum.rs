//! Stratum V1 front end of the Photon gateway: ASICs and farm proxies attach here.
//!
//! Work units are (variant, nTime offset, version slice) of the gateway's own template, as in the pool's Stratum server
//! (empty extranonce1, extranonce2_size 0). Shares that meet the pool channel's target are forwarded to the pool,
//! blocks are submitted to the gateway's own node.
use super::msg::ShareItem;
use crate::addr::script_for_address;
use super::verify::VERSION_MASK;
use crate::diff::{hashes_per_share, target_from_difficulty};
use crate::pool::{Epoch, ShareResult, MINER_VERSION_MASK, POOL_VERSION_SHIFT};
use crate::rpc::NodeRpc;
use crate::template::{header_hash, meets_target};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};
use tokio::time::{sleep_until, timeout, Instant as TokioInstant};

const MAX_LINE: usize = 8192;
/// Longest JSON line that ESP-Miner (Bitaxe and other ESP32 based miners) accepts: `STRATUM_V1_MAX_JSON_LINE_SIZE`.
pub const ESP_MAX_LINE: usize = 16_384;
static OVERSIZED_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Size of a `mining.notify` line for a coinbase with `p2wpkh_outputs` payout outputs and a merkle branch of `branch_depth` hashes:
/// the coinbase is sent in hex (two characters per byte), plus about 260 bytes of fixed text and 68 bytes per branch hash.
pub fn notify_size_estimate(p2wpkh_outputs: usize, branch_depth: usize) -> usize {
    let coinbase = 31 * p2wpkh_outputs + 200;
    2 * coinbase + 260 + 68 * branch_depth
}
const MAX_JOBS: usize = 16;
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const PREV_TARGET_GRACE: Duration = Duration::from_secs(60);

#[derive(Deserialize, Clone)]
pub struct StratumGwCfg {
    /// Address for ASICs, for example `0.0.0.0:3333`. Empty disables the front end.
    #[serde(default)]
    pub listen: String,
    #[serde(default = "d_diff")]
    pub initial_difficulty: f64,
    #[serde(default = "d_min")]
    pub min_difficulty: f64,
    #[serde(default = "d_max")]
    pub max_difficulty: f64,
    /// Target seconds between shares of one ASIC.
    #[serde(default = "d_secs")]
    pub share_secs: f64,
    #[serde(default = "d_conn")]
    pub max_connections: usize,
    #[serde(default = "d_idle")]
    pub idle_secs: u64,
    #[serde(default = "d_minjob")]
    pub min_job_interval_ms: u64,
    #[serde(default = "d_maxjob")]
    pub max_job_interval_ms: u64,
    /// Refuse users that are not `<payout address>.<worker>`. Otherwise they mine for the gateway's default payout address.
    #[serde(default)]
    pub require_address: bool,
}

impl Default for StratumGwCfg {
    fn default() -> Self {
        toml::from_str("").unwrap()
    }
}

fn d_diff() -> f64 { 1024.0 }
fn d_min() -> f64 { 0.0001 }
fn d_max() -> f64 { 1.0e12 }
fn d_secs() -> f64 { 10.0 }
fn d_conn() -> usize { 20_000 }
fn d_idle() -> u64 { 600 }
fn d_minjob() -> u64 { 200 }
fn d_maxjob() -> u64 { 30_000 }

/// What the front end hands to the gateway's pool connection.
pub enum Fwd {
    /// A worker needs a channel for this payout script; the gateway opens it at the pool (or answers from its cache).
    Open { script: Vec<u8>, reply: oneshot::Sender<std::result::Result<u32, String>> },
    Share { channel: u32, template_id: u32, item: ShareItem },
    Block { template_id: u32, variant: u32, height: u32, hash: [u8; 32] },
}

#[derive(Default)]
pub struct GwStats {
    pub sessions: AtomicU64,
    pub ok: AtomicU64,
    pub stale: AtomicU64,
    pub dup: AtomicU64,
    pub invalid: AtomicU64,
    pub forwarded: AtomicU64,
    pub blocks: AtomicU64,
}

pub struct Shared {
    pub epoch: watch::Receiver<Option<Arc<Epoch>>>,
    /// Share target of every open pool channel; shares meeting their channel's target are forwarded.
    pub targets: Arc<RwLock<HashMap<u32, [u8; 32]>>>,
    /// Address prefix of the network, for parsing `<address>.<worker>` users.
    pub hrp: String,
    /// Payout script of users that do not name an address.
    pub default_script: Vec<u8>,
    pub fwd: mpsc::UnboundedSender<Fwd>,
    pub node: Arc<NodeRpc>,
    pub stats: GwStats,
}

pub async fn serve(cfg: StratumGwCfg, shared: Arc<Shared>) -> Result<()> {
    let listener = TcpListener::bind(&cfg.listen).await?;
    tracing::info!("gateway stratum listening on {}", cfg.listen);
    let cfg = Arc::new(cfg);
    let permits = Arc::new(Semaphore::new(cfg.max_connections));
    tokio::spawn(report(shared.clone()));
    let mut id = 0u64;
    loop {
        let (tcp, _) = listener.accept().await?;
        let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
        tcp.set_nodelay(true).ok();
        id += 1;
        let (cfg, shared) = (cfg.clone(), shared.clone());
        tokio::spawn(async move {
            shared.stats.sessions.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = Session::new(cfg, shared.clone(), id).run(tcp).await {
                tracing::debug!("asic session {id}: {e:#}");
            }
            shared.stats.sessions.fetch_sub(1, Ordering::Relaxed);
            drop(permit);
        });
    }
}

async fn report(shared: Arc<Shared>) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.tick().await;
    loop {
        tick.tick().await;
        let s = &shared.stats;
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        tracing::info!("gateway stratum: {} ASIC sessions, shares ok {} stale {} dup {} invalid {}, forwarded to pool {}, blocks {}", g(&s.sessions), g(&s.ok), g(&s.stale), g(&s.dup), g(&s.invalid), g(&s.forwarded), g(&s.blocks));
    }
}

struct JobRef {
    id: u64,
    epoch: Arc<Epoch>,
    cand: usize,
    slice: u8,
    version: u32,
}

struct Session {
    cfg: Arc<StratumGwCfg>,
    sh: Arc<Shared>,
    id: u64,
    epoch_rx: watch::Receiver<Option<Arc<Epoch>>>,
    subscribed: bool,
    authorized: bool,
    started: bool,
    worker: String,
    channel: u32,
    difficulty: f64,
    target: [u8; 32],
    prev: Option<(f64, [u8; 32], Instant)>,
    mask: u32,
    rolling_bits: u32,
    jobs: VecDeque<JobRef>,
    last_tip: String,
    next_job: Instant,
    window_start: Instant,
    window_diff: f64,
    hash_est: f64,
    last_adjust: Instant,
    shares_since_adjust: u32,
    bad: u32,
}

#[derive(Deserialize)]
struct Req<'a> {
    id: Option<Value>,
    method: &'a str,
    #[serde(default)]
    params: Value,
}

async fn send(wr: &mut OwnedWriteHalf, data: &[u8]) -> Result<()> {
    timeout(WRITE_TIMEOUT, wr.write_all(data)).await.map_err(|_| anyhow!("write timeout"))??;
    Ok(())
}

async fn read_chunk(rd: &mut BufReader<OwnedReadHalf>, buf: &mut Vec<u8>) -> Result<Option<bool>> {
    if buf.len() > MAX_LINE {
        bail!("line too long");
    }
    let limit = (MAX_LINE + 1 - buf.len()) as u64;
    let n = (&mut *rd).take(limit).read_until(b'\n', buf).await?;
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(buf.last() == Some(&b'\n')))
}

impl Session {
    fn new(cfg: Arc<StratumGwCfg>, sh: Arc<Shared>, id: u64) -> Self {
        let now = Instant::now();
        let difficulty = cfg.initial_difficulty.clamp(cfg.min_difficulty, cfg.max_difficulty);
        let epoch_rx = sh.epoch.clone();
        Self {
            cfg, sh, id, epoch_rx, subscribed: false, authorized: false, started: false, worker: String::new(), channel: 0, difficulty, target: target_from_difficulty(difficulty), prev: None, mask: 0,
            rolling_bits: 0, jobs: VecDeque::new(), last_tip: String::new(), next_job: now + Duration::from_secs(3600), window_start: now, window_diff: 0.0, hash_est: 0.0, last_adjust: now,
            shares_since_adjust: 0, bad: 0,
        }
    }

    async fn run(mut self, stream: TcpStream) -> Result<()> {
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::with_capacity(4096, rd);
        let mut buf: Vec<u8> = Vec::with_capacity(512);
        let idle = Duration::from_secs(self.cfg.idle_secs);
        let mut deadline = Instant::now() + idle;
        loop {
            let next_job = TokioInstant::from_std(self.next_job);
            let started = self.started;
            tokio::select! {
                r = read_chunk(&mut rd, &mut buf) => match r? {
                    None => return Ok(()),
                    Some(false) => {}
                    Some(true) => {
                        deadline = Instant::now() + idle;
                        let line = std::mem::take(&mut buf);
                        self.handle_line(&mut wr, &line).await?;
                        buf = line;
                        buf.clear();
                    }
                },
                ch = self.epoch_rx.changed() => {
                    if ch.is_err() { return Ok(()); }
                    if self.started { self.send_job(&mut wr, false).await?; }
                }
                _ = sleep_until(next_job), if started => self.send_job(&mut wr, false).await?,
                _ = sleep_until(TokioInstant::from_std(deadline)) => bail!("idle timeout"),
            }
        }
    }

    async fn reply(&self, wr: &mut OwnedWriteHalf, id: &str, result: &str, error: &str) -> Result<()> {
        send(wr, format!("{{\"id\":{id},\"result\":{result},\"error\":{error}}}\n").as_bytes()).await
    }

    async fn error(&self, wr: &mut OwnedWriteHalf, id: &str, code: u32, msg: &str) -> Result<()> {
        self.reply(wr, id, "null", &format!("[{code},\"{msg}\",null]")).await
    }

    async fn handle_line(&mut self, wr: &mut OwnedWriteHalf, line: &[u8]) -> Result<()> {
        let Ok(req) = serde_json::from_slice::<Req>(line) else {
            self.bad += 1;
            if self.bad > 10 {
                bail!("too many malformed messages");
            }
            return Ok(());
        };
        let id = req.id.as_ref().map(|v| v.to_string()).unwrap_or_else(|| "null".into());
        match req.method {
            "mining.subscribe" => {
                self.subscribed = true;
                self.reply(wr, &id, &format!("[[[\"mining.notify\",\"{:x}\"]],\"\",0]", self.id), "null").await?;
                self.maybe_start(wr).await
            }
            "mining.configure" => self.configure(wr, &id, &req.params).await,
            "mining.authorize" => self.authorize(wr, &id, &req.params).await,
            "mining.suggest_difficulty" => {
                if let Some(d) = req.params.get(0).and_then(Value::as_f64) {
                    self.set_difficulty(wr, d).await?;
                }
                Ok(())
            }
            "mining.extranonce.subscribe" => self.reply(wr, &id, "false", "null").await,
            "mining.submit" => self.submit(wr, &id, &req.params).await,
            _ => self.error(wr, &id, 20, "unknown method").await,
        }
    }

    async fn configure(&mut self, wr: &mut OwnedWriteHalf, id: &str, params: &Value) -> Result<()> {
        let mut result = serde_json::Map::new();
        for ext in params.get(0).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if ext == "version-rolling" {
                let opts = params.get(1);
                let asked = opts.and_then(|o| o.get("version-rolling.mask")).and_then(Value::as_str).and_then(|m| u32::from_str_radix(m, 16).ok()).unwrap_or(u32::MAX);
                let min_bits = opts.and_then(|o| o.get("version-rolling.min-bit-count")).and_then(Value::as_u64).unwrap_or(0) as u32;
                let mask = asked & MINER_VERSION_MASK;
                if mask != 0 && mask.count_ones() >= min_bits {
                    self.mask = mask;
                    self.rolling_bits = mask.count_ones();
                    result.insert("version-rolling".into(), json!(true));
                    result.insert("version-rolling.mask".into(), json!(format!("{mask:08x}")));
                } else {
                    result.insert("version-rolling".into(), json!(false));
                }
            } else {
                result.insert(ext.to_string(), json!(false));
            }
        }
        self.reply(wr, id, &Value::Object(result).to_string(), "null").await
    }

    /// User `<payout address>.<worker>`: the address selects the pool channel (and therefore who is credited), anything else uses the default one.
    async fn authorize(&mut self, wr: &mut OwnedWriteHalf, id: &str, params: &Value) -> Result<()> {
        let user: String = params.get(0).and_then(Value::as_str).unwrap_or("worker").chars().take(128).collect();
        let (account, worker) = user.split_once('.').unwrap_or((user.as_str(), "default"));
        let script = match script_for_address(account, &self.sh.hrp) {
            Ok(s) => s,
            Err(_) if !self.cfg.require_address => self.sh.default_script.clone(),
            Err(_) => {
                self.bad += 1;
                if self.bad > 10 {
                    bail!("too many failed authorizations");
                }
                return self.reply(wr, id, "false", "[24,\"User must be <payout address>.<worker>\",null]").await;
            }
        };
        let (tx, rx) = oneshot::channel();
        if self.sh.fwd.send(Fwd::Open { script, reply: tx }).is_err() {
            bail!("gateway is shutting down");
        }
        match timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(Ok(ch))) => {
                self.channel = ch;
                self.worker = worker.chars().take(64).collect();
                self.authorized = true;
                self.reply(wr, id, "true", "null").await?;
                self.maybe_start(wr).await
            }
            Ok(Ok(Err(why))) => self.reply(wr, id, "false", &format!("[20,{},null]", json!(why))).await,
            _ => self.reply(wr, id, "false", "[20,\"Pool did not open a channel\",null]").await,
        }
    }

    async fn maybe_start(&mut self, wr: &mut OwnedWriteHalf) -> Result<()> {
        if self.started || !self.subscribed || !self.authorized {
            return Ok(());
        }
        self.started = true;
        self.send_difficulty(wr).await?;
        self.send_job(wr, true).await
    }

    async fn send_difficulty(&mut self, wr: &mut OwnedWriteHalf) -> Result<()> {
        send(wr, format!("{{\"id\":null,\"method\":\"mining.set_difficulty\",\"params\":[{}]}}\n", self.difficulty).as_bytes()).await
    }

    async fn set_difficulty(&mut self, wr: &mut OwnedWriteHalf, d: f64) -> Result<()> {
        let d = d.clamp(self.cfg.min_difficulty, self.cfg.max_difficulty);
        if (d - self.difficulty).abs() < f64::EPSILON {
            return Ok(());
        }
        self.prev = Some((self.difficulty, self.target, Instant::now()));
        self.difficulty = d;
        self.target = target_from_difficulty(d);
        if self.started {
            self.send_difficulty(wr).await?;
        }
        Ok(())
    }

    fn schedule_next(&mut self) {
        let unit_hashes = 2f64.powi(32 + self.rolling_bits as i32);
        let secs = if self.hash_est > 0.0 { unit_hashes / self.hash_est * 0.8 } else { 1.0 };
        let ms = (secs * 1000.0).clamp(self.cfg.min_job_interval_ms as f64, self.cfg.max_job_interval_ms as f64);
        self.next_job = Instant::now() + Duration::from_millis(ms as u64);
    }

    async fn send_job(&mut self, wr: &mut OwnedWriteHalf, force_clean: bool) -> Result<()> {
        let Some(epoch) = self.epoch_rx.borrow().clone() else {
            self.schedule_next();
            return Ok(());
        };
        let clean = force_clean || epoch.tip_hex != self.last_tip;
        let (n, unit) = epoch.next_unit();
        if epoch.tip_hex != self.last_tip {
            self.last_tip = epoch.tip_hex.clone();
        }
        let cand = &epoch.candidates[unit.cand];
        let version = cand.version | ((unit.slice as u32) << POOL_VERSION_SHIFT);
        let ntime = cand.curtime.max(cand.mintime) + unit.ntime_off;
        let id = (epoch.id << 32) | n as u64;
        let mut msg = String::with_capacity(1024);
        cand.write_notify(&mut msg, id, version, ntime, clean);
        if msg.len() > ESP_MAX_LINE - 400 && !OVERSIZED_WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!("gateway stratum: a job is {} bytes; ESP32 based miners (Bitaxe) read at most {ESP_MAX_LINE} bytes per line and will not see it. Lower max_outputs at the pool and in this gateway.", msg.len());
        }
        send(wr, msg.as_bytes()).await?;
        self.jobs.push_back(JobRef { id, cand: unit.cand, slice: unit.slice, version, epoch });
        while self.jobs.len() > MAX_JOBS {
            self.jobs.pop_front();
        }
        self.schedule_next();
        Ok(())
    }

    fn accept_target(&self) -> ([u8; 32], f64) {
        match &self.prev {
            Some((d, t, at)) if at.elapsed() < PREV_TARGET_GRACE && *t > self.target => (*t, d.min(self.difficulty)),
            _ => (self.target, self.difficulty),
        }
    }

    async fn submit(&mut self, wr: &mut OwnedWriteHalf, id: &str, params: &Value) -> Result<()> {
        if !self.authorized {
            return self.error(wr, id, 24, "Unauthorized worker").await;
        }
        let s = |i: usize| params.get(i).and_then(Value::as_str);
        let (Some(job_s), Some(ntime_s), Some(nonce_s)) = (s(1), s(3), s(4)) else {
            self.invalid();
            return self.error(wr, id, 20, "bad params").await;
        };
        let (Ok(job_id), Ok(ntime), Ok(nonce)) = (u64::from_str_radix(job_s, 16), u32::from_str_radix(ntime_s, 16), u32::from_str_radix(nonce_s, 16)) else {
            self.invalid();
            return self.error(wr, id, 20, "bad params").await;
        };
        let rolled = s(5).and_then(|b| u32::from_str_radix(b, 16).ok());
        let Some(job) = self.jobs.iter().find(|j| j.id == job_id) else {
            self.sh.stats.stale.fetch_add(1, Ordering::Relaxed);
            return self.error(wr, id, 21, "Job not found").await;
        };
        let (epoch, cand_idx, slice, job_version) = (job.epoch.clone(), job.cand, job.slice, job.version);
        if !self.epoch_rx.borrow().as_ref().map(|c| c.tip_matches(&epoch)).unwrap_or(false) {
            self.sh.stats.stale.fetch_add(1, Ordering::Relaxed);
            return self.error(wr, id, 21, "Stale share").await;
        }
        let version = match rolled {
            Some(b) if self.mask != 0 => (job_version & !self.mask) | (b & self.mask),
            _ => job_version,
        };
        let cand = &epoch.candidates[cand_idx];
        let (target, credit) = self.accept_target();
        let result = epoch.check(cand, slice, ntime, version, nonce, &target);
        match result {
            ShareResult::Valid | ShareResult::Block => {
                self.sh.stats.ok.fetch_add(1, Ordering::Relaxed);
                self.count_ok(credit);
                let hash = header_hash(&cand.header(version, ntime, nonce));
                let template_id = epoch.id as u32;
                let channel_target = self.sh.targets.read().unwrap().get(&self.channel).copied().unwrap_or([0u8; 32]);
                if result == ShareResult::Block || meets_target(&hash, &channel_target) {
                    let item = ShareItem { variant: cand_idx as u32, nonce, ntime_delta: ntime.saturating_sub(cand.curtime), version_bits: version & VERSION_MASK };
                    self.sh.stats.forwarded.fetch_add(1, Ordering::Relaxed);
                    let _ = self.sh.fwd.send(Fwd::Share { channel: self.channel, template_id, item });
                }
                if result == ShareResult::Block {
                    self.sh.stats.blocks.fetch_add(1, Ordering::Relaxed);
                    let (node, fwd, block_hex, height) = (self.sh.node.clone(), self.sh.fwd.clone(), cand.block_hex(version, ntime, nonce), epoch.height);
                    let worker = self.worker.clone();
                    tokio::spawn(async move {
                        match node.submit_block(&block_hex).await {
                            Ok(None) => {
                                tracing::info!("BLOCK FOUND at height {height} by {worker}: accepted by the node");
                                let _ = fwd.send(Fwd::Block { template_id, variant: cand_idx as u32, height, hash });
                            }
                            Ok(Some(r)) => tracing::error!("block at height {height} by {worker} rejected by the node: {r}"),
                            Err(e) => tracing::error!("submitblock failed for height {height}: {e}"),
                        }
                    });
                }
                self.reply(wr, id, "true", "null").await?;
                self.vardiff(wr).await
            }
            ShareResult::Duplicate => {
                self.sh.stats.dup.fetch_add(1, Ordering::Relaxed);
                self.error(wr, id, 22, "Duplicate share").await
            }
            ShareResult::LowDifficulty => {
                self.invalid();
                self.error(wr, id, 23, "Low difficulty share").await
            }
            ShareResult::Invalid(reason) => {
                self.invalid();
                if self.bad > 100 {
                    bail!("too many invalid shares");
                }
                self.error(wr, id, 20, reason).await
            }
        }
    }

    fn invalid(&mut self) {
        self.bad += 1;
        self.sh.stats.invalid.fetch_add(1, Ordering::Relaxed);
    }

    fn count_ok(&mut self, credit: f64) {
        self.shares_since_adjust += 1;
        self.window_diff += credit;
        let elapsed = self.window_start.elapsed().as_secs_f64();
        self.hash_est = hashes_per_share(self.window_diff) / elapsed.max(5.0);
        if elapsed > 120.0 {
            self.window_diff = self.hash_est * 10.0 / 4_294_967_296.0;
            self.window_start = Instant::now() - Duration::from_secs(10);
        }
    }

    async fn vardiff(&mut self, wr: &mut OwnedWriteHalf) -> Result<()> {
        let elapsed = self.last_adjust.elapsed().as_secs_f64();
        if !(elapsed >= 30.0 || self.shares_since_adjust >= 50) {
            return Ok(());
        }
        let ratio = self.shares_since_adjust as f64 / elapsed.max(1.0) * self.cfg.share_secs;
        self.last_adjust = Instant::now();
        self.shares_since_adjust = 0;
        if (0.6..=1.5).contains(&ratio) {
            return Ok(());
        }
        let d = self.difficulty * ratio.clamp(0.125, 8.0);
        self.set_difficulty(wr, d).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_output_limit_fits_the_line_limit_of_esp_miner() {
        // 200 outputs and a branch of 14 hashes (16,000 transactions)
        assert!(notify_size_estimate(200, 14) < ESP_MAX_LINE - 2_000, "{}", notify_size_estimate(200, 14));
        // the limit is reached at about 230 outputs
        assert!(notify_size_estimate(240, 14) > ESP_MAX_LINE - 400);
        // a plain one-output coinbase is small
        assert!(notify_size_estimate(1, 12) < 2_000);
    }
}
