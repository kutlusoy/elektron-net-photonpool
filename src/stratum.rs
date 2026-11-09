use crate::addr::script_for_address;
use crate::config::{Mode, PortCfg, StratumCfg};
use crate::diff::{hashes_per_share, target_from_difficulty};
use crate::accounting::minute_now;
use crate::pool::{get_f64, set_f64, unix_now, Epoch, FoundBlock, Pool, SessionStats, ShareResult, SoloEntry, MINER_VERSION_MASK, POOL_VERSION_SHIFT};
use crate::template::Candidate;
use crate::template::{header_hash, meets_target};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::time::{sleep_until, timeout, Instant as TokioInstant};

const MAX_LINE: usize = 8192;
/// Jobs kept per connection for late shares: at least 16, then every job up to this age, never more than the hard limit.
const MAX_JOBS: usize = 16;
const MAX_JOBS_HARD: usize = 128;
const JOB_KEEP: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const PREV_TARGET_GRACE: Duration = Duration::from_secs(60);
/// After a duplicate share the miner gets fresh work at the shortest interval for this long.
const DUP_BOOST: Duration = Duration::from_secs(30);

/// The next job interval of a connection: faster when more than a tenth of its shares are duplicates or for jobs the pool no longer knows (it works through its jobs
/// faster than they arrive or hands in late results), slowly slower again when fewer than two in a hundred are, always between `floor` and `cap`.
fn adapt_interval(cur_ms: f64, ok: u32, dup: u32, stale: u32, floor_ms: f64, cap_ms: f64) -> f64 {
    let total = ok + dup + stale;
    if total < 4 {
        return cur_ms;
    }
    let bad = (dup + stale) as f64 / total as f64;
    let next = if bad > 0.10 { cur_ms * 0.6 } else if bad < 0.02 { cur_ms * 1.15 } else { cur_ms };
    next.clamp(floor_ms, cap_ms.max(floor_ms))
}

/// Whether a miner with this user agent gets every fresh job marked clean.
fn agent_wants_clean(words: &[String], agent: &str) -> bool {
    let a = agent.to_ascii_lowercase();
    words.iter().any(|w| w == "*" || (!w.is_empty() && a.contains(&w.to_ascii_lowercase())))
}

/// Time until the next job for a miner: the time its hardware needs to sweep the space of one job (a bit less, to stay ahead), within the configured limits.
/// Without a hash rate estimate the pool sends a job per second; after a duplicate share it sends them as fast as allowed.
fn job_delay_ms(cfg: &crate::config::StratumCfg, rolling_bits: u32, hash_est: f64, boost: bool, floor_ms: f64) -> u64 {
    let bits = rolling_bits.min(cfg.version_space_bits);
    let unit_hashes = 2f64.powi(32 + bits as i32);
    let secs = if boost { 0.0 } else if hash_est > 0.0 { unit_hashes / hash_est * 0.8 } else { 1.0 };
    let max = cfg.max_job_interval_ms as f64;
    (secs * 1000.0).clamp(floor_ms.min(max), max) as u64
}

/// The hash rate a new connection starts with. A measured rate can be too low because the miner was starved or lost shares, and starting from it again would keep it
/// low, so where the device type has a typical rate the higher of the two is used; without a typical rate the learned one is all there is.
fn start_rate(learned: Option<f64>, typical: Option<f64>) -> Option<f64> {
    match (learned, typical) {
        (Some(l), Some(t)) => Some(l.max(t)),
        (l, t) => l.or(t),
    }
}

/// Shortest job interval of one connection: the configured minimum, and never so short that all connections together exceed `max_jobs_per_sec`.
fn floor_ms(cfg: &crate::config::StratumCfg, conns: u64) -> f64 {
    (cfg.min_job_interval_ms as f64).max(1000.0 * conns as f64 / cfg.max_jobs_per_sec.max(1) as f64)
}

pub async fn serve(pool: Arc<Pool>) -> Result<()> {
    let cfg = Arc::new(pool.cfg.stratum.clone());
    let permits = Arc::new(Semaphore::new(cfg.max_connections));
    tokio::spawn(stats_task(pool.clone()));
    let mut listeners = Vec::new();
    for port in pool.cfg.stratum_ports() {
        let listener = TcpListener::bind(&port.listen).await?;
        pool.event("info", format!("stratum v1 listening on {} (default mode {}{})", port.listen, port.mode.as_str(), if port.allow_override { ", override allowed" } else { "" }));
        listeners.push(tokio::spawn(accept_loop(pool.clone(), cfg.clone(), permits.clone(), listener, port)));
    }
    for l in listeners {
        let _ = l.await;
    }
    Ok(())
}

async fn accept_loop(pool: Arc<Pool>, cfg: Arc<StratumCfg>, permits: Arc<Semaphore>, listener: TcpListener, port: PortCfg) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            NET.refused.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let _ = stream.set_nodelay(true);
        let (pool, cfg, port) = (pool.clone(), cfg.clone(), port.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let started = Instant::now();
            let id = pool.session_seq.fetch_add(1, Ordering::Relaxed) + 1;
            NET.accepted.fetch_add(1, Ordering::Relaxed);
            pool.conns.fetch_add(1, Ordering::Relaxed);
            let stats = Arc::new(SessionStats::new());
            pool.sessions.lock().unwrap().insert(id, stats.clone());
            let stats_end = stats.clone();
            let res = Session::new(pool.clone(), cfg, port, stats, id).run(stream).await;
            pool.sessions.lock().unwrap().remove(&id);
            NET.closed.fetch_add(1, Ordering::Relaxed);
            pool.conns.fetch_sub(1, Ordering::Relaxed);
            NET.bytes_in.fetch_add(stats_end.net_in.load(Ordering::Relaxed), Ordering::Relaxed);
            NET.bytes_out.fetch_add(stats_end.net_out.load(Ordering::Relaxed), Ordering::Relaxed);
            NET.jobs.fetch_add(stats_end.net_jobs.load(Ordering::Relaxed), Ordering::Relaxed);
            let shares = stats_end.ok.load(Ordering::Relaxed) + stats_end.stale.load(Ordering::Relaxed) + stats_end.inv.load(Ordering::Relaxed) + stats_end.dup.load(Ordering::Relaxed);
            let (secs, agent) = (started.elapsed().as_secs(), stats_end.agent.lock().unwrap().clone());
            let why = match &res { Ok(()) => "closed by the miner".to_string(), Err(e) => e.to_string() };
            if shares == 0 && secs >= 1 {
                // a miner that connected and never sent a share: the usual sign of a handshake that the firmware did not accept. Logged for the first few
                // sessions of every minute, then counted (a miner that is turned away reconnects every second).
                let trace = stats_end.trace.lock().unwrap().clone();
                let minute = unix_now() as u64 / 60;
                let (show, skipped) = {
                    let mut g = ENDED.lock().unwrap();
                    let skipped = if g.0 != minute { let s = g.2; *g = (minute, 0, 0); s } else { 0 };
                    if g.1 < 3 { g.1 += 1; (true, skipped) } else { g.2 += 1; (false, skipped) }
                };
                if skipped > 0 {
                    tracing::info!("{skipped} more sessions ended without a share in the minute before");
                }
                if show {
                    tracing::info!("session {id} {peer} [{}] ended without a share after {secs}s: {why}; it sent: {}", if agent.is_empty() { "no user agent" } else { agent.as_str() }, if trace.is_empty() { "nothing" } else { trace.as_str() });
                }
            } else if let Err(e) = res {
                tracing::debug!("session {id} {peer} closed: {e}");
            }
        });
    }
}

