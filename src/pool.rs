use crate::addr::script_for_address;
use crate::config::Config;
use crate::rpc::NodeRpc;
use crate::template::{header_hash, meets_target, Candidate, Gbt};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, watch};

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
}

impl Epoch {
    pub fn new(id: u64, tip_hex: String, height: u32, candidates: Vec<Candidate>) -> Self {
        Self { id, tip_hex, height, candidates, started_unix: unix_now(), next_unit: AtomicU64::new(0), overlap: AtomicU64::new(0), seen: Mutex::new(HashSet::new()) }
    }

    pub fn units_total(&self) -> u64 {
        self.candidates.len() as u64 * SLICES * NTIME_SLOTS
    }

    pub fn units_used(&self) -> u64 {
        self.next_unit.load(Ordering::Relaxed).min(self.units_total())
    }

    /// Hands out each (candidate, slice, ntime offset) once per epoch; wrapping is counted as overlap.
    pub fn next_unit(&self) -> Unit {
        let n = self.next_unit.fetch_add(1, Ordering::Relaxed);
        let total = self.units_total();
        if n >= total {
            self.overlap.fetch_add(1, Ordering::Relaxed);
        }
        let c = self.candidates.len() as u64;
        let u = n % total;
        Unit { cand: (u % c) as usize, slice: ((u / c) % SLICES) as u8, ntime_off: (u / (c * SLICES)) as u32 }
    }

    pub fn check_share(&self, s: &Submission, share_target: &[u8; 32]) -> ShareResult {
        let Some(c) = self.candidates.get(s.cand) else { return ShareResult::Invalid("unknown candidate") };
        if s.slice as u64 >= SLICES {
            return ShareResult::Invalid("bad slice");
        }
        let expected = c.version | ((s.slice as u32) << POOL_VERSION_SHIFT);
        if (s.version & !MINER_VERSION_MASK) != expected {
            return ShareResult::Invalid("version bits outside the miner mask");
        }
        if s.ntime < c.mintime || s.ntime > c.curtime.saturating_add(NTIME_SLOTS as u32 + 60) {
            return ShareResult::Invalid("ntime out of range");
        }
        let hash = header_hash(&c.header(s.version, s.ntime, s.nonce));
        if !meets_target(&hash, share_target) {
            return ShareResult::LowDifficulty;
        }
        let key = u128::from_le_bytes(hash[..16].try_into().unwrap()) ^ ((s.cand as u128) << 100);
        if !self.seen.lock().unwrap().insert(key) {
            return ShareResult::Duplicate;
        }
        if meets_target(&hash, &c.target) {
            ShareResult::Block
        } else {
            ShareResult::Valid
        }
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
}

impl Pool {
    pub async fn connect(cfg: Config) -> Result<Arc<Pool>> {
        let net = cfg.network()?;
        let node = NodeRpc::new(&cfg.node.rpc_url, &cfg.node.auth)?;
        let (chain, height) = node.chain_info().await.context("cannot reach the node")?;
        if chain != net.chain {
            bail!("node is on chain {chain}, config says {}", cfg.pool.network);
        }
        if height < net.min_height {
            bail!("node height {height} is below the attestation fix height {}", net.min_height);
        }
        let payouts = cfg
            .pool
            .payout_addresses
            .iter()
            .map(|a| Ok(Payout { address: a.clone(), script: script_for_address(a, net.hrp)? }))
            .collect::<Result<Vec<_>>>()?;
        let (events, _) = broadcast::channel(1024);
        let (epoch, _) = watch::channel(None);
        let pool = Arc::new(Pool { cfg, node, payouts, epoch, stats: Stats::default(), events, started: Instant::now(), epoch_seq: AtomicU64::new(0) });
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

    /// Fetches one candidate per payout address for the current tip and validates `check` of them against the node.
    pub async fn build_epoch(&self, check: usize) -> Result<Epoch> {
        let t0 = Instant::now();
        let best = self.node.best_block_hash().await?;
        let mut candidates = Vec::with_capacity(self.payouts.len());
        let mut height = 0;
        for p in &self.payouts {
            let g: Gbt = serde_json::from_value(self.node.template(&p.address).await?)?;
            if g.previousblockhash != best {
                bail!("tip moved while building candidates");
            }
            height = g.height;
            candidates.push(Candidate::from_gbt(g, &p.script)?);
        }
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
                    self.epoch.send_replace(Some(Arc::new(e)));
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

    fn dummy(n: usize) -> Epoch {
        let c = |i: u8| Candidate {
            prev: [i; 32], version: 0x2000_0000, bits: 0x207f_ffff, target: [0xff; 32], curtime: 1000, mintime: 900,
            coinbase_wit: vec![], merkle_root: [i; 32], tx_datas: vec![],
        };
        Epoch::new(1, "00".repeat(32), 1, (0..n as u8).map(c).collect())
    }

    #[test]
    fn units_are_unique_until_the_space_is_exhausted() {
        let e = dummy(3);
        let total = e.units_total();
        let mut seen = HashSet::new();
        for _ in 0..total {
            let u = e.next_unit();
            assert!(seen.insert((u.cand, u.slice, u.ntime_off)));
        }
        assert_eq!(e.overlap.load(Ordering::Relaxed), 0);
        e.next_unit();
        assert_eq!(e.overlap.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn share_checks() {
        let e = dummy(2);
        let ok = |slice: u8, version: u32, ntime: u32, nonce: u32| Submission { cand: 0, slice, ntime, version, nonce };
        let v = 0x2000_0000 | (3 << POOL_VERSION_SHIFT) | 0x0020_0000;
        let easy = [0xff; 32];
        assert_eq!(e.check_share(&ok(3, v, 1000, 7), &easy), ShareResult::Block);
        assert_eq!(e.check_share(&ok(3, v, 1000, 7), &easy), ShareResult::Duplicate);
        assert!(matches!(e.check_share(&ok(4, v, 1000, 8), &easy), ShareResult::Invalid(_)));
        assert!(matches!(e.check_share(&ok(3, v, 800, 9), &easy), ShareResult::Invalid(_)));
        assert!(matches!(e.check_share(&ok(3, v | 1, 1000, 10), &easy), ShareResult::Invalid(_)));
        assert_eq!(e.check_share(&ok(3, v, 1000, 11), &[0u8; 32]), ShareResult::LowDifficulty);
    }
}
