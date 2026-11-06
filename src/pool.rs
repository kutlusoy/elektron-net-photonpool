use crate::addr::script_for_address;
use crate::config::{Config, Mode};
use crate::ledger::Ledger;
use crate::rpc::NodeRpc;
use crate::template::{header_hash, meets_target, BatchTemplate, Candidate, Gbt, TxSet};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, watch};
use std::sync::RwLock;

pub const NTIME_SLOTS: u64 = 60;
pub const SLICES: u64 = 16;
pub const POOL_VERSION_SHIFT: u32 = 13;
pub const MINER_VERSION_MASK: u32 = 0x1ffe_0000;

pub fn unix_now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

#[derive(Clone, Serialize)]
pub struct Event {
    pub t: f64,
    pub lvl: &'static str,
    pub msg: String,
}

pub struct Payout {
    pub address: String,
    pub script: Vec<u8>,
}

#[derive(Serialize, Clone)]
pub struct FoundBlock {
    pub height: u32,
    pub hash: String,
    pub time: f64,
    pub worker: String,
    pub mode: &'static str,
    pub status: &'static str,
}

/// What the pool has learned about a kind of device (see `devices::profile_key`): typical hash rate, job interval, whether it needs clean jobs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Learned {
    pub hashrate: f64,
    pub job_ms: f64,
    pub clean: bool,
    pub samples: u32,
}

/// Folds the current observation of a device type (median over its connections) into what was learned before: a moving average that follows slowly, so one odd
/// connection does not change much, and that is replaced outright by the first observation.
pub fn merge_learned(old: Option<Learned>, hashrate: f64, job_ms: f64, clean: bool) -> Learned {
    match old {
        None => Learned { hashrate, job_ms, clean, samples: 1 },
        Some(o) => {
            let w = 0.1;
            Learned {
                hashrate: o.hashrate * (1.0 - w) + hashrate * w,
                job_ms: if job_ms > 0.0 && o.job_ms > 0.0 { o.job_ms * (1.0 - w) + job_ms * w } else if job_ms > 0.0 { job_ms } else { o.job_ms },
                clean: if job_ms > 0.0 { clean } else { o.clean },
                samples: o.samples.saturating_add(1),
            }
        }
    }
}

#[derive(Default)]
pub struct Stats {
    pub ok: AtomicU64,
    pub stale: AtomicU64,
    pub dup: AtomicU64,
    pub inv: AtomicU64,
    pub candidates_rejected: AtomicU64,
    pub last_fetch_ms: AtomicU64,
    pub connected: AtomicBool,
    pub tip_ready_ms: Mutex<VecDeque<f64>>,
    pub found: Mutex<Vec<FoundBlock>>,
}