enum Src {
    Pool,
    Solo(Arc<SoloEntry>),
}

struct JobRef {
    id: u64,
    epoch: Arc<Epoch>,
    src: Src,
    n: u32,
    version: u32,
    sent: Instant,
}

struct Session {
    pool: Arc<Pool>,
    cfg: Arc<StratumCfg>,
    stats: Arc<SessionStats>,
    id: u64,
    port: PortCfg,
    mode: Mode,
    epoch_rx: watch::Receiver<Option<Arc<Epoch>>>,
    subscribed: bool,
    authorized: bool,
    started: bool,
    account: String,
    worker: String,
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
    /// Typical hash rate of this kind of device (from the user agent), 0 when unknown: a lower bound for the job rate while the measured rate is still too low.
    prior_hs: f64,
    last_job: Instant,
    dup_until: Instant,
    agent_clean: bool,
    /// extranonce1 of this connection when it gets extranonce space (see [stratum] extranonce_agents).
    en1: Option<[u8; crate::template::EN1_BYTES]>,
    adapt_ms: Option<f64>,
    adapt_ok: u32,
    adapt_dup: u32,
    adapt_stale: u32,
    adapt_t: Instant,
    last_adjust: Instant,
    shares_since_adjust: u32,
    bad: u32,
    acc_minute: i64,
    acc_diff: f64,
    acc_cnt: u64,
    acc_last: Instant,
}

#[derive(Deserialize)]
struct Req<'a> {
    id: Option<Value>,
    method: &'a str,
    #[serde(default)]
    params: Value,
}

/// Network counters of the miner-facing listeners, sampled once a second into rates for the engine view. The hot path (every read and every job) counts in the
/// session's own atomics (no shared cache line between connections); `bytes_in`, `bytes_out` and `jobs` here hold only what closed sessions have counted.
use std::sync::atomic::AtomicU64;
pub struct Net {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub accepted: AtomicU64,
    pub closed: AtomicU64,
    pub refused: AtomicU64,
    pub jobs: AtomicU64,
}
/// (minute, sessions logged in it, sessions not logged in it) for the sessions that ended without a share.
static ENDED: std::sync::Mutex<(u64, u32, u32)> = std::sync::Mutex::new((0, 0, 0));

pub static NET: Net = Net { bytes_in: AtomicU64::new(0), bytes_out: AtomicU64::new(0), accepted: AtomicU64::new(0), closed: AtomicU64::new(0), refused: AtomicU64::new(0), jobs: AtomicU64::new(0) };

async fn send(wr: &mut OwnedWriteHalf, data: &[u8]) -> Result<()> {
    timeout(WRITE_TIMEOUT, wr.write_all(data)).await.map_err(|_| anyhow!("write timeout"))??;
    Ok(())
}

async fn read_chunk(rd: &mut BufReader<OwnedReadHalf>, buf: &mut Vec<u8>, counted: &AtomicU64) -> Result<Option<bool>> {
    if buf.len() > MAX_LINE {
        bail!("line too long");
    }
    let limit = (MAX_LINE + 1 - buf.len()) as u64;
    let n = (&mut *rd).take(limit).read_until(b'\n', buf).await?;
    counted.fetch_add(n as u64, Ordering::Relaxed);
    if n == 0 {
        return Ok(None);
    }
    Ok(Some(buf.last() == Some(&b'\n')))
}

impl Session {
    fn new(pool: Arc<Pool>, cfg: Arc<StratumCfg>, port: PortCfg, stats: Arc<SessionStats>, id: u64) -> Self {
        let now = Instant::now();
        let difficulty = cfg.initial_difficulty.clamp(cfg.min_difficulty, cfg.max_difficulty);
        let epoch_rx = pool.epoch.subscribe();
        Self {
            pool, cfg, stats, id, mode: port.mode, port, epoch_rx, subscribed: false, authorized: false, started: false, account: String::new(), worker: String::new(),
            difficulty, target: target_from_difficulty(difficulty), prev: None, mask: 0, rolling_bits: 0, jobs: VecDeque::new(), last_tip: String::new(),
            next_job: now + Duration::from_secs(3600), window_start: now, window_diff: 0.0, hash_est: 0.0, prior_hs: 0.0, last_job: now, dup_until: now, agent_clean: false, en1: None, adapt_ms: None, adapt_ok: 0, adapt_dup: 0, adapt_stale: 0, adapt_t: now, last_adjust: now, shares_since_adjust: 0, bad: 0,
            acc_minute: 0, acc_diff: 0.0, acc_cnt: 0, acc_last: now,
        }
    }

    async fn run(mut self, stream: TcpStream) -> Result<()> {
        let res = self.run_inner(stream).await;
        self.flush_acc();
        res
    }

