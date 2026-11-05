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
use std::collections::VecDeque;
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
fn job_delay_ms(cfg: &crate::config::StratumCfg, rolling_bits: u32, hash_est: f64, boost: bool) -> u64 {
    let bits = rolling_bits.min(cfg.version_space_bits);
    let unit_hashes = 2f64.powi(32 + bits as i32);
    let secs = if boost { 0.0 } else if hash_est > 0.0 { unit_hashes / hash_est * 0.8 } else { 1.0 };
    (secs * 1000.0).clamp(cfg.min_job_interval_ms as f64, cfg.max_job_interval_ms as f64) as u64
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
        let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
        let _ = stream.set_nodelay(true);
        let (pool, cfg, port) = (pool.clone(), cfg.clone(), port.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let id = pool.session_seq.fetch_add(1, Ordering::Relaxed) + 1;
            let stats = Arc::new(SessionStats::new());
            pool.sessions.lock().unwrap().insert(id, stats.clone());
            let res = Session::new(pool.clone(), cfg, port, stats, id).run(stream).await;
            pool.sessions.lock().unwrap().remove(&id);
            if let Err(e) = res {
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
    last_job: Instant,
    dup_until: Instant,
    agent_clean: bool,
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
    fn new(pool: Arc<Pool>, cfg: Arc<StratumCfg>, port: PortCfg, stats: Arc<SessionStats>, id: u64) -> Self {
        let now = Instant::now();
        let difficulty = cfg.initial_difficulty.clamp(cfg.min_difficulty, cfg.max_difficulty);
        let epoch_rx = pool.epoch.subscribe();
        Self {
            pool, cfg, stats, id, mode: port.mode, port, epoch_rx, subscribed: false, authorized: false, started: false, account: String::new(), worker: String::new(),
            difficulty, target: target_from_difficulty(difficulty), prev: None, mask: 0, rolling_bits: 0, jobs: VecDeque::new(), last_tip: String::new(),
            next_job: now + Duration::from_secs(3600), window_start: now, window_diff: 0.0, hash_est: 0.0, last_job: now, dup_until: now, agent_clean: false, adapt_ms: None, adapt_ok: 0, adapt_dup: 0, adapt_stale: 0, adapt_t: now, last_adjust: now, shares_since_adjust: 0, bad: 0,
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
                r = read_chunk(&mut rd, &mut buf) => match r? {
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
                }
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
                let agent = crate::devices::clean_agent(req.params.get(0).and_then(Value::as_str).unwrap_or(""));
                self.agent_clean = agent_wants_clean(&self.cfg.clean_work_agents, &agent);
                *self.stats.agent.lock().unwrap() = agent;
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

    async fn authorize(&mut self, wr: &mut OwnedWriteHalf, id: &str, params: &Value) -> Result<()> {
        let user = params.get(0).and_then(Value::as_str).unwrap_or("");
        let (account, worker) = user.split_once('.').unwrap_or((user, "default"));
        let hrp = self.pool.cfg.network()?.hrp;
        let valid = !account.is_empty() && (!self.cfg.require_address || script_for_address(account, hrp).is_ok());
        if !valid || worker.len() > 64 {
            self.bad += 1;
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
        let boost = Instant::now() < self.dup_until;
        let model = job_delay_ms(&self.cfg, self.rolling_bits, self.hash_est, boost);
        let ms = match self.adapt_ms {
            Some(a) if !boost => (a.clamp(self.cfg.adaptive_min_interval_ms as f64, self.cfg.max_job_interval_ms as f64)) as u64,
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
        let cap = job_delay_ms(&self.cfg, self.rolling_bits, self.hash_est, false) as f64;
        let floor = self.cfg.adaptive_min_interval_ms as f64;
        let cur = self.adapt_ms.unwrap_or(cap);
        let next = adapt_interval(cur, self.adapt_ok, self.adapt_dup, self.adapt_stale, floor, cap);
        let bad = (self.adapt_dup + self.adapt_stale) as f64 / total as f64;
        // fastest interval and still repeating work: the miner keeps old jobs queued, so give it clean jobs
        if bad > 0.10 && next <= floor * 1.01 && !self.agent_clean {
            self.agent_clean = true;
        }
        self.adapt_ms = Some(next);
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
        cand.write_notify(&mut msg, id, version, ntime, clean);
        send(wr, msg.as_bytes()).await?;
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
        let result = epoch.check(cand_ref, slice, ntime, version, nonce, &target);
        match result {
            ShareResult::Valid | ShareResult::Block => {
                self.adapt_ok += 1;
                self.adapt();
                let d = epoch.share_difficulty(cand_ref, ntime, version, nonce);
                if d > get_f64(&self.stats.best) {
                    set_f64(&self.stats.best, d);
                }
                let agent = self.stats.agent.lock().unwrap().clone();
                self.pool.note_best(&agent, d);
                self.count_ok(credit);
                if result == ShareResult::Block {
                    self.block_found(&epoch, cand_ref, version, ntime, nonce);
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
                self.next_job = self.next_job.min((self.last_job + Duration::from_millis(self.cfg.min_job_interval_ms)).max(Instant::now()));
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
        let rate = self.shares_since_adjust as f64 / elapsed.max(1.0);
        let ratio = rate * self.cfg.share_secs;
        self.last_adjust = Instant::now();
        self.shares_since_adjust = 0;
        if (0.6..=1.5).contains(&ratio) {
            return Ok(());
        }
        let d = self.difficulty * ratio.clamp(0.125, 8.0);
        self.set_difficulty(wr, d).await
    }

    fn block_found(&mut self, epoch: &Arc<Epoch>, c: &Candidate, version: u32, ntime: u32, nonce: u32) {
        self.flush_acc();
        let hex = c.block_hex(version, ntime, nonce);
        let mut h = header_hash(&c.header(version, ntime, nonce));
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
                    pool.event("blk", format!("BLOCK FOUND {height} {}.. by {worker} [{}] -> submitblock: accepted", &hash[..16], mode.as_str()));
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
    // one history sample per minute for the public statistics
    let (mut hist_minute, mut hist_sum, mut hist_n, mut hist_base) = (unix_now() as i64 / 60, 0.0f64, 0u32, (0u64, 0u64, 0u64));
    loop {
        tick.tick().await;
        let sessions: Vec<Arc<SessionStats>> = pool.sessions.lock().unwrap().values().cloned().collect();
        let now = unix_now();
        let (mut hr, mut online, mut shares, mut pps_hr) = (0.0, 0u64, 0u64, 0.0);
        for s in &sessions {
            hr += get_f64(&s.hashrate);
            if s.mode.load(Ordering::Relaxed) == Mode::Pps as u64 {
                pps_hr += get_f64(&s.hashrate);
            }
            if now - get_f64(&s.last_seen) < 120.0 {
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
        live.online = online;
        live.total = sessions.len() as u64;
        drop(live);
        (prev_shares, prev_units, prev_epoch) = (shares, used, eid);
        hist_sum += hr;
        hist_n += 1;
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> crate::config::StratumCfg {
        toml::from_str("").unwrap()
    }

    #[test]
    fn job_cadence_follows_the_space_a_miner_really_sweeps() {
        let c = cfg();
        // a Bitaxe at 1 TH/s sweeps 2^37 hashes per job in about 0.14 s: fresh work at the shortest interval
        assert_eq!(job_delay_ms(&c, 12, 1.0e12, false), c.min_job_interval_ms);
        // a 10 GH/s miner can keep one job for about 11 s, a very slow one never longer than the maximum
        assert_eq!(job_delay_ms(&c, 12, 1.0e10, false), (2f64.powi(37) / 1.0e10 * 0.8 * 1000.0) as u64);
        assert_eq!(job_delay_ms(&c, 12, 1.0e9, false), c.max_job_interval_ms);
        assert_eq!(job_delay_ms(&c, 12, 1.0e3, false), c.max_job_interval_ms);
        // miners that granted fewer bits, or a configured smaller space, are not given more than they have
        assert_eq!(job_delay_ms(&c, 2, 1.0e10, false), (2f64.powi(34) / 1.0e10 * 0.8 * 1000.0) as u64);
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
        assert_eq!(job_delay_ms(&c, 12, 0.0, false), 1000);
        assert_eq!(job_delay_ms(&c, 12, 1.0e9, true), c.min_job_interval_ms);
        assert_eq!(job_delay_ms(&c, 12, 0.0, true), c.min_job_interval_ms);
    }
}