pub fn set_f64(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

pub fn get_f64(a: &AtomicU64) -> f64 {
    f64::from_bits(a.load(Ordering::Relaxed))
}

/// Counters of one miner connection, read lock-free by the admin API.
pub struct SessionStats {
    pub name: Mutex<String>,
    pub ok: AtomicU64,
    pub stale: AtomicU64,
    pub dup: AtomicU64,
    pub inv: AtomicU64,
    pub hashrate: AtomicU64,
    pub difficulty: AtomicU64,
    pub last_seen: AtomicU64,
    pub unit: AtomicU64,
    pub mode: AtomicU64,
    /// Job interval in milliseconds as f64 bits (what the learning chose for this connection).
    pub job_ms: AtomicU64,
    /// User agent sent with mining.subscribe (printable ASCII, at most 64 characters).
    pub agent: Mutex<String>,
    /// (key, name) of the device type, from the agent (set when the miner subscribes).
    pub device: Mutex<(String, String)>,
    /// Best share difficulty of this connection, as f64 bits.
    pub best: AtomicU64,
    /// Network counters of this connection (bytes read, bytes written, jobs sent); summed once a second.
    pub net_in: AtomicU64,
    pub net_out: AtomicU64,
    pub net_jobs: AtomicU64,
    /// Job interval the pool learned for this connection from its duplicate and stale shares (f64 bits, 0 = not learned yet) and whether that made it use clean jobs.
    pub adapt_ms: AtomicU64,
    pub clean_learned: AtomicU64,
}

impl SessionStats {
    pub fn new() -> Self {
        let now = AtomicU64::new(0);
        set_f64(&now, unix_now());
        Self {
            name: Mutex::new(String::new()), ok: AtomicU64::new(0), stale: AtomicU64::new(0), dup: AtomicU64::new(0), inv: AtomicU64::new(0),
            hashrate: AtomicU64::new(0), difficulty: AtomicU64::new(0), last_seen: now, unit: AtomicU64::new(0), mode: AtomicU64::new(1), job_ms: AtomicU64::new(0), agent: Mutex::new(String::new()), device: Mutex::new(("other:unknown".into(), "unknown".into())), best: AtomicU64::new(0), net_in: AtomicU64::new(0), net_out: AtomicU64::new(0), net_jobs: AtomicU64::new(0), adapt_ms: AtomicU64::new(0), clean_learned: AtomicU64::new(0),
        }
    }
}

#[derive(Default)]
pub struct Live {
    pub hashrate: f64,
    pub m1: f64,
    pub h1: f64,
    pub hist: VecDeque<f64>,
    pub shares_per_sec: f64,
    pub units_per_sec: f64,
    pub online: u64,
    pub total: u64,
    pub pps_hashrate: f64,
    /// Network rates of the last second: bytes in and out, jobs sent, new connections.
    pub in_bps: f64,
    pub out_bps: f64,
    pub jobs_per_sec: f64,
    pub connects_per_sec: f64,
    /// Bytes in and out since the start (closed sessions plus live ones), sampled once a second.
    pub tot_in: u64,
    pub tot_out: u64,
    /// Started connections and their summed hash rate per payout mode (index: solo, pplns, pps).
    pub by_mode: [(u64, f64); 3],
}

pub struct Unit {
    pub cand: usize,
    pub slice: u8,
    pub ntime_off: u32,
}

pub struct Submission {
    pub cand: usize,
    pub slice: u8,
    pub ntime: u32,
    pub version: u32,
    pub nonce: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ShareResult {
    Block,
    Valid,
    LowDifficulty,
    Duplicate,
    Invalid(&'static str),
}

/// All candidates for one tip, plus the unit allocator and duplicate filter.
pub struct Epoch {
    pub id: u64,
    pub tip_hex: String,
    pub height: u32,
    pub candidates: Vec<Candidate>,
    /// Version-bit slices per candidate: 16 when the pool reserves bits 13-16 to tell miners apart, 1 when every unit gets all 16 BIP 320 bits.
    pub slices: u64,
    pub started_unix: f64,
    next_unit: AtomicU64,
    pub overlap: AtomicU64,
    seen: Mutex<HashSet<u128>>,
    solo: RwLock<HashMap<String, Arc<SoloEntry>>>,
    solo_build: tokio::sync::Mutex<()>,
}

/// Direct-payout candidate of one solo account: the coinbase pays the miner's own address.
pub struct SoloEntry {
    pub cand: Candidate,
    next: AtomicU64,
    pub overlap: AtomicU64,
}

impl SoloEntry {
    pub fn new(cand: Candidate) -> Self {
        Self { cand, next: AtomicU64::new(0), overlap: AtomicU64::new(0) }
    }

    pub fn next_unit(&self) -> (u32, u8, u32) {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        if n >= SLICES * NTIME_SLOTS {
            self.overlap.fetch_add(1, Ordering::Relaxed);
        }
        let (slice, off) = Self::unit_from_n(n as u32);
        (n as u32, slice, off)
    }

    pub fn unit_from_n(n: u32) -> (u8, u32) {
        let u = n as u64 % (SLICES * NTIME_SLOTS);
        ((u % SLICES) as u8, (u / SLICES) as u32)
    }
}

impl Epoch {
    pub fn new(id: u64, tip_hex: String, height: u32, candidates: Vec<Candidate>) -> Self {
        Self::with_slices(id, tip_hex, height, candidates, SLICES)
    }

    pub fn with_slices(id: u64, tip_hex: String, height: u32, candidates: Vec<Candidate>, slices: u64) -> Self {
        let slices = slices.clamp(1, SLICES);
        Self { id, tip_hex, height, candidates, slices, started_unix: unix_now(), next_unit: AtomicU64::new(0), overlap: AtomicU64::new(0), seen: Mutex::new(HashSet::new()), solo: RwLock::new(HashMap::new()), solo_build: tokio::sync::Mutex::new(()) }
    }

    pub fn units_total(&self) -> u64 {
        self.candidates.len() as u64 * self.slices * NTIME_SLOTS
    }

    pub fn units_used(&self) -> u64 {
        self.next_unit.load(Ordering::Relaxed).min(self.units_total())
    }

    /// Hands out each (candidate, slice, ntime offset) once per epoch; wrapping is counted as overlap.
    pub fn next_unit(&self) -> (u32, Unit) {
        let n = self.next_unit.fetch_add(1, Ordering::Relaxed);
        if n >= self.units_total() {
            self.overlap.fetch_add(1, Ordering::Relaxed);
        }
        (n as u32, self.unit_from_n(n as u32))
    }

    pub fn unit_from_n(&self, n: u32) -> Unit {
        let c = self.candidates.len() as u64;
        let u = n as u64 % self.units_total();
        Unit { cand: (u % c) as usize, slice: ((u / c) % self.slices) as u8, ntime_off: (u / (c * self.slices)) as u32 }
    }

    pub fn tip_matches(&self, other: &Epoch) -> bool {
        self.tip_hex == other.tip_hex
    }

    pub fn check_share(&self, s: &Submission, share_target: &[u8; 32]) -> ShareResult {
        let Some(c) = self.candidates.get(s.cand) else { return ShareResult::Invalid("unknown candidate") };
        self.check(c, s.slice, s.ntime, s.version, s.nonce, share_target)
    }

    pub fn check(&self, c: &Candidate, slice: u8, ntime: u32, version: u32, nonce: u32, share_target: &[u8; 32]) -> ShareResult {
        self.check_with_mask(c, slice, ntime, version, nonce, share_target, MINER_VERSION_MASK)
    }

    /// Like `check`, for a miner that negotiated `miner_mask` (all of BIP 320 when the epoch has one slice).
    #[allow(clippy::too_many_arguments)]
    pub fn check_with_mask(&self, c: &Candidate, slice: u8, ntime: u32, version: u32, nonce: u32, share_target: &[u8; 32], miner_mask: u32) -> ShareResult {
        if slice as u64 >= self.slices {
            return ShareResult::Invalid("bad slice");
        }
        let expected = (c.version | ((slice as u32) << POOL_VERSION_SHIFT)) & !miner_mask;
        if (version & !miner_mask) != expected {
            return ShareResult::Invalid("version bits outside the miner mask");
        }
        if ntime < c.mintime || ntime > c.curtime.saturating_add(NTIME_SLOTS as u32 + 60) {
            return ShareResult::Invalid("ntime out of range");
        }
        let hash = header_hash(&c.header(version, ntime, nonce));
        let is_block = meets_target(&hash, &c.target);
        if !is_block && !meets_target(&hash, share_target) {
            return ShareResult::LowDifficulty;
        }
        let key = u128::from_le_bytes(hash[..16].try_into().unwrap());
        if !self.seen.lock().unwrap().insert(key) {
            return ShareResult::Duplicate;
        }
        if is_block {
            ShareResult::Block
        } else {
            ShareResult::Valid
        }
    }

    /// Difficulty of the block hash of a share (for the best-share statistics).
    pub fn share_difficulty(&self, c: &Candidate, ntime: u32, version: u32, nonce: u32) -> f64 {
        let mut h = header_hash(&c.header(version, ntime, nonce));
        h.reverse();
        crate::diff::difficulty_from_target(&h)
    }

    pub fn solo_entry(&self, address: &str) -> Option<Arc<SoloEntry>> {
        self.solo.read().unwrap().get(address).cloned()
    }

    pub fn solo_count(&self) -> usize {
        self.solo.read().unwrap().len()
    }
}

pub struct Pool {
    pub cfg: Config,
    pub node: NodeRpc,
    pub payouts: Vec<Payout>,
    pub epoch: watch::Sender<Option<Arc<Epoch>>>,
    pub stats: Stats,
    pub events: broadcast::Sender<Event>,
    pub started: Instant,
    epoch_seq: AtomicU64,
    pub sessions: Mutex<HashMap<u64, Arc<SessionStats>>>,
    pub session_seq: AtomicU64,
    /// Open Stratum connections (for the job budget).
    pub conns: AtomicU64,
    pub live: Mutex<Live>,
    pub solo_active: Mutex<HashMap<String, Instant>>,
    pub pps_ready: AtomicBool,
    pub ledger: Ledger,
    pub agg: Mutex<HashMap<(i64, String, u8), (f64, u64)>>,
    pub wallet: Option<NodeRpc>,
    pub treasury_free: AtomicI64,
    /// 0 = batch attestation not tried yet, 1 = supported by the node, 2 = not supported.
    pub batch_state: std::sync::atomic::AtomicU8,
    pub backend_name: Mutex<&'static str>,
    /// Candidates in use (the first ones of `payouts`); grows and shrinks with the work demand when autoscaling is on.
    pub active_candidates: AtomicUsize,
    /// Work units per second handed out in the last epochs, as f64 bits.
    pub demand_units: AtomicU64,
    /// Best share difficulty per device type (key of `devices::classify`), kept in the ledger.
    pub device_best: Mutex<HashMap<String, f64>>,
    /// Learned values per device type, loaded from the ledger and updated once a minute.
    pub learned: Mutex<HashMap<String, Learned>>,
    /// Answers of the history API, cached for a few seconds: range -> (time, json).
    pub hist_cache: Mutex<HashMap<String, (Instant, serde_json::Value)>>,
    /// The events that are fit for the public page (see `public_message`), newest last: (sequence number, time, level, text).
    pub public_events: Mutex<VecDeque<(u64, f64, &'static str, String)>>,
    pub public_seq: AtomicU64,
    pub photon: std::sync::OnceLock<Arc<crate::photon::service::PhotonService>>,
}

impl Pool {
    pub async fn connect(cfg: Config) -> Result<Arc<Pool>> {
        let net = cfg.network()?;
        let node = NodeRpc::new(&cfg.node.rpc_url, &cfg.node.auth)?;
        let (chain, height, syncing) = node.chain_info().await.context("cannot reach the node")?;
        if chain != net.chain {
            bail!("node is on chain {chain}, config says {}", cfg.pool.network);
        }
        if syncing {
            bail!("node is still syncing (height {height})");
        }
        if height < net.min_height {
            bail!("node height {height} is below the attestation fix height {}", net.min_height);
        }
        let wallet = if cfg.payout.wallet.is_empty() { None } else { Some(crate::accounting::wallet_rpc(&cfg.node.rpc_url, &cfg.node.auth, &cfg.payout.wallet)?) };
        let mut addresses = cfg.pool.payout_addresses.clone();
        if let (Some(w), n) = (&wallet, cfg.payout.wallet_addresses) {
            if n > 0 {
                for a in crate::accounting::provision_wallet_addresses(&node, w, &cfg.payout.wallet, n, &cfg.payout.wallet_passphrase_env).await.context("provisioning pool addresses from the wallet")? {
                    if !addresses.contains(&a) {
                        addresses.push(a);
                    }
                }
            }
        }
        let payouts = addresses
            .iter()
            .map(|a| Ok(Payout { address: a.clone(), script: script_for_address(a, net.hrp)? }))
            .collect::<Result<Vec<_>>>()?;
        let active = if cfg.pool.autoscale_candidates { cfg.pool.min_candidates.clamp(1, payouts.len().max(1)) } else { payouts.len() };
        let ledger = Ledger::open(&cfg.ledger.path).with_context(|| format!("opening ledger {}", cfg.ledger.path))?;
        let learned: HashMap<String, Learned> = ledger.call(|c| crate::ledger::load_learned(c)).await.unwrap_or_default().into_iter().map(|(k, h, j, c, s)| (k, Learned { hashrate: h, job_ms: j, clean: c, samples: s })).collect();
        let bests: HashMap<String, f64> = ledger.call(|c| crate::ledger::device_bests(c)).await.unwrap_or_default().into_iter().collect();
        let (events, _) = broadcast::channel(1024);
        let (epoch, _) = watch::channel(None);
        let pool = Arc::new(Pool {
            cfg, node, payouts, epoch, stats: Stats::default(), events, started: Instant::now(), epoch_seq: AtomicU64::new(0),
            sessions: Mutex::new(HashMap::new()), session_seq: AtomicU64::new(0), conns: AtomicU64::new(0), live: Mutex::new(Live::default()), solo_active: Mutex::new(HashMap::new()),
            pps_ready: AtomicBool::new(false), ledger, agg: Mutex::new(HashMap::new()), wallet, treasury_free: AtomicI64::new(0),
            batch_state: std::sync::atomic::AtomicU8::new(0), backend_name: Mutex::new("gbt"), active_candidates: AtomicUsize::new(active), demand_units: AtomicU64::new(0), device_best: Mutex::new(bests), learned: Mutex::new(learned), hist_cache: Mutex::new(HashMap::new()), public_events: Mutex::new(VecDeque::new()), public_seq: AtomicU64::new(0), photon: std::sync::OnceLock::new(),
        });
        pool.stats.connected.store(true, Ordering::Relaxed);
        Ok(pool)
    }

    pub fn event(&self, lvl: &'static str, msg: String) {
        match lvl {
            "err" => tracing::error!("{msg}"),
            "warn" => tracing::warn!("{msg}"),
            _ => tracing::info!("{msg}"),
        }
        if let Some(m) = public_message(lvl, &msg) {
            let seq = self.public_seq.fetch_add(1, Ordering::Relaxed) + 1;
            let mut q = self.public_events.lock().unwrap();
            q.push_back((seq, unix_now(), lvl, m));
            if q.len() > 200 {
                q.pop_front();
            }
        }
        let _ = self.events.send(Event { t: unix_now(), lvl, msg });
    }

    /// Whether a connection may start in `mode` right now; the error text is shown to the miner.
    pub fn mode_available(&self, mode: Mode) -> Result<(), &'static str> {
        if !self.cfg.payout.offers(mode) {
            return Err("mode not offered");
        }
        if mode == Mode::Pps && !self.pps_ready.load(Ordering::Relaxed) {
            return Err("PPS is paused until the pool reserve is funded");
        }
        Ok(())
    }

    pub fn touch_solo(&self, address: &str) {
        let mut m = self.solo_active.lock().unwrap();
        m.insert(address.to_string(), Instant::now());
        if m.len() > self.cfg.payout.max_solo_accounts * 2 {
            m.retain(|_, t| t.elapsed() < Duration::from_secs(600));
        }
    }

    async fn build_solo(&self, address: &str, tip: &str) -> Result<SoloEntry> {
        let script = script_for_address(address, self.cfg.network()?.hrp)?;
        let g: Gbt = serde_json::from_value(self.node.template(address).await?)?;
        if g.previousblockhash != tip {
            bail!("tip moved while building a solo candidate");
        }
        let c = Candidate::from_gbt(g, &script, None)?;
        if let Some(r) = self.node.proposal(&c.block_hex(c.version, c.curtime, 0)).await? {
            bail!("node rejected the solo candidate: {r}");
        }
        Ok(SoloEntry::new(c))
    }

    /// Returns the direct-payout candidate of a solo account for this epoch, building it when needed.
    pub async fn ensure_solo(&self, epoch: &Arc<Epoch>, address: &str) -> Result<Arc<SoloEntry>> {
        if let Some(e) = epoch.solo_entry(address) {
            return Ok(e);
        }
        let _guard = epoch.solo_build.lock().await;
        if let Some(e) = epoch.solo_entry(address) {
            return Ok(e);
        }
        if epoch.solo_count() >= self.cfg.payout.max_solo_accounts {
            bail!("solo account limit reached");
        }
        let entry = Arc::new(self.build_solo(address, &epoch.tip_hex).await?);
        epoch.solo.write().unwrap().insert(address.to_string(), entry.clone());
        Ok(entry)
    }

    async fn prebuild_solo(self: Arc<Self>, epoch: Arc<Epoch>) {
        let addrs: Vec<String> = {
            let mut m = self.solo_active.lock().unwrap();
            m.retain(|_, t| t.elapsed() < Duration::from_secs(600));
            m.keys().cloned().collect()
        };
        for a in addrs {
            if self.ensure_solo(&epoch, &a).await.is_err() {
                break;
            }
        }
    }

    /// Fetches one candidate per payout address for the current tip and validates `check` of them against the node.
    async fn fetch_candidates(&self, best: &str) -> Result<(Vec<Candidate>, u32)> {
        use std::sync::atomic::Ordering::Relaxed;
        let mode = self.cfg.pool.backend.as_str();
        if mode != "gbt" && self.batch_state.load(Relaxed) != 2 {
            match self.fetch_batch(best).await {
                Ok(r) => {
                    if self.batch_state.swap(1, Relaxed) != 1 {
                        *self.backend_name.lock().unwrap() = "batch";
                        self.event("info", "candidate backend: batch attestation (getattestationbatch)".into());
                    }
                    return Ok(r);
                }
                Err(e) if mode == "auto" && self.batch_state.load(Relaxed) == 0 && e.to_string().contains("Method not found") => {
                    self.batch_state.store(2, Relaxed);
                    self.event("info", "node has no getattestationbatch, using one getblocktemplate call per address".into());
                }
                Err(e) => return Err(e),
            }
        }
        let active = self.active_candidates.load(Ordering::Relaxed).clamp(1, self.payouts.len().max(1));
        let mut candidates: Vec<Candidate> = Vec::with_capacity(active);
        let mut height = 0;
        for p in &self.payouts[..active.min(self.payouts.len())] {
            let g: Gbt = serde_json::from_value(self.node.template(&p.address).await?)?;
            if g.previousblockhash != best {
                bail!("tip moved while building candidates");
            }
            height = g.height;
            let reuse: Option<std::sync::Arc<TxSet>> = candidates.last().map(|c| c.txs.clone());
            candidates.push(Candidate::from_gbt(g, &p.script, reuse.as_ref())?);
        }
        Ok((candidates, height))
    }

    async fn fetch_batch(&self, best: &str) -> Result<(Vec<Candidate>, u32)> {
        const CHUNK: usize = 2000;
        let active = self.active_candidates.load(Ordering::Relaxed).clamp(1, self.payouts.len().max(1));
        let mut candidates: Vec<Candidate> = Vec::with_capacity(active);
        let mut height = 0;
        let mut shared: Option<std::sync::Arc<TxSet>> = None;
        for chunk in self.payouts[..active.min(self.payouts.len())].chunks(CHUNK) {
            let scripts: Vec<String> = chunk.iter().map(|p| hex::encode(&p.script)).collect();
            let bt: BatchTemplate = serde_json::from_value(self.node.attestation_batch(&scripts).await?)?;
            if bt.tip != best {
                bail!("tip moved while building candidates");
            }
            if bt.candidates.len() != chunk.len() {
                bail!("node returned {} candidates for {} requested", bt.candidates.len(), chunk.len());
            }
            let txs = match &shared {
                Some(t) if t.matches(&bt.transactions) => t.clone(),
                _ => std::sync::Arc::new(TxSet::from_gbt(&bt.transactions)?),
            };
            shared = Some(txs.clone());
            height = bt.height;
            for (p, c) in chunk.iter().zip(&bt.candidates) {
                candidates.push(Candidate::from_batch(&bt, &txs, c, &p.script)?);
            }
        }
        Ok((candidates, height))
    }

    /// Records a share difficulty for the best-share figures of a device type; the ledger is only written when the record is beaten.
    pub fn note_best(self: &Arc<Self>, agent: &str, difficulty: f64) {
        if !difficulty.is_finite() || difficulty <= 0.0 {
            return;
        }
        let dev = crate::devices::classify(agent);
        let beaten = {
            let mut m = self.device_best.lock().unwrap();
            let old = m.get(&dev.key).copied().unwrap_or(0.0);
            if difficulty > old {
                m.insert(dev.key.clone(), difficulty);
                true
            } else {
                false
            }
        };
        if beaten {
            let pool = self.clone();
            tokio::spawn(async move {
                let (key, name) = (dev.key, dev.name);
                let _ = pool.ledger.call(move |c| crate::ledger::bump_device_best(c, &key, &name, difficulty, unix_now() as i64)).await;
            });
        }
    }

    /// Sets how many candidates the next epoch uses from how fast the work units of the last one were used up.
    fn autoscale(&self) {
        if !self.cfg.pool.autoscale_candidates {
            return;
        }
        let Some(prev) = self.epoch.borrow().clone() else { return };
        let age = unix_now() - prev.started_unix;
        if age < 3.0 {
            return;
        }
        let rate = prev.next_unit.load(Ordering::Relaxed) as f64 / age;
        let old = get_f64(&self.demand_units);
        // up at once, down by half per epoch
        let demand = if rate > old { rate } else { old * 0.5 + rate * 0.5 };
        set_f64(&self.demand_units, demand);
        let horizon = if self.cfg.pool.refresh_secs > 0 { self.cfg.pool.refresh_secs as f64 } else { 60.0 };
        let want = candidates_needed(demand, horizon, prev.slices, self.cfg.pool.min_candidates, self.payouts.len());
        let have = self.active_candidates.swap(want, Ordering::Relaxed);
        if want != have {
            self.event("info", format!("work space: {have} -> {want} candidates ({demand:.0} units/s used, {} available)", self.payouts.len()));
        }
    }

    pub async fn build_epoch(&self, check: usize) -> Result<Epoch> {
        self.autoscale();
        let t0 = Instant::now();
        let best = self.node.best_block_hash().await?;
        let (candidates, height) = self.fetch_candidates(&best).await?;
        self.stats.last_fetch_ms.store(t0.elapsed().as_millis() as u64, Ordering::Relaxed);

        let n = candidates.len();
        let mut bad = HashSet::new();
        for i in 0..check.min(n) {
            let idx = (i * n.max(1) / check.max(1)).min(n - 1);
            let c = &candidates[idx];
            match self.node.proposal(&c.block_hex(c.version, c.curtime, 0)).await? {
                None => {}
                Some(r) if r.starts_with("inconclusive") => bail!("tip moved during validation"),
                Some(r) => {
                    self.stats.candidates_rejected.fetch_add(1, Ordering::Relaxed);
                    self.event("err", format!("candidate {idx} rejected by node: {r}"));
                    bad.insert(idx);
                }
            }
        }
        let candidates: Vec<Candidate> = candidates.into_iter().enumerate().filter(|(i, _)| !bad.contains(i)).map(|(_, c)| c).collect();
        if candidates.is_empty() {
            bail!("no valid candidates");
        }
        let id = self.epoch_seq.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(Epoch::new(id, best, height, candidates))
    }

    /// The current epoch has used up (nearly) all its work units although more candidates are available: build a bigger one now instead of waiting for the refresh.
    fn epoch_exhausted(&self, since_build: Duration) -> bool {
        if !self.cfg.pool.autoscale_candidates || since_build < Duration::from_secs(3) || self.active_candidates.load(Ordering::Relaxed) >= self.payouts.len() {
            return false;
        }
        self.epoch.borrow().as_ref().map_or(false, |e| e.units_used() * 10 >= e.units_total() * 9)
    }

    pub async fn run_tip_loop(self: Arc<Self>) {
        let poll = Duration::from_millis(self.cfg.node.poll_ms);
        let refresh = Duration::from_secs(self.cfg.pool.refresh_secs);
        let (mut last_tip, mut last_build) = (String::new(), Instant::now());
        let mut was_down = false;
        loop {
            tokio::time::sleep(poll).await;
            let best = match self.node.best_block_hash().await {
                Ok(b) => b,
                Err(e) => {
                    self.stats.connected.store(false, Ordering::Relaxed);
                    if !was_down {
                        self.event("err", format!("node link lost: {e}"));
                    }
                    was_down = true;
                    continue;
                }
            };
            if was_down {
                self.event("info", "node link restored".into());
                was_down = false;
            }
            self.stats.connected.store(true, Ordering::Relaxed);
            let tip_changed = best != last_tip;
            if !tip_changed && !(refresh.as_secs() > 0 && last_build.elapsed() >= refresh) && !self.epoch_exhausted(last_build.elapsed()) {
                continue;
            }
            let detected = Instant::now();
            match self.build_epoch(self.cfg.pool.spot_check).await {
                Ok(e) => {
                    let ms = detected.elapsed().as_secs_f64() * 1000.0;
                    if tip_changed {
                        let mut q = self.stats.tip_ready_ms.lock().unwrap();
                        q.push_back(ms);
                        if q.len() > 400 {
                            q.pop_front();
                        }
                    }
                    self.event("info", format!("epoch {} height {} tip {} {} candidates ready in {:.0} ms", e.id, e.height, e.tip_hex, e.candidates.len(), ms));
                    last_tip = e.tip_hex.clone();
                    last_build = Instant::now();
                    let e = Arc::new(e);
                    self.epoch.send_replace(Some(e.clone()));
                    tokio::spawn(self.clone().prebuild_solo(e));
                }
                Err(e) => {
                    self.event("warn", format!("epoch build failed: {e}"));
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    }
}

fn mask_account(a: &str) -> String {
    if a.len() > 14 { format!("{}..{}", &a[..6], &a[a.len() - 4..]) } else { a.to_string() }
}

/// The text of an event as the public page may show it, or None when it is not for the public: only epochs, the work space, the candidate backend,
/// the node link, block status changes, PPS availability, payouts (count and txid only), the number of connected miners and found blocks (with the finder masked, no worker name). Share, job, error and start-up lines never leave the operator view.
pub fn public_message(lvl: &str, msg: &str) -> Option<String> {
    if msg.starts_with("epoch ") && msg.contains(" candidates ready in ") || msg.starts_with("work space:") || msg.starts_with("candidate backend:") {
        return Some(msg.to_string());
    }
    if msg.starts_with("node link lost") {
        return Some("node link lost".into());
    }
    if msg.starts_with("node link restored") || msg.starts_with("PPS is now ") || msg.starts_with("miners online: ") {
        return Some(msg.to_string());
    }
    // block status changes ("block <hash> is mature"), credits of a pool block and broadcast payouts (hashes and txids are public chain data, no account)
    if (msg.starts_with("block ") && (msg.contains(" is ") || msg.ends_with(" PPLNS credits recorded"))) || (msg.starts_with("payout ") && msg.contains(" accounts, tx ")) {
        return Some(msg.to_string());
    }
    if lvl == "blk" && msg.starts_with("BLOCK FOUND ") {
        // BLOCK FOUND <height> <hash>.. by <account>.<worker> [<mode>] -> submitblock: accepted
        let (head, rest) = msg.split_once(" by ")?;
        let (who, tail) = rest.split_once(" [")?;
        let account = who.split('.').next().unwrap_or("");
        return Some(format!("{head} by {} [{tail}", mask_account(account)));
    }
    None
}

/// Candidates needed to hand out `units_per_sec` work units for `horizon_secs` (one epoch) without repeating any, with 25 % headroom, within `min..=max`.
pub fn candidates_needed(units_per_sec: f64, horizon_secs: f64, slices: u64, min: usize, max: usize) -> usize {
    let per_candidate = (slices * NTIME_SLOTS) as f64;
    let want = (units_per_sec * horizon_secs * 1.25 / per_candidate).ceil() as usize;
    let max = max.max(1);
    // fewer addresses than `min_candidates` is fine: use all of them (clamp would panic when min > max)
    want.clamp(min.clamp(1, max), max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learned_values_follow_slowly_and_start_from_the_first_observation() {
        let first = merge_learned(None, 1.0e12, 0.0, false);
        assert_eq!((first.hashrate, first.samples, first.job_ms), (1.0e12, 1, 0.0));
        let next = merge_learned(Some(first), 2.0e12, 100.0, true);
        assert!((next.hashrate - 1.1e12).abs() < 1.0);
        assert_eq!((next.job_ms, next.clean, next.samples), (100.0, true, 2));
        // an observation without a learned interval keeps the one that was learned
        let kept = merge_learned(Some(next), 1.1e12, 0.0, false);
        assert_eq!((kept.job_ms, kept.clean), (100.0, true));
        let moved = merge_learned(Some(kept), 1.1e12, 50.0, true);
        assert!((moved.job_ms - 95.0).abs() < 1e-9);
    }

    #[test]
    fn only_harmless_events_are_public_and_the_finder_is_masked() {
        let ok = "epoch 48 height 297373 tip 000000000000.. 1 candidates ready in 2 ms";
        assert_eq!(public_message("info", ok).as_deref(), Some(ok));
        assert_eq!(public_message("info", "work space: 16 -> 40 candidates (4000 units/s used, 512 available)").is_some(), true);
        assert_eq!(public_message("warn", "node link lost: connection refused to http://127.0.0.1:8332 (cookie /node/.cookie)").as_deref(), Some("node link lost"));
        assert!(public_message("info", "block 0123456789abcdef is mature").is_some());
        assert!(public_message("info", "PPS is now paused").is_some());
        assert!(public_message("info", "payout 7: 3 accounts, tx 00ff").is_some());
        assert!(public_message("info", "dry run: would pay 3 accounts, 1.5 coins in total").is_none());
        assert!(public_message("info", "miners online: 5 (+1)").is_some());
        let blk = public_message("blk", "BLOCK FOUND 131 0123456789abcdef.. by be1qxym6egwr7esxryrayg2fdnnprl9zfepvc9p6z2.bitaxe3 [solo] -> submitblock: accepted").unwrap();
        assert_eq!(blk, "BLOCK FOUND 131 0123456789abcdef.. by be1qxy..p6z2 [solo] -> submitblock: accepted");
        assert!(!blk.contains("bitaxe3"));
        for (l, m) in [("share", "share ok be1q.rig1"), ("err", "ledger could not record block 5: disk full"), ("info", "stratum v1 listening on 0.0.0.0:3333"), ("info", "photond 0.2 listening on 127.0.0.1:8081"), ("err", "block 9 by x.y rejected by node: bad-txns")] {
            assert_eq!(public_message(l, m), None, "{m}");
        }
    }

    #[test]
    fn candidates_follow_the_demand() {
        // 1,000 Bitaxes at 5 jobs per second, 20 s epochs, 16 slices x 60 ntime steps: about 130 candidates
        assert_eq!(candidates_needed(5000.0, 20.0, 16, 16, 4096), 131);
        // little demand stays at the minimum, a huge demand is capped at the addresses there are
        assert_eq!(candidates_needed(10.0, 20.0, 16, 16, 4096), 16);
        assert_eq!(candidates_needed(1.0e6, 20.0, 16, 16, 512), 512);
        // never more than there are, never fewer than one
        assert_eq!(candidates_needed(0.0, 20.0, 16, 0, 0), 1);
        // a pool with fewer addresses than the minimum uses all of them (this once panicked in the epoch loop)
        assert_eq!(candidates_needed(0.0, 60.0, 16, 16, 2), 2);
        assert_eq!(candidates_needed(1.0e6, 60.0, 16, 16, 8), 8);
    }

    fn dummy(n: usize, target: u8) -> Epoch {
        let c = |i: u8| Candidate {
            prev: [i; 32], version: 0x2000_0000, bits: 0x207f_ffff, target: [target; 32], curtime: 1000, mintime: 900, coinbase_value: 0, fees: 0,
            coinbase_wit: vec![], merkle_root: [i; 32],
            txs: std::sync::Arc::new(TxSet { datas: vec![], txids: vec![], branch: vec![] }),
            notify_b: String::new(), notify_c: String::new(),
        };
        Epoch::new(1, "00".repeat(32), 1, (0..n as u8).map(c).collect())
    }

    #[test]
    fn one_slice_gives_every_unit_the_whole_version_space() {
        let c = |i: u8| Candidate {
            prev: [i; 32], version: 0x2000_0000, bits: 0x207f_ffff, target: [0; 32], curtime: 1000, mintime: 900, coinbase_value: 0, fees: 0,
            coinbase_wit: vec![], merkle_root: [i; 32],
            txs: std::sync::Arc::new(TxSet { datas: vec![], txids: vec![], branch: vec![] }),
            notify_b: String::new(), notify_c: String::new(),
        };
        let e = Epoch::with_slices(1, "00".repeat(32), 1, vec![c(1), c(2), c(3)], 1);
        assert_eq!(e.units_total(), 3 * NTIME_SLOTS);
        let mut seen = HashSet::new();
        for _ in 0..e.units_total() {
            let (_, u) = e.next_unit();
            assert_eq!(u.slice, 0);
            assert!(seen.insert((u.cand, u.ntime_off)));
        }
        assert_eq!(e.overlap.load(Ordering::Relaxed), 0);
        // all 16 BIP 320 bits are the miner's: a share rolling bits 13-16 is valid, the same share is invalid in a 16-slice epoch
        let full = 0x1fff_e000u32;
        let rolled = 0x2000_0000 | 0x0000_6000 | 0x0100_0000;
        assert_eq!(e.check_with_mask(&e.candidates[0], 0, 1000, rolled, 1, &[0xff; 32], full), ShareResult::Valid);
        let sliced = Epoch::with_slices(2, "00".repeat(32), 1, vec![c(1)], 16);
        assert!(matches!(sliced.check_with_mask(&sliced.candidates[0], 0, 1000, rolled, 1, &[0xff; 32], MINER_VERSION_MASK), ShareResult::Invalid(_)));
        // bits outside the negotiated mask are refused
        assert!(matches!(e.check_with_mask(&e.candidates[0], 0, 1000, 0x2000_0000 | 1, 2, &[0xff; 32], full), ShareResult::Invalid(_)));
    }

    #[test]
    fn units_are_unique_until_the_space_is_exhausted() {
        let e = dummy(3, 0);
        let total = e.units_total();
        let mut seen = HashSet::new();
        for _ in 0..total {
            let (_, u) = e.next_unit();
            assert!(seen.insert((u.cand, u.slice, u.ntime_off)));
        }
        assert_eq!(e.overlap.load(Ordering::Relaxed), 0);
        e.next_unit();
        assert_eq!(e.overlap.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn share_checks() {
        let e = dummy(2, 0);
        let sub = |slice: u8, version: u32, ntime: u32, nonce: u32| Submission { cand: 0, slice, ntime, version, nonce };
        let v = 0x2000_0000 | (3 << POOL_VERSION_SHIFT) | 0x0020_0000;
        let easy = [0xff; 32];
        assert_eq!(e.check_share(&sub(3, v, 1000, 7), &easy), ShareResult::Valid);
        assert_eq!(e.check_share(&sub(3, v, 1000, 7), &easy), ShareResult::Duplicate);
        assert!(matches!(e.check_share(&sub(4, v, 1000, 8), &easy), ShareResult::Invalid(_)));
        assert!(matches!(e.check_share(&sub(3, v, 800, 9), &easy), ShareResult::Invalid(_)));
        assert!(matches!(e.check_share(&sub(3, v | 1, 1000, 10), &easy), ShareResult::Invalid(_)));
        assert_eq!(e.check_share(&sub(3, v, 1000, 11), &[0u8; 32]), ShareResult::LowDifficulty);
    }

    #[test]
    fn a_hash_meeting_the_network_target_is_a_block_even_below_the_share_target() {
        let e = dummy(1, 0xff);
        let v = 0x2000_0000 | (1 << POOL_VERSION_SHIFT);
        let sub = Submission { cand: 0, slice: 1, ntime: 1000, version: v, nonce: 1 };
        assert_eq!(e.check_share(&sub, &[0u8; 32]), ShareResult::Block);
    }
}
