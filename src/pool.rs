use crate::addr::script_for_address;
use crate::config::{Config, Mode};
use crate::ledger::Ledger;
use crate::rpc::NodeRpc;
use crate::template::{header_hash, meets_target, BatchTemplate, Candidate, Gbt, TxSet};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
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
}

impl SessionStats {
    pub fn new() -> Self {
        let now = AtomicU64::new(0);
        set_f64(&now, unix_now());
        Self {
            name: Mutex::new(String::new()), ok: AtomicU64::new(0), stale: AtomicU64::new(0), dup: AtomicU64::new(0), inv: AtomicU64::new(0),
            hashrate: AtomicU64::new(0), difficulty: AtomicU64::new(0), last_seen: now, unit: AtomicU64::new(0), mode: AtomicU64::new(1),
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
        Self { id, tip_hex, height, candidates, started_unix: unix_now(), next_unit: AtomicU64::new(0), overlap: AtomicU64::new(0), seen: Mutex::new(HashSet::new()), solo: RwLock::new(HashMap::new()), solo_build: tokio::sync::Mutex::new(()) }
    }

    pub fn units_total(&self) -> u64 {
        self.candidates.len() as u64 * SLICES * NTIME_SLOTS
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
        Unit { cand: (u % c) as usize, slice: ((u / c) % SLICES) as u8, ntime_off: (u / (c * SLICES)) as u32 }
    }

    pub fn tip_matches(&self, other: &Epoch) -> bool {
        self.tip_hex == other.tip_hex
    }

    pub fn check_share(&self, s: &Submission, share_target: &[u8; 32]) -> ShareResult {
        let Some(c) = self.candidates.get(s.cand) else { return ShareResult::Invalid("unknown candidate") };
        self.check(c, s.slice, s.ntime, s.version, s.nonce, share_target)
    }

    pub fn check(&self, c: &Candidate, slice: u8, ntime: u32, version: u32, nonce: u32, share_target: &[u8; 32]) -> ShareResult {
        if slice as u64 >= SLICES {
            return ShareResult::Invalid("bad slice");
        }
        let expected = c.version | ((slice as u32) << POOL_VERSION_SHIFT);
        if (version & !MINER_VERSION_MASK) != expected {
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
        let ledger = Ledger::open(&cfg.ledger.path).with_context(|| format!("opening ledger {}", cfg.ledger.path))?;
        let (events, _) = broadcast::channel(1024);
        let (epoch, _) = watch::channel(None);
        let pool = Arc::new(Pool {
            cfg, node, payouts, epoch, stats: Stats::default(), events, started: Instant::now(), epoch_seq: AtomicU64::new(0),
            sessions: Mutex::new(HashMap::new()), session_seq: AtomicU64::new(0), live: Mutex::new(Live::default()), solo_active: Mutex::new(HashMap::new()),
            pps_ready: AtomicBool::new(false), ledger, agg: Mutex::new(HashMap::new()), wallet, treasury_free: AtomicI64::new(0),
            batch_state: std::sync::atomic::AtomicU8::new(0), backend_name: Mutex::new("gbt"), photon: std::sync::OnceLock::new(),
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
        let mut candidates: Vec<Candidate> = Vec::with_capacity(self.payouts.len());
        let mut height = 0;
        for p in &self.payouts {
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
        let mut candidates: Vec<Candidate> = Vec::with_capacity(self.payouts.len());
        let mut height = 0;
        let mut shared: Option<std::sync::Arc<TxSet>> = None;
        for chunk in self.payouts.chunks(CHUNK) {
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

    pub async fn build_epoch(&self, check: usize) -> Result<Epoch> {
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
            if !tip_changed && !(refresh.as_secs() > 0 && last_build.elapsed() >= refresh) {
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
                    self.event("info", format!("epoch {} height {} tip {}.. {} candidates ready in {:.0} ms", e.id, e.height, &e.tip_hex[..12], e.candidates.len(), ms));
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

#[cfg(test)]
mod tests {
    use super::*;

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