    async fn run_inner(&mut self, stream: TcpStream) -> Result<()> {
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::with_capacity(4096, rd);
        let mut buf: Vec<u8> = Vec::with_capacity(512);
        let idle = Duration::from_secs(self.cfg.idle_secs);
        let mut deadline = Instant::now() + idle;
        loop {
            let next_job = TokioInstant::from_std(self.next_job);
            let started = self.started;
            tokio::select! {
                r = read_chunk(&mut rd, &mut buf, &self.stats.net_in) => match r? {
                    None => return Ok(()),
                    Some(false) => {}
                    Some(true) => {
                        deadline = Instant::now() + idle;
                        self.stats.last_seen.store(unix_now().to_bits(), Ordering::Relaxed);
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
                _ = sleep_until(next_job), if started => {
                    self.send_job(&mut wr, false).await?;
                    // a slow miner (CPU, tiny device) finds no share at all at a high difficulty, so vardiff must also run without shares;
                    // only for a connection that has never had a valid share, so the difficulty of an established miner is never touched here
                    if self.stats.ok.load(Ordering::Relaxed) == 0 {
                        self.vardiff(&mut wr).await?;
                    }
                }
                _ = sleep_until(TokioInstant::from_std(deadline)) => bail!("idle timeout"),
            }
        }
    }

    async fn tx(&self, wr: &mut OwnedWriteHalf, data: &[u8]) -> Result<()> {
        self.stats.net_out.fetch_add(data.len() as u64, Ordering::Relaxed);
        send(wr, data).await
    }

    async fn reply(&self, wr: &mut OwnedWriteHalf, id: &str, result: &str, error: &str) -> Result<()> {
        self.tx(wr, format!("{{\"id\":{id},\"result\":{result},\"error\":{error}}}\n").as_bytes()).await
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
        self.stats.note(&match req.method {
            "mining.configure" => {
                let o = req.params.get(1);
                format!("configure(min-bits {}, mask {})", o.and_then(|o| o.get("version-rolling.min-bit-count")).map(|v| v.to_string()).unwrap_or("-".into()), o.and_then(|o| o.get("version-rolling.mask")).and_then(Value::as_str).unwrap_or("-"))
            }
            m => m.strip_prefix("mining.").unwrap_or(m).chars().take(24).collect(),
        });
        match req.method {
            "mining.subscribe" => {
                self.subscribed = true;
                let agent = crate::devices::clean_agent(req.params.get(0).and_then(Value::as_str).unwrap_or(""));
                self.agent_clean = agent_wants_clean(&self.cfg.clean_work_agents, &agent);
                let en_mode = agent_wants_clean(&self.cfg.extranonce_agents, &agent);
                if en_mode {
                    self.en1 = Some((self.id as u32).to_be_bytes());
                }
                let label = if en_mode || agent_wants_clean(&self.cfg.label_extranonce_agents, &agent) { format!("{:08x}", self.id) } else { String::new() };
                let n2 = if en_mode { crate::template::EN2_BYTES } else { 0 };
                let dev = crate::devices::classify(&agent);
                // What the pool knows about this kind of device: what it learned from earlier connections (after a few observations), else a typical value
                // of the chip. It sets the first share difficulty (about one share per share_secs), the job pace and whether jobs are clean; everything is
                // corrected by the measurement of this connection from the first shares on.
                let learned = self.pool.learned.lock().unwrap().get(&crate::devices::profile_key(&agent)).copied().filter(|l| l.samples >= 3);
                let prior = start_rate(learned.map(|l| l.hashrate), crate::devices::expected_hashrate(&agent));
                if let Some(h) = prior.filter(|h| *h > 0.0) {
                    self.prior_hs = h;
                    let d = (h * self.cfg.share_secs / 4_294_967_296.0).clamp(self.cfg.min_difficulty, self.cfg.max_difficulty);
                    if !self.started {
                        self.difficulty = d;
                        self.target = target_from_difficulty(d);
                    }
                }
                if let Some(l) = learned {
                    if l.job_ms > 0.0 {
                        self.adapt_ms = Some(l.job_ms);
                    }
                    self.agent_clean |= l.clean;
                }
                *self.stats.device.lock().unwrap() = (dev.key, dev.name);
                *self.stats.agent.lock().unwrap() = agent;
                self.reply(wr, &id, &format!("[[[\"mining.notify\",\"{:x}\"]],\"{label}\",{n2}]", self.id), "null").await?;
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

    async fn authorize(&mut self, wr: &mut OwnedWriteHalf, id: &str, params: &Value) -> Result<()> {
        let user = params.get(0).and_then(Value::as_str).unwrap_or("");
        let (account, worker) = user.split_once('.').unwrap_or((user, "default"));
        let hrp = self.pool.cfg.network()?.hrp;
        let valid = !account.is_empty() && (!self.cfg.require_address || script_for_address(account, hrp).is_ok());
        if !valid || worker.len() > 64 {
            self.bad += 1;
            tracing::info!("session {}: authorization refused, the user name must be <address>.<worker> with a valid address (got {:?})", self.id, user.chars().take(24).collect::<String>());
            if self.bad > 10 {
                bail!("too many failed authorizations");
            }
            return self.reply(wr, id, "false", "[24,\"Unauthorized worker\",null]").await;
        }
        let mut mode = self.port.mode;
        if self.port.allow_override {
            let pw = params.get(1).and_then(Value::as_str).unwrap_or("");
            if let Some(m) = pw.split(|c| c == ',' || c == ';' || c == ' ').find_map(|kv| kv.strip_prefix("m=")).and_then(Mode::parse) {
                mode = m;
            }
        }
        if mode == Mode::Solo && script_for_address(account, hrp).is_err() {
            return self.reply(wr, id, "false", "[24,\"Solo mode needs a payout address as account\",null]").await;
        }
        if let Err(why) = self.pool.mode_available(mode) {
            return self.reply(wr, id, "false", &format!("[20,\"{why}\",null]")).await;
        }
        self.mode = mode;
        self.stats.mode.store(mode as u64, Ordering::Relaxed);
        self.account = account.to_string();
        self.worker = worker.to_string();
        *self.stats.name.lock().unwrap() = format!("{account}.{worker}");
        if mode == Mode::Solo {
            self.pool.touch_solo(account);
        }
        self.authorized = true;
        self.reply(wr, id, "true", "null").await?;
        self.maybe_start(wr).await
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
        set_f64(&self.stats.difficulty, self.difficulty);
        self.tx(wr, format!("{{\"id\":null,\"method\":\"mining.set_difficulty\",\"params\":[{}]}}\n", self.difficulty).as_bytes()).await
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

    /// The hash rate the job interval is based on: the measured one, but at least 60 % of the typical rate of the device type. A miner that is starved of work reports
    /// less than it can do, so the measurement alone would keep it starved; with the prior it gets work at the right pace at once and the interval follows the
    /// measurement up from there.
    fn rate_for_jobs(&self) -> f64 {
        self.hash_est.max(0.6 * self.prior_hs)
    }

    fn schedule_next(&mut self) {
        let boost = Instant::now() < self.dup_until;
        let floor = floor_ms(&self.cfg, self.pool.conns.load(Ordering::Relaxed));
        let model = job_delay_ms(&self.cfg, self.rolling_bits, self.rate_for_jobs(), boost, floor);
        let ms = match self.adapt_ms {
            Some(a) if !boost => (a.clamp((self.cfg.adaptive_min_interval_ms as f64).max(floor).min(self.cfg.max_job_interval_ms as f64), self.cfg.max_job_interval_ms as f64)) as u64,
            _ => model,
        };
        set_f64(&self.stats.job_ms, ms as f64);
        self.next_job = Instant::now() + Duration::from_millis(ms);
    }

    /// Learns the job interval of this connection from its duplicate and stale shares (see `adapt_interval`).
    fn adapt(&mut self) {
        if !self.cfg.adaptive_jobs {
            return;
        }
        let total = self.adapt_ok + self.adapt_dup + self.adapt_stale;
        if total < 12 && !(total >= 4 && self.adapt_t.elapsed() > Duration::from_secs(60)) {
            return;
        }
        let global = floor_ms(&self.cfg, self.pool.conns.load(Ordering::Relaxed));
        let cap = job_delay_ms(&self.cfg, self.rolling_bits, self.rate_for_jobs(), false, global) as f64;
        let floor = (self.cfg.adaptive_min_interval_ms as f64).max(global).min(cap);
        let cur = self.adapt_ms.unwrap_or(cap);
        let next = adapt_interval(cur, self.adapt_ok, self.adapt_dup, self.adapt_stale, floor, cap);
        let bad = (self.adapt_dup + self.adapt_stale) as f64 / total as f64;
        // fastest interval and still repeating work: the miner keeps old jobs queued, so give it clean jobs
        if bad > 0.10 && next <= floor * 1.01 && !self.agent_clean {
            self.agent_clean = true;
            self.stats.clean_learned.store(1, Ordering::Relaxed);
        }
        self.adapt_ms = Some(next);
        set_f64(&self.stats.adapt_ms, next);
        (self.adapt_ok, self.adapt_dup, self.adapt_stale, self.adapt_t) = (0, 0, 0, Instant::now());
    }

    async fn send_job(&mut self, wr: &mut OwnedWriteHalf, force_clean: bool) -> Result<()> {
        if self.acc_cnt > 0 && self.acc_last.elapsed() > Duration::from_secs(2) {
            self.flush_acc();
        }
        let Some(epoch) = self.epoch_rx.borrow().clone() else {
            self.schedule_next();
            return Ok(());
        };
        let clean = force_clean || epoch.tip_hex != self.last_tip || self.agent_clean;
        let (n, src, unit): (u32, Src, (usize, u8, u32)) = if self.mode == Mode::Solo {
            self.pool.touch_solo(&self.account);
            match self.pool.ensure_solo(&epoch, &self.account).await {
                Ok(entry) => {
                    let (n, slice, off) = entry.next_unit();
                    (n, Src::Solo(entry), (0, slice, off))
                }
                Err(e) => {
                    tracing::debug!("solo candidate for {} not ready: {e}", self.account);
                    self.next_job = Instant::now() + Duration::from_millis(500);
                    return Ok(());
                }
            }
        } else {
            let (n, u) = epoch.next_unit();
            (n, Src::Pool, (u.cand, u.slice, u.ntime_off))
        };
        if clean && epoch.tip_hex != self.last_tip {
            self.last_tip = epoch.tip_hex.clone();
        }
        let cand: &Candidate = match &src {
            Src::Pool => &epoch.candidates[unit.0],
            Src::Solo(e) => &e.cand,
        };
        let version = cand.version | ((unit.1 as u32) << POOL_VERSION_SHIFT);
        let ntime = cand.curtime.max(cand.mintime) + unit.2;
        let id = (epoch.id << 32) | n as u64;
        let mut msg = String::with_capacity(1024);
        if self.en1.is_some() && cand.has_en() {
            cand.write_notify_en(&mut msg, id, version, ntime, clean);
        } else {
            cand.write_notify(&mut msg, id, version, ntime, clean);
        }
        self.tx(wr, msg.as_bytes()).await?;
        self.stats.net_jobs.fetch_add(1, Ordering::Relaxed);
        self.last_job = Instant::now();
        self.stats.unit.store(((unit.0 as u64) << 16) | ((unit.2 as u64) << 8) | unit.1 as u64, Ordering::Relaxed);
        self.jobs.push_back(JobRef { id, epoch, src, n, version, sent: Instant::now() });
        while self.jobs.len() > MAX_JOBS_HARD || (self.jobs.len() > MAX_JOBS && self.jobs.front().map_or(false, |j| j.sent.elapsed() > JOB_KEEP)) {
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
            self.count_invalid();
            return self.error(wr, id, 20, "bad params").await;
        };
        let (Ok(job_id), Ok(ntime), Ok(nonce)) = (u64::from_str_radix(job_s, 16), u32::from_str_radix(ntime_s, 16), u32::from_str_radix(nonce_s, 16)) else {
            self.count_invalid();
            return self.error(wr, id, 20, "bad params").await;
        };
        let rolled = s(5).and_then(|b| u32::from_str_radix(b, 16).ok());
        let found = self.jobs.iter().find(|j| j.id == job_id).map(|j| {
            let solo = match &j.src {
                Src::Solo(e) => Some(e.clone()),
                Src::Pool => None,
            };
            (j.epoch.clone(), solo, j.n, j.version)
        });
        let Some((epoch, solo, job_n, job_version)) = found else {
            self.adapt_stale += 1;
            self.adapt();
            self.count_stale();
            return self.error(wr, id, 21, "Job not found").await;
        };
        let current = self.epoch_rx.borrow().clone();
        if !current.map(|c| c.tip_matches(&epoch)).unwrap_or(false) {
            self.count_stale();
            return self.error(wr, id, 21, "Stale share").await;
        }
        let version = match rolled {
            Some(b) if self.mask != 0 => (job_version & !self.mask) | (b & self.mask),
            _ => job_version,
        };
        let (cand_ref, slice): (&Candidate, u8) = match &solo {
            None => {
                let u = epoch.unit_from_n(job_n);
                (&epoch.candidates[u.cand], u.slice)
            }
            Some(e) => (&e.cand, SoloEntry::unit_from_n(job_n).0),
        };
        let (target, credit) = self.accept_target();
        let en: Option<[u8; crate::template::EN_BYTES]> = match self.en1 {
            None => None,
            Some(en1) => {
                let en2 = s(2).filter(|x| x.len() == crate::template::EN2_BYTES * 2).and_then(|x| hex::decode(x).ok());
                let Some(en2) = en2 else {
                    self.count_invalid();
                    return self.error(wr, id, 20, "bad extranonce2").await;
                };
                let mut b = [0u8; crate::template::EN_BYTES];
                b[..crate::template::EN1_BYTES].copy_from_slice(&en1);
                b[crate::template::EN1_BYTES..].copy_from_slice(&en2);
                Some(b)
            }
        };
        let result = match &en {
            Some(en) => epoch.check_en(cand_ref, slice, ntime, version, nonce, &target, MINER_VERSION_MASK, en),
            None => epoch.check(cand_ref, slice, ntime, version, nonce, &target),
        };
        match result {
            ShareResult::Valid | ShareResult::Block => {
                self.adapt_ok += 1;
                self.adapt();
                let d = match &en {
                    Some(en) => epoch.share_difficulty_en(cand_ref, ntime, version, nonce, en),
                    None => epoch.share_difficulty(cand_ref, ntime, version, nonce),
                };
                if d > get_f64(&self.stats.best) {
                    set_f64(&self.stats.best, d);
                }
                let agent = self.stats.agent.lock().unwrap().clone();
                self.pool.note_best(&agent, d);
                self.count_ok(credit);
                if result == ShareResult::Block {
                    self.block_found(&epoch, cand_ref, version, ntime, nonce, en.as_ref());
                }
                self.reply(wr, id, "true", "null").await?;
                self.vardiff(wr).await
            }
            ShareResult::Duplicate => {
                self.stats.dup.fetch_add(1, Ordering::Relaxed);
                self.pool.stats.dup.fetch_add(1, Ordering::Relaxed);
                self.adapt_dup += 1;
                self.adapt();
                // The same share again means the miner has swept the space of its job and starts over: give it fresh work, but not faster than min_job_interval_ms.
                self.dup_until = Instant::now() + DUP_BOOST;
                self.next_job = self.next_job.min((self.last_job + Duration::from_millis(floor_ms(&self.cfg, self.pool.conns.load(Ordering::Relaxed)) as u64)).max(Instant::now()));
                self.error(wr, id, 22, "Duplicate share").await
            }
            ShareResult::LowDifficulty => {
                self.count_invalid();
                self.error(wr, id, 23, "Low difficulty share").await
            }
            ShareResult::Invalid(reason) => {
                self.count_invalid();
                if self.bad > 100 {
                    bail!("too many invalid shares");
                }
                self.error(wr, id, 20, reason).await
            }
        }
    }

    fn count_stale(&self) {
        self.stats.stale.fetch_add(1, Ordering::Relaxed);
        self.pool.stats.stale.fetch_add(1, Ordering::Relaxed);
    }

    fn count_invalid(&mut self) {
        self.bad += 1;
        self.stats.inv.fetch_add(1, Ordering::Relaxed);
        self.pool.stats.inv.fetch_add(1, Ordering::Relaxed);
    }

    fn flush_acc(&mut self) {
        if self.acc_cnt > 0 {
            self.pool.agg_add(self.acc_minute, &self.account, self.mode, self.acc_diff, self.acc_cnt);
            self.acc_diff = 0.0;
            self.acc_cnt = 0;
        }
    }

    fn acc_share(&mut self, diff: f64) {
        let minute = minute_now();
        if self.acc_cnt > 0 && minute != self.acc_minute {
            self.flush_acc();
        }
        if self.acc_cnt == 0 {
            self.acc_minute = minute;
            self.acc_last = Instant::now();
        }
        self.acc_diff += diff;
        self.acc_cnt += 1;
    }

    fn count_ok(&mut self, credit: f64) {
        self.acc_share(credit);
        self.stats.ok.fetch_add(1, Ordering::Relaxed);
        self.pool.stats.ok.fetch_add(1, Ordering::Relaxed);
        self.shares_since_adjust += 1;
        self.window_diff += credit;
        let elapsed = self.window_start.elapsed().as_secs_f64();
        self.hash_est = hashes_per_share(self.window_diff) / elapsed.max(5.0);
        set_f64(&self.stats.hashrate, self.hash_est);
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
        let none = self.shares_since_adjust == 0;
        let rate = self.shares_since_adjust as f64 / elapsed.max(1.0);
        let ratio = rate * self.cfg.share_secs;
        self.last_adjust = Instant::now();
        self.shares_since_adjust = 0;
        if (0.6..=1.5).contains(&ratio) {
            return Ok(());
        }
        // no share at all in the window: the difficulty is far too high for this miner, go down fast
        let d = self.difficulty * if none { 1.0 / 32.0 } else { ratio.clamp(0.125, 8.0) };
        self.set_difficulty(wr, d).await
    }

    fn block_found(&mut self, epoch: &Arc<Epoch>, c: &Candidate, version: u32, ntime: u32, nonce: u32, en: Option<&[u8; crate::template::EN_BYTES]>) {
        self.flush_acc();
        let (hex, header) = match en {
            Some(en) => (c.block_hex_en(en, version, ntime, nonce), c.header_root(&c.en_root(en), version, ntime, nonce)),
            None => (c.block_hex(version, ntime, nonce), c.header(version, ntime, nonce)),
        };
        let mut h = header_hash(&header);
        debug_assert!(meets_target(&h, &c.target));
        h.reverse();
        let (pool, height, hash, account, worker, mode) = (self.pool.clone(), epoch.height, hex::encode(h), self.account.clone(), format!("{}.{}", self.account, self.worker), self.mode);
        let (reward, subsidy, net_diff) = (c.coinbase_value, c.subsidy(), crate::diff::difficulty_from_target(&c.target));
        tokio::spawn(async move {
            match pool.node.submit_block(&hex).await {
                Ok(None) => {
                    pool.stats.found.lock().unwrap().push(FoundBlock { height, hash: hash.clone(), time: unix_now(), worker: worker.clone(), mode: mode.as_str(), status: "found" });
                    if let Err(e) = pool.record_block(height, reward, subsidy, net_diff, hash.clone(), account, worker.clone(), mode).await {
                        pool.event("err", format!("ledger could not record block {height}: {e}"));
                    }
                    pool.event("blk", format!("BLOCK FOUND {height} {hash} by {worker} [{}] -> submitblock: accepted", mode.as_str()));
                    pool.registry.block_found(&hash, pool.clone());
                }
                Ok(Some(r)) => pool.event("err", format!("block {height} by {worker} rejected by node: {r}")),
                Err(e) => pool.event("err", format!("submitblock failed for {height}: {e}")),
            }
        });
    }
}

/// One-second sampling of session counters for the admin API.
async fn stats_task(pool: Arc<Pool>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let (mut prev_shares, mut prev_units, mut prev_epoch) = (0u64, 0u64, 0u64);
    let mut prev_net = (0u64, 0u64, 0u64, 0u64);
    let (mut told_online, mut told_at) = (0u64, Instant::now());
    // one history sample per minute for the public statistics
    let (mut hist_minute, mut hist_sum, mut hist_n, mut hist_base) = (unix_now() as i64 / 60, 0.0f64, 0u32, (0u64, 0u64, 0u64));
    // per device type: (name, summed hash rate of the seconds, seconds, peak working connections), written once per device_history_minutes
    let slot_len = pool.cfg.ledger.device_history_minutes.max(1);
    let mut dev_slot = unix_now() as i64 / 60 / slot_len;
    let mut dev_acc: std::collections::HashMap<String, (String, f64, u32, u64)> = std::collections::HashMap::new();
    loop {
        tick.tick().await;
        let sessions: Vec<Arc<SessionStats>> = pool.sessions.lock().unwrap().values().cloned().collect();
        let now = unix_now();
        let (mut hr, mut online, mut shares, mut pps_hr) = (0.0, 0u64, 0u64, 0.0);
        let (mut live_in, mut live_out, mut live_jobs) = (0u64, 0u64, 0u64);
        let mut by_mode = [(0u64, 0.0f64); 3];
        for s in &sessions {
            hr += get_f64(&s.hashrate);
            live_in += s.net_in.load(Ordering::Relaxed);
            live_out += s.net_out.load(Ordering::Relaxed);
            live_jobs += s.net_jobs.load(Ordering::Relaxed);
            if s.mode.load(Ordering::Relaxed) == Mode::Pps as u64 {
                pps_hr += get_f64(&s.hashrate);
            }
            if get_f64(&s.difficulty) > 0.0 {
                let m = (s.mode.load(Ordering::Relaxed) as usize).min(2);
                by_mode[m].0 += 1;
                by_mode[m].1 += get_f64(&s.hashrate);
            }
            // a connected miner is online even while it hashes without finding a share (CPU miners can take minutes per share)
            if get_f64(&s.difficulty) > 0.0 {
                online += 1;
            }
            shares += s.ok.load(Ordering::Relaxed) + s.stale.load(Ordering::Relaxed) + s.dup.load(Ordering::Relaxed) + s.inv.load(Ordering::Relaxed);
        }
        let epoch = pool.epoch.borrow().clone();
        let (eid, used) = epoch.map(|e| (e.id, e.units_used())).unwrap_or((0, 0));
        let mut live = pool.live.lock().unwrap();
        live.units_per_sec = if eid == prev_epoch && used >= prev_units { (used - prev_units) as f64 } else { 0.0 };
        live.shares_per_sec = shares.saturating_sub(prev_shares) as f64;
        live.hashrate = hr;
        live.pps_hashrate = pps_hr;
        live.m1 = if live.m1 == 0.0 { hr } else { live.m1 + (hr - live.m1) * (1.0 - (-1.0f64 / 60.0).exp()) };
        live.h1 = if live.h1 == 0.0 { hr } else { live.h1 + (hr - live.h1) * (1.0 - (-1.0f64 / 3600.0).exp()) };
        live.hist.push_back(hr);
        if live.hist.len() > 60 {
            live.hist.pop_front();
        }
        let net = (NET.bytes_in.load(Ordering::Relaxed) + live_in, NET.bytes_out.load(Ordering::Relaxed) + live_out, NET.jobs.load(Ordering::Relaxed) + live_jobs, NET.accepted.load(Ordering::Relaxed));
        live.tot_in = net.0;
        live.tot_out = net.1;
        live.in_bps = net.0.saturating_sub(prev_net.0) as f64;
        live.out_bps = net.1.saturating_sub(prev_net.1) as f64;
        live.jobs_per_sec = net.2.saturating_sub(prev_net.2) as f64;
        live.connects_per_sec = net.3.saturating_sub(prev_net.3) as f64;
        prev_net = net;
        live.by_mode = by_mode;
        // a changed number of connected miners is worth a line in the feed, at most one every 30 seconds
        if online != told_online && told_at.elapsed() >= Duration::from_secs(30) {
            let d = online as i64 - told_online as i64;
            told_online = online;
            told_at = Instant::now();
            let p = pool.clone();
            p.event("info", format!("miners online: {online} ({}{d})", if d >= 0 { "+" } else { "" }));
        }
        live.online = online;
        live.total = sessions.len() as u64;
        drop(live);
        (prev_shares, prev_units, prev_epoch) = (shares, used, eid);
        hist_sum += hr;
        hist_n += 1;
        {
            let mut now_dev: std::collections::HashMap<String, (String, f64, u64)> = std::collections::HashMap::new();
            for s in &sessions {
                if get_f64(&s.difficulty) <= 0.0 {
                    continue;
                }
                let d = s.device.lock().unwrap();
                let e = now_dev.entry(d.0.clone()).or_insert_with(|| (d.1.clone(), 0.0, 0));
                e.1 += get_f64(&s.hashrate);
                e.2 += 1;
            }
            for (key, (name, rate, workers)) in now_dev {
                let a = dev_acc.entry(key).or_insert((name, 0.0, 0, 0));
                a.1 += rate;
                a.2 += 1;
                a.3 = a.3.max(workers);
            }
        }
        let slot = now as i64 / 60 / slot_len;
        if slot != dev_slot {
            let rows: Vec<crate::ledger::DeviceRow> = dev_acc.drain().map(|(key, (name, sum, n, workers))| (dev_slot * slot_len, key, name, sum / n.max(1) as f64, workers as i64)).collect();
            dev_slot = slot;
            if !rows.is_empty() {
                let p = pool.clone();
                tokio::spawn(async move {
                    let _ = p.ledger.call(move |c| crate::ledger::add_device_history(c, &rows)).await;
                });
            }
        }
        let minute = now as i64 / 60;
        if minute != hist_minute {
            let (ok, stale, dup) = (pool.stats.ok.load(Ordering::Relaxed), pool.stats.stale.load(Ordering::Relaxed), pool.stats.dup.load(Ordering::Relaxed));
            let net_diff = pool.epoch.borrow().as_ref().and_then(|e| e.candidates.first().map(|c| crate::diff::difficulty_from_target(&c.target))).unwrap_or(0.0);
            let row = crate::ledger::HistoryRow {
                minute: hist_minute, hashrate: hist_sum / hist_n.max(1) as f64, miners: online as i64,
                ok: ok.saturating_sub(hist_base.0) as i64, stale: stale.saturating_sub(hist_base.1) as i64, dup: dup.saturating_sub(hist_base.2) as i64, net_diff,
            };
            let p = pool.clone();
            tokio::spawn(async move {
                let _ = p.ledger.call(move |c| crate::ledger::add_history(c, &row)).await;
            });
            (hist_minute, hist_sum, hist_n, hist_base) = (minute, 0.0, 0, (ok, stale, dup));
            learn_devices(&pool, &sessions, now);
        }
    }
}

/// One connection as seen by the learning: (device key, hash rate, learned job interval or 0, learned clean jobs).
type Observation = (String, f64, f64, bool);

/// Per device type the median hash rate, the median learned job interval (0 if none) and whether most connections that learned an interval needed clean jobs.
fn aggregate_types(obs: &[Observation]) -> HashMap<String, (f64, f64, bool)> {
    fn median(v: &mut Vec<f64>) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if v.is_empty() { 0.0 } else { v[v.len() / 2] }
    }
    let mut by_type: HashMap<&str, (Vec<f64>, Vec<f64>, usize)> = HashMap::new();
    for (key, hr, adapt, clean) in obs {
        let e = by_type.entry(key.as_str()).or_default();
        e.0.push(*hr);
        if *adapt > 0.0 {
            e.1.push(*adapt);
            e.2 += *clean as usize;
        }
    }
    by_type.into_iter().map(|(k, (mut hr, mut ad, votes))| {
        let clean = !ad.is_empty() && votes * 2 > ad.len();
        (k.to_string(), (median(&mut hr), median(&mut ad), clean))
    }).collect()
}

/// Once a minute: what the connections of each device type show (typical hash rate, learned job interval, need for clean jobs) is folded into the knowledge of
/// that type and saved. Only connections with enough shares count.
fn learn_devices(pool: &Arc<Pool>, sessions: &[Arc<SessionStats>], now: f64) {
    let mut obs: Vec<Observation> = Vec::new();
    for s in sessions {
        let (hr, ok) = (get_f64(&s.hashrate), s.ok.load(Ordering::Relaxed));
        if hr <= 0.0 || ok < 20 || now - get_f64(&s.last_seen) > 120.0 {
            continue;
        }
        let key = crate::devices::profile_key(&s.agent.lock().unwrap());
        obs.push((key, hr, get_f64(&s.adapt_ms), s.clean_learned.load(Ordering::Relaxed) != 0));
    }
    for (key, (h, j, clean)) in aggregate_types(&obs) {
        let merged = {
            let mut m = pool.learned.lock().unwrap();
            let l = crate::pool::merge_learned(m.get(&key).copied(), h, j, clean);
            m.insert(key.clone(), l);
            l
        };
        let p = pool.clone();
        tokio::spawn(async move {
            let _ = p.ledger.call(move |c| crate::ledger::save_learned(c, &key, merged.hashrate, merged.job_ms, merged.clean, merged.samples, unix_now() as i64)).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_low_learned_rate_does_not_pull_a_known_chip_below_its_typical_rate() {
        assert_eq!(start_rate(Some(0.4e12), Some(1.2e12)), Some(1.2e12));
        assert_eq!(start_rate(Some(2.0e12), Some(1.2e12)), Some(2.0e12));
        assert_eq!(start_rate(Some(5.0e6), None), Some(5.0e6));
        assert_eq!(start_rate(None, Some(1.2e12)), Some(1.2e12));
        assert_eq!(start_rate(None, None), None);
    }

    #[test]
    fn learning_takes_the_median_per_device_type() {
        let obs: Vec<Observation> = vec![
            ("bitaxe/bm1370".into(), 1.1e12, 90.0, false), ("bitaxe/bm1370".into(), 1.2e12, 100.0, false), ("bitaxe/bm1370".into(), 5.0e12, 400.0, true),
            ("cpuminer".into(), 5.0e6, 0.0, false),
        ];
        let m = aggregate_types(&obs);
        assert_eq!(m["bitaxe/bm1370"], (1.2e12, 100.0, false));
        assert_eq!(m["cpuminer"], (5.0e6, 0.0, false));
        let clean: Vec<Observation> = vec![("x".into(), 1.0, 50.0, true), ("x".into(), 2.0, 60.0, true), ("x".into(), 3.0, 70.0, false)];
        assert!(aggregate_types(&clean)["x"].2);
    }

    #[test]
    fn job_floor_grows_with_the_connections() {
        let c = cfg();
        // a few connections: the configured minimum; a big farm: the budget decides (50,000 jobs per second in total)
        assert_eq!(floor_ms(&c, 10), c.min_job_interval_ms as f64);
        assert_eq!(floor_ms(&c, 15_000), 300.0);
        // the model alone (with a short floor): a device of 1.2 TH/s sweeps 2^37 hashes in 114 ms, the interval is 0.8 of that
        let ms = job_delay_ms(&c, 12, 1.2e12, false, 25.0);
        assert!((80..114).contains(&ms), "{ms}");
        // the interval never exceeds max_job_interval_ms even when the floor of a huge farm is above it
        let mut tight = cfg();
        tight.max_job_interval_ms = 200;
        assert_eq!(job_delay_ms(&tight, 12, 1.0e12, false, floor_ms(&tight, 100_000)), 200);
    }

    #[test]
    fn a_starved_miner_converges_to_its_real_rate() {
        // the model alone (floor 25 ms), as a property of the formula; the real hardware may behave differently (clean jobs cost it time). The miner's real rate H, but it only does the work it gets: rate(I) = min(H, 2^37 / I). The pool derives the interval from the measured rate.
        let c = cfg();
        let real: f64 = 1.2e12;
        let mut measured: f64 = 0.35e12;
        for _ in 0..12 {
            let i = job_delay_ms(&c, 12, measured, false, 25.0) as f64 / 1000.0;
            measured = real.min(2f64.powi(37) / i);
        }
        assert!(measured > 0.97 * real, "{measured}");
    }

    fn cfg() -> crate::config::StratumCfg {
        toml::from_str("").unwrap()
    }

    #[test]
    fn job_cadence_follows_the_space_a_miner_really_sweeps() {
        let c = cfg();
        // a Bitaxe at 1 TH/s sweeps 2^37 hashes per job in about 0.14 s: fresh work a bit earlier, at 0.8 of that
        assert_eq!(job_delay_ms(&c, 12, 1.0e12, false, 25.0), (2f64.powi(37) / 1.0e12 * 0.8 * 1000.0) as u64);
        // the shortest interval (default 200 ms) is a floor
        assert_eq!(job_delay_ms(&c, 12, 1.0e12, false, c.min_job_interval_ms as f64), c.min_job_interval_ms);
        assert_eq!(job_delay_ms(&c, 12, 1.0e13, false, c.min_job_interval_ms as f64), c.min_job_interval_ms);
        // a 10 GH/s miner can keep one job for about 11 s, a very slow one never longer than the maximum
        assert_eq!(job_delay_ms(&c, 12, 1.0e10, false, c.min_job_interval_ms as f64), (2f64.powi(37) / 1.0e10 * 0.8 * 1000.0) as u64);
        assert_eq!(job_delay_ms(&c, 12, 1.0e9, false, c.min_job_interval_ms as f64), c.max_job_interval_ms);
        assert_eq!(job_delay_ms(&c, 12, 1.0e3, false, c.min_job_interval_ms as f64), c.max_job_interval_ms);
        // miners that granted fewer bits, or a configured smaller space, are not given more than they have
        assert_eq!(job_delay_ms(&c, 2, 1.0e10, false, c.min_job_interval_ms as f64), (2f64.powi(34) / 1.0e10 * 0.8 * 1000.0) as u64);
    }

    #[test]
    fn the_interval_is_learned_from_the_bad_share_ratio() {
        // many duplicates: faster, down to the floor
        let mut ms = 200.0;
        for _ in 0..10 {
            ms = adapt_interval(ms, 10, 5, 0, 50.0, 200.0);
        }
        assert_eq!(ms, 50.0);
        // clean again: slowly slower, up to the cap
        for _ in 0..30 {
            ms = adapt_interval(ms, 20, 0, 0, 50.0, 200.0);
        }
        assert_eq!(ms, 200.0);
        // a few bad shares change nothing, too few shares change nothing
        assert_eq!(adapt_interval(120.0, 97, 3, 0, 50.0, 200.0), 120.0);
        assert_eq!(adapt_interval(120.0, 2, 1, 0, 50.0, 200.0), 120.0);
        // the cap never drops below the floor
        assert_eq!(adapt_interval(100.0, 20, 0, 0, 50.0, 10.0), 50.0);
    }

    #[test]
    fn clean_work_goes_to_the_listed_agents_only() {
        let w = cfg().clean_work_agents;
        assert!(agent_wants_clean(&w, "bitaxe/BM1370/v2.9.0"));
        assert!(agent_wants_clean(&w, "ESP-Miner/2.9"));
        assert!(!agent_wants_clean(&w, "cgminer/4.11"));
        assert!(!agent_wants_clean(&w, ""));
        assert!(agent_wants_clean(&["*".to_string()], "anything"));
        assert!(!agent_wants_clean(&[], "bitaxe"));
    }

    #[test]
    fn unknown_speed_gets_a_job_per_second_and_a_duplicate_makes_it_fast() {
        let c = cfg();
        assert_eq!(job_delay_ms(&c, 12, 0.0, false, c.min_job_interval_ms as f64), 1000);
        assert_eq!(job_delay_ms(&c, 12, 1.0e9, true, c.min_job_interval_ms as f64), c.min_job_interval_ms);
        assert_eq!(job_delay_ms(&c, 12, 0.0, true, c.min_job_interval_ms as f64), c.min_job_interval_ms);
    }
}
