//! Pool side of the Photon protocol: listener, plan publication, channels, announces, shares and attestation spot checks.
use super::coinbase::to_witness;
use super::merkle::{self, AccountingTree};
use super::msg::*;
use super::noise::{self, Keypair, NoiseConn};
use super::plan::{derive_full, Rules};
use super::trustbook::{Entry, TrustBook, MAX_AGE_SECS};
use super::verify::{check_share, Announced, Seen, Trust, Verdict, MAX_NTIME_DELTA, VERSION_MASK};
use super::PROTOCOL_VERSION;
use crate::accounting::minute_now;
use crate::config::PhotonCfg;
use crate::diff::{difficulty_from_target, target_from_difficulty};
use crate::ledger;
use crate::pool::{Epoch, Pool};
use crate::tx::put_varint;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};

/// Credits of the plan window are expressed in this many units (the sum over all accounts).
const CREDIT_SCALE: i64 = 1_000_000_000_000;
const MAX_TEMPLATES: usize = 16;
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_STRIKES: u32 = 5;
const STALE_GRACE: Duration = Duration::from_secs(10);
const TIP_WAIT: Duration = Duration::from_millis(1500);

pub mod code {
    pub const NOT_READY: u16 = 1;
    pub const BAD_SCRIPT: u16 = 2;
    pub const TOO_MANY_CHANNELS: u16 = 3;
    pub const STALE: u16 = 4;
    pub const NO_PLAN: u16 = 5;
    pub const BAD_ANNOUNCE: u16 = 6;
    pub const UNKNOWN_CHANNEL: u16 = 7;
    pub const NOT_FOUND: u16 = 8;
    pub const CHECK_FAILED: u16 = 20;
    pub const BANNED: u16 = 21;
    pub const PROTOCOL: u16 = 22;
    pub const VERSION: u16 = 23;
}

pub struct PlanEntry {
    pub plan: PayoutPlan,
    pub tree: Arc<AccountingTree>,
}

/// Everything a session needs about the current tip: plans for the block being mined and the next one.
pub struct PlanBook {
    pub epoch_id: u64,
    pub tip: [u8; 32],
    pub tip_hex: String,
    pub tip_height: u32,
    pub plans: BTreeMap<u32, Arc<PlanEntry>>,
    pub max_reward: u64,
    /// Compact target of the pool's own template for this tip; announces must carry the same value.
    pub bits: u32,
}

impl PlanBook {
    pub fn height(&self) -> u32 {
        self.tip_height + 1
    }
}

#[derive(Default)]
pub struct PhotonStats {
    pub connections: AtomicU64,
    pub channels: AtomicU64,
    pub accepted: AtomicU64,
    pub rejected: AtomicU64,
    pub announces: AtomicU64,
    pub checks_ok: AtomicU64,
    pub checks_failed: AtomicU64,
    pub checks_inconclusive: AtomicU64,
    pub blocks: AtomicU64,
    /// Spot check timing: request to verdict (transfer plus node validation), node validation alone, bytes received.
    pub check_total_us: AtomicU64,
    pub check_max_us: AtomicU64,
    pub check_node_us: AtomicU64,
    pub check_bytes: AtomicU64,
    pub check_timed: AtomicU64,
    /// Checks that were due but skipped because the per-tip budget was used up.
    pub checks_skipped: AtomicU64,
}

pub struct PhotonService {
    pub pool: Arc<Pool>,
    pub cfg: PhotonCfg,
    key: Keypair,
    pub book: watch::Sender<Arc<PlanBook>>,
    pub stats: PhotonStats,
    rules: Rules,
    /// Share hashes of the current and the previous tip (old-tip shares are still accepted for a short grace period).
    seen: Mutex<(HashSet<[u8; 32]>, HashSet<[u8; 32]>)>,
    recent: Mutex<VecDeque<Arc<PlanEntry>>>,
    check_used: std::sync::atomic::AtomicU32,
    check_slots: Semaphore,
    /// Remembered trust per network address and per payout script.
    pub trust: Mutex<TrustBook>,
    /// Sum of the spot check probabilities of all connected gateways, in millionths.
    total_p_micro: AtomicI64,
    ips: Mutex<HashMap<std::net::IpAddr, usize>>,
}

fn rand64() -> u64 {
    std::collections::hash_map::RandomState::new().build_hasher().finish()
}

/// Loads the Noise key (private and public hex on two lines) or creates it with owner-only permissions.
pub fn load_or_create_key(path: &str) -> Result<Keypair> {
    if let Ok(s) = std::fs::read_to_string(path) {
        let mut l = s.lines();
        let private = hex::decode(l.next().unwrap_or("").trim()).context("photon key file: private key")?;
        let public: [u8; 32] = hex::decode(l.next().unwrap_or("").trim()).context("photon key file: public key")?.try_into().map_err(|_| anyhow!("photon key file: public key must be 32 bytes"))?;
        if private.len() != 32 {
            bail!("photon key file: private key must be 32 bytes");
        }
        return Ok(Keypair { private, public });
    }
    let kp = noise::generate_keypair()?;
    let body = format!("{}\n{}\n", hex::encode(&kp.private), hex::encode(kp.public));
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).with_context(|| format!("creating {path}"))?;
        f.write_all(body.as_bytes())?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, body)?;
    Ok(kp)
}

/// Trust key of a network address: IPv4 as is, IPv6 by its /64 prefix (a host controls its whole /64).
pub fn ip_key(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V4(v) => format!("ip:{v}"),
        std::net::IpAddr::V6(v) => {
            let s = v.segments();
            format!("ip:{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    }
}

pub fn valid_payout_script(s: &[u8]) -> bool {
    // witness v0 keyhash (22 bytes), v0 scripthash and v1 taproot (34 bytes)
    (s.len() == 22 && s[0] == 0x00 && s[1] == 0x14) || (s.len() == 34 && ((s[0] == 0x00 && s[1] == 0x20) || (s[0] == 0x51 && s[1] == 0x20)))
}

/// The announced compact target must equal the pool's own view of the network target for this tip.
pub fn check_announce_terms(announced_bits: u32, network_bits: u32) -> std::result::Result<(), &'static str> {
    if network_bits == 0 || announced_bits != network_bits {
        return Err("bits differ from the network's difficulty");
    }
    Ok(())
}

/// Memory limits of one session: variants per template and bytes of announced coinbases kept.
pub const MAX_VARIANTS_PER_TEMPLATE: usize = 4096;
pub const MAX_TEMPLATE_BYTES: usize = 48 * 1024 * 1024;

fn template_bytes(chunks: &[Announced]) -> usize {
    chunks.iter().map(|a| a.ann.coinbases.iter().map(Vec::len).sum::<usize>() + a.txids.len() * 32).sum()
}

/// Divisor for all spot check probabilities: 1 while their sum fits into the per-tip budget, otherwise sum / budget.
pub fn scale_for(sum_p: f64, budget: u32) -> f64 {
    (sum_p / budget.max(1) as f64).max(1.0)
}

/// Next share difficulty so that a channel produces about one share per `goal_secs`.
pub fn retarget(diff: f64, accepted: u32, secs: f64, goal_secs: f64, min: f64, max: f64) -> Option<f64> {
    if secs < 30.0 {
        return None;
    }
    let ratio = (accepted.max(1) as f64 / secs) * goal_secs;
    if (0.67..=1.5).contains(&ratio) {
        return None;
    }
    let next = (diff * ratio.clamp(0.25, 4.0)).clamp(min, max);
    ((next / diff - 1.0).abs() > 0.05).then_some(next)
}

impl PhotonService {
    pub fn new(pool: Arc<Pool>) -> Result<Arc<Self>> {
        let cfg = pool.cfg.photon.clone();
        let key = load_or_create_key(&cfg.key_file)?;
        let net = pool.cfg.network()?;
        let fee_script = if cfg.fee_address.is_empty() {
            pool.payouts.first().ok_or_else(|| anyhow!("photon needs a fee address or at least one pool payout address"))?.script.clone()
        } else {
            crate::addr::script_for_address(&cfg.fee_address, net.hrp)?
        };
        let cfg_conc = cfg.max_concurrent_checks.max(1);
        let rules = Rules { n_max: cfg.max_outputs, fee_ppb: (cfg.fee_pct * 10_000_000.0).round() as u32, fee_script };
        rules.validate()?;
        let empty = Arc::new(PlanBook { epoch_id: 0, tip: [0; 32], tip_hex: String::new(), tip_height: 0, plans: BTreeMap::new(), max_reward: 0, bits: 0 });
        let (book, _) = watch::channel(empty);
        Ok(Arc::new(Self { pool, cfg, key, book, stats: PhotonStats::default(), rules, seen: Mutex::new((HashSet::new(), HashSet::new())), recent: Mutex::new(VecDeque::new()), check_used: std::sync::atomic::AtomicU32::new(0), check_slots: Semaphore::new(cfg_conc), trust: Mutex::new(TrustBook::default()), total_p_micro: AtomicI64::new(0), ips: Mutex::new(HashMap::new()) }))
    }

    pub fn now() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }

    /// Factor by which individual probabilities are divided so that the expected number of checks per tip matches the budget.
    /// Selection is proportional to each gateway's own probability: less trusted gateways are checked more often, trusted ones never drop to zero.
    pub fn p_scale(&self) -> f64 {
        scale_for(self.sum_p(), self.cfg.max_checks_per_tip)
    }

    pub fn sum_p(&self) -> f64 {
        self.total_p_micro.load(Ordering::Relaxed).max(0) as f64 / 1e6
    }

    async fn load_trust(&self) {
        let since = Self::now() - MAX_AGE_SECS;
        match self.pool.ledger.call(move |c| ledger::trust_load(c, since)).await {
            Ok(rows) => {
                let n = rows.len();
                let rows = rows.into_iter().map(|(k, passes, fails, last_fail, banned_until, updated)| (k, Entry { trust: Trust { passes: passes as u32, fails: fails as u32 }, last_fail, banned_until, updated })).collect();
                self.trust.lock().unwrap().load(rows);
                self.pool.event("info", format!("photon: {n} remembered gateway trust entries loaded"));
            }
            Err(e) => self.pool.event("warn", format!("photon: could not load remembered trust: {e:#}")),
        }
    }

    async fn trust_flush_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tick.tick().await;
            self.flush_trust().await;
        }
    }

    /// Writes changed trust entries to the ledger and removes expired ones; also called on shutdown.
    pub async fn flush_trust(&self) {
        let (dirty, gone) = {
            let mut b = self.trust.lock().unwrap();
            let gone = b.prune(Self::now());
            (b.take_dirty(), gone)
        };
        if dirty.is_empty() && gone.is_empty() {
            return;
        }
        let rows: Vec<ledger::TrustRow> = dirty.into_iter().map(|(k, e)| (k, e.trust.passes as i64, e.trust.fails as i64, e.last_fail, e.banned_until, e.updated)).collect();
        if let Err(e) = self.pool.ledger.call(move |c| ledger::trust_save(c, &rows, &gone)).await {
            self.pool.event("warn", format!("photon: could not save gateway trust: {e:#}"));
        }
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.key.public)
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let listener = TcpListener::bind(&self.cfg.listen).await.with_context(|| format!("binding photon listener {}", self.cfg.listen))?;
        self.pool.event("info", format!("photon protocol listening on {} (pool key {})", self.cfg.listen, self.public_key_hex()));
        self.load_trust().await;
        tokio::spawn(self.clone().trust_flush_loop());
        tokio::spawn(self.clone().plan_loop());
        let slots = Arc::new(Semaphore::new(self.cfg.max_connections));
        loop {
            let (tcp, peer) = listener.accept().await?;
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                continue;
            };
            let key = match peer.ip() {
                std::net::IpAddr::V6(v) => { let s = v.segments(); std::net::IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0)) }
                v4 => v4,
            };
            {
                let mut ips = self.ips.lock().unwrap();
                let n = ips.entry(key).or_insert(0);
                if *n >= self.cfg.max_connections_per_ip {
                    continue;
                }
                *n += 1;
            }
            let svc = self.clone();
            tokio::spawn(async move {
                svc.stats.connections.fetch_add(1, Ordering::Relaxed);
                if let Err(e) = svc.clone().session(tcp, peer).await {
                    tracing::debug!("photon session {peer}: {e:#}");
                }
                svc.stats.connections.fetch_sub(1, Ordering::Relaxed);
                {
                    let mut ips = svc.ips.lock().unwrap();
                    if let Some(n) = ips.get_mut(&key) {
                        *n = n.saturating_sub(1);
                        if *n == 0 { ips.remove(&key); }
                    }
                }
                drop(permit);
            });
        }
    }

    async fn plan_loop(self: Arc<Self>) {
        let mut rx = self.pool.epoch.subscribe();
        let mut last: Option<Arc<PlanBook>> = None;
        loop {
            let epoch = rx.borrow_and_update().clone();
            if let Some(e) = epoch {
                if last.as_ref().map_or(true, |b| b.tip_hex != e.tip_hex) {
                    match self.make_book(&e, last.as_ref()).await {
                        Ok(b) => {
                            let b = Arc::new(b);
                            last = Some(b.clone());
                            self.check_used.store(0, Ordering::Relaxed);
                            {
                                let mut g = self.seen.lock().unwrap();
                                let cur = std::mem::take(&mut g.0);
                                g.1 = cur;
                            }
                            self.pool.event("info", format!("photon plans for heights {:?} ready (tip {}..)", b.plans.keys().collect::<Vec<_>>(), &b.tip_hex[..12]));
                            self.book.send_replace(b);
                        }
                        Err(err) => self.pool.event("warn", format!("photon plan failed: {err:#}")),
                    }
                }
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    async fn make_book(&self, e: &Arc<Epoch>, old: Option<&Arc<PlanBook>>) -> Result<PlanBook> {
        let cand = e.candidates.first().ok_or_else(|| anyhow!("epoch without candidates"))?;
        let mut tip: [u8; 32] = hex::decode(&e.tip_hex)?.try_into().map_err(|_| anyhow!("bad tip"))?;
        tip.reverse();
        let tip_height = e.height - 1;
        let mut plans: BTreeMap<u32, Arc<PlanEntry>> = BTreeMap::new();
        // the plan for the block being mined was published one block ago, if this tip extends the previous one
        if let Some(o) = old {
            if o.tip_height + 1 == tip_height {
                let parent = self.pool.node.call("getblockheader", json!([e.tip_hex])).await?;
                if parent["previousblockhash"].as_str() == Some(o.tip_hex.as_str()) {
                    if let Some(p) = o.plans.get(&e.height) {
                        plans.insert(e.height, p.clone());
                    }
                }
            }
        }
        self.pool.flush_shares().await?;
        let window = self.cfg.window_factor * difficulty_from_target(&cand.target);
        let rows = self.pool.ledger.call(move |c| ledger::window_split(c, ledger::MODE_PHOTON, window, CREDIT_SCALE)).await?;
        let credits: Vec<(Vec<u8>, u64)> = rows.into_iter().filter_map(|(a, v)| Some((hex::decode(a).ok()?, v as u64))).collect();
        for height in [e.height, e.height + 1] {
            if height == e.height + 1 || !plans.contains_key(&height) {
                let (plan, tree) = derive_full(&self.rules, tip, height, &credits)?;
                plans.insert(height, Arc::new(PlanEntry { plan, tree: Arc::new(tree) }));
            }
        }
        {
            let mut recent = self.recent.lock().unwrap();
            for p in plans.values() {
                if !recent.iter().any(|r| r.plan.plan_hash == p.plan.plan_hash) {
                    recent.push_back(p.clone());
                }
            }
            while recent.len() > 8 {
                recent.pop_front();
            }
        }
        let subsidy = cand.subsidy().max(0) as f64;
        let max_reward = ((subsidy * (1.0 + self.cfg.max_reward_over_subsidy_pct / 100.0)) as u64).max(cand.coinbase_value.max(0) as u64);
        Ok(PlanBook { epoch_id: e.id, tip, tip_hex: e.tip_hex.clone(), tip_height, plans, max_reward, bits: cand.bits })
    }

    /// Takes one unit of the per-tip spot check budget.
    fn reserve_check(&self) -> bool {
        let ok = self.check_used.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| (u < self.cfg.max_checks_per_tip).then_some(u + 1)).is_ok();
        if !ok {
            self.stats.checks_skipped.fetch_add(1, Ordering::Relaxed);
        }
        ok
    }

    fn credit(&self, account: &str, diff: f64) {
        let mut m = self.pool.agg.lock().unwrap();
        let e = m.entry((minute_now(), account.to_string(), ledger::MODE_PHOTON as u8)).or_default();
        e.0 += diff;
        e.1 += 1;
    }

    async fn session(self: Arc<Self>, tcp: TcpStream, peer: SocketAddr) -> Result<()> {
        tcp.set_nodelay(true).ok();
        let conn = tokio::time::timeout(Duration::from_secs(10), noise::accept(tcp, &self.key)).await.map_err(|_| anyhow!("handshake timeout"))??;
        let mut s = Session::new(self.clone(), conn, peer);
        let r = s.run().await;
        s.set_contrib(0);
        r
    }
}

struct Chan {
    script: Vec<u8>,
    account: String,
    target: [u8; 32],
    diff: f64,
    accepted: u32,
    window_start: Instant,
}

struct PendingCheck {
    template_id: u32,
    variant: u32,
    started: Instant,
    buf: Vec<u8>,
    next_seq: u32,
}

struct Session {
    svc: Arc<PhotonService>,
    conn: NoiseConn<TcpStream>,
    peer: SocketAddr,
    book: Arc<PlanBook>,
    book_rx: watch::Receiver<Arc<PlanBook>>,
    stale_since: HashMap<u32, Instant>,
    sent: HashSet<u32>,
    channels: HashMap<u32, Chan>,
    next_chan: u32,
    templates: HashMap<u32, Vec<Announced>>,
    order: VecDeque<u32>,
    keys: Vec<String>,
    contrib: i64,
    tpl_bytes: usize,
    proofs: u32,
    seen: Seen,
    check: Option<PendingCheck>,
    held: HashMap<(u32, u32), Vec<(String, f64)>>,
    strikes: u32,
    last_rx: Instant,
}

fn find_in(t: &HashMap<u32, Vec<Announced>>, template_id: u32, variant: u32) -> Option<&Announced> {
    t.get(&template_id)?.iter().find(|a| variant >= a.ann.first_variant && ((variant - a.ann.first_variant) as usize) < a.ann.coinbases.len())
}

fn reject(code: u16, reason: &str) -> Msg {
    Msg::Reject(Reject { code, reason: reason.into() })
}

impl Session {
    fn new(svc: Arc<PhotonService>, conn: NoiseConn<TcpStream>, peer: SocketAddr) -> Self {
        let mut book_rx = svc.book.subscribe();
        let book = book_rx.borrow_and_update().clone();
        Self {
            svc, conn, peer, book, book_rx, stale_since: HashMap::new(), sent: HashSet::new(), channels: HashMap::new(), next_chan: 1, templates: HashMap::new(), order: VecDeque::new(), keys: vec![ip_key(peer.ip())], contrib: 0, tpl_bytes: 0, proofs: 0, seen: Seen::default(), check: None,
            held: HashMap::new(), strikes: 0, last_rx: Instant::now(),
        }
    }

    async fn send(&mut self, m: Msg) -> Result<()> {
        self.conn.send(&m.encode()).await
    }

    async fn disconnect(&mut self, c: u16, reason: &str) -> Result<()> {
        let _ = self.send(Msg::Disconnect(Reject { code: c, reason: reason.into() })).await;
        bail!("disconnected: {reason}")
    }

    fn set_contrib(&mut self, micro: i64) {
        self.svc.total_p_micro.fetch_add(micro - self.contrib, Ordering::Relaxed);
        self.contrib = micro;
    }

    fn refresh_contrib(&mut self) {
        let p = self.svc.trust.lock().unwrap().probability(&self.keys, PhotonService::now());
        self.set_contrib((p * 1e6) as i64);
    }

    fn banned(&self) -> bool {
        self.svc.trust.lock().unwrap().banned(&self.keys, PhotonService::now())
    }

    async fn run(&mut self) -> Result<()> {
        if self.banned() {
            return self.disconnect(code::BANNED, "banned after repeated attestation check failures").await;
        }
        self.refresh_contrib();
        let first = tokio::time::timeout(Duration::from_secs(10), self.conn.recv()).await.map_err(|_| anyhow!("no Setup"))??;
        match Msg::decode(&first)? {
            Msg::Setup(s) if s.version == PROTOCOL_VERSION => self.send(Msg::SetupAck(Setup { version: PROTOCOL_VERSION, features: 0, max_frame: super::wire::MAX_FRAME as u32 })).await?,
            Msg::Setup(_) => return self.disconnect(code::VERSION, "unsupported protocol version").await,
            _ => return self.disconnect(code::PROTOCOL, "Setup expected").await,
        }
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        self.push_book().await?;
        loop {
            tokio::select! {
                f = self.conn.recv() => {
                    let f = f?;
                    self.last_rx = Instant::now();
                    let m = Msg::decode(&f)?;
                    self.handle(m).await?;
                }
                r = self.book_rx.changed() => {
                    r?;
                    self.book = self.book_rx.borrow_and_update().clone();
                    self.on_new_tip().await?;
                }
                _ = tick.tick() => self.on_tick().await?,
            }
            if self.strikes >= MAX_STRIKES {
                return self.disconnect(code::BANNED, "too many invalid messages").await;
            }
        }
    }

    async fn push_book(&mut self) -> Result<()> {
        if self.book.epoch_id == 0 {
            return Ok(());
        }
        let (tip, height) = (self.book.tip, self.book.height());
        self.send(Msg::NewTip(NewTip { tip, height: self.book.tip_height })).await?;
        let plans: Vec<_> = self.book.plans.values().cloned().collect();
        for p in plans {
            if self.sent.insert(p.plan.height) {
                self.send(Msg::PayoutPlan(p.plan.clone())).await?;
                self.send(Msg::EpochCommit(EpochCommit { height: p.plan.height, root: p.plan.accounting_root, leaves: p.tree.len() as u32 })).await?;
            }
        }
        let _ = height;
        Ok(())
    }

    async fn on_new_tip(&mut self) -> Result<()> {
        // shares for templates of the previous tip stay valid for a grace period: a block found by this very gateway changes the tip before its final share arrives
        let now = Instant::now();
        for id in self.templates.keys() {
            self.stale_since.entry(*id).or_insert(now);
        }
        self.proofs = 0;
        self.sent.retain(|h| self.book.plans.contains_key(h));
        if let Some(c) = self.check.take() {
            self.svc.stats.checks_inconclusive.fetch_add(1, Ordering::Relaxed);
            self.release((c.template_id, c.variant));
        }
        self.push_book().await
    }

    async fn on_tick(&mut self) -> Result<()> {
        if self.last_rx.elapsed() > Duration::from_secs(self.svc.cfg.idle_secs) {
            return self.disconnect(code::PROTOCOL, "idle").await;
        }
        if let Some(c) = &self.check {
            if c.started.elapsed() > CHECK_TIMEOUT {
                let key = (c.template_id, c.variant);
                self.check = None;
                self.fail_check(key, "transactions were not delivered in time").await?;
            }
        }
        let expired: Vec<u32> = self.stale_since.iter().filter(|(_, t)| t.elapsed() > STALE_GRACE).map(|(id, _)| *id).collect();
        let any_expired = !expired.is_empty();
        for id in expired {
            self.stale_since.remove(&id);
            self.drop_template(id);
            self.order.retain(|x| *x != id);
        }
        if any_expired && self.stale_since.is_empty() {
            // the duplicate filter of the old tip is no longer needed; the pool-wide filter still covers the current one
            self.seen.clear();
        }
        let (min, max, goal) = (self.svc.cfg.min_difficulty, self.svc.cfg.max_difficulty, self.svc.cfg.share_secs);
        let mut updates = vec![];
        for (id, ch) in self.channels.iter_mut() {
            let secs = ch.window_start.elapsed().as_secs_f64();
            if let Some(next) = retarget(ch.diff, ch.accepted, secs, goal, min, max) {
                ch.diff = next;
                ch.target = target_from_difficulty(next);
                updates.push((*id, ch.target));
            }
            if secs >= 30.0 {
                ch.accepted = 0;
                ch.window_start = Instant::now();
            }
        }
        for (channel, target) in updates {
            self.send(Msg::SetTarget(SetTarget { channel, target })).await?;
        }
        Ok(())
    }

    async fn handle(&mut self, m: Msg) -> Result<()> {
        match m {
            Msg::OpenChannel(m) => self.open_channel(m).await,
            Msg::TemplateAnnounce(a) => self.announce(a).await,
            Msg::Shares(s) => self.shares(s).await,
            Msg::Transactions(t) => self.transactions(t).await,
            Msg::BlockFound(b) => {
                self.svc.stats.blocks.fetch_add(1, Ordering::Relaxed);
                let svc = self.svc.clone();
                tokio::spawn(async move {
                    let mut disp = b.hash;
                    disp.reverse();
                    let known = svc.pool.node.call("getblockheader", json!([hex::encode(disp)])).await.is_ok();
                    svc.pool.event(if known { "info" } else { "warn" }, format!("photon block {} at height {} reported by a gateway ({})", hex::encode(disp), b.height, if known { "known to the node" } else { "not known to the node" }));
                });
                Ok(())
            }
            Msg::GetProof(g) => self.proof(g).await,
            Msg::Setup(_) | Msg::SetupAck(_) | Msg::Disconnect(_) | Msg::Reject(_) => Ok(()),
            _ => {
                self.strikes += 1;
                self.send(reject(code::PROTOCOL, "message not accepted by the pool")).await
            }
        }
    }

    async fn open_channel(&mut self, m: OpenChannel) -> Result<()> {
        if self.book.epoch_id == 0 {
            return self.send(reject(code::NOT_READY, "pool has no plan yet")).await;
        }
        if !valid_payout_script(&m.payout_script) {
            return self.send(reject(code::BAD_SCRIPT, "payout script must be a witness v0/v1 program")).await;
        }
        if self.channels.len() >= self.svc.cfg.max_channels_per_connection {
            return self.send(reject(code::TOO_MANY_CHANNELS, "channel limit reached")).await;
        }
        let want = difficulty_from_target(&m.target);
        let diff = if want.is_finite() && want > 0.0 { want } else { self.svc.cfg.initial_difficulty }.clamp(self.svc.cfg.min_difficulty, self.svc.cfg.max_difficulty);
        let key = format!("script:{}", hex::encode(&m.payout_script));
        if !self.keys.contains(&key) {
            self.keys.push(key);
            if self.banned() {
                return self.disconnect(code::BANNED, "payout script is banned after repeated attestation check failures").await;
            }
            self.refresh_contrib();
        }
        let id = self.next_chan;
        self.next_chan += 1;
        let target = target_from_difficulty(diff);
        self.channels.insert(id, Chan { account: hex::encode(&m.payout_script), script: m.payout_script, target, diff, accepted: 0, window_start: Instant::now() });
        self.svc.stats.channels.fetch_add(1, Ordering::Relaxed);
        let plan_height = self.book.height();
        self.send(Msg::OpenChannelAck(OpenChannelAck { request: m.request, channel: id, target, plan_height })).await
    }

    async fn announce(&mut self, a: TemplateAnnounce) -> Result<()> {
        if a.tip != self.book.tip {
            // the gateway's node can be a little ahead of the pool's: wait briefly for the pool's own tip
            let deadline = Instant::now() + TIP_WAIT;
            while a.tip != self.book.tip && self.book.height() < a.height {
                let left = deadline.saturating_duration_since(Instant::now());
                if !tokio::time::timeout(left, self.book_rx.changed()).await.map_or(false, |r| r.is_ok()) {
                    break;
                }
                self.book = self.book_rx.borrow_and_update().clone();
                self.on_new_tip().await?;
            }
            if a.tip != self.book.tip {
                return self.send(reject(code::STALE, "announce is not for the current tip")).await;
            }
        }
        let Some(entry) = self.book.plans.get(&a.height).cloned() else {
            return self.send(reject(code::NO_PLAN, "no plan for that height")).await;
        };
        let template_id = a.template_id;
        // the network target decides which hashes count as blocks and bypass the share target: it must be the node's, not the gateway's
        if let Err(why) = check_announce_terms(a.bits, self.book.bits) {
            self.strikes += 1;
            return self.send(reject(code::BAD_ANNOUNCE, why)).await;
        }
        let announced = match Announced::new(a, &self.book.tip, &entry.plan, self.book.max_reward) {
            Ok(x) => x,
            Err(e) => {
                self.strikes += 1;
                return self.send(reject(code::BAD_ANNOUNCE, &format!("{e:#}").chars().take(200).collect::<String>())).await;
            }
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        if (announced.ann.curtime as i64 - now).abs() > MAX_NTIME_DELTA as i64 {
            self.strikes += 1;
            return self.send(reject(code::BAD_ANNOUNCE, "curtime is too far from the pool's clock")).await;
        }
        let new_bytes = template_bytes(std::slice::from_ref(&announced));
        let have_variants: usize = self.templates.get(&template_id).map_or(0, |p| p.iter().map(|a| a.ann.coinbases.len()).sum());
        if have_variants + announced.ann.coinbases.len() > MAX_VARIANTS_PER_TEMPLATE || new_bytes > MAX_TEMPLATE_BYTES {
            self.strikes += 1;
            return self.send(reject(code::BAD_ANNOUNCE, "too many variants for one template")).await;
        }
        if let Some(prev) = self.templates.get(&template_id) {
            let f = &prev[0].ann;
            let (n, lo, hi) = (&announced.ann, announced.ann.first_variant as u64, announced.ann.first_variant as u64 + announced.ann.coinbases.len() as u64);
            let overlap = prev.iter().any(|p| (p.ann.first_variant as u64) < hi && lo < p.ann.first_variant as u64 + p.ann.coinbases.len() as u64);
            if f.version != n.version || f.bits != n.bits || f.curtime != n.curtime || f.branch != n.branch || f.height != n.height || overlap {
                self.strikes += 1;
                return self.send(reject(code::BAD_ANNOUNCE, "chunks of one template disagree or overlap")).await;
            }
        } else {
            while self.templates.len() >= MAX_TEMPLATES || (self.tpl_bytes + new_bytes > MAX_TEMPLATE_BYTES && !self.order.is_empty()) {
                let Some(old) = self.order.pop_front() else { break };
                self.drop_template(old);
            }
            self.order.push_back(template_id);
        }
        self.svc.stats.announces.fetch_add(1, Ordering::Relaxed);
        let (first, n) = (announced.ann.first_variant, announced.ann.coinbases.len() as u64);
        self.tpl_bytes += new_bytes;
        self.templates.entry(template_id).or_default().push(announced);
        let roll = (rand64() >> 11) as f64 / (1u64 << 53) as f64;
        let p = self.svc.trust.lock().unwrap().probability(&self.keys, PhotonService::now()) / self.svc.p_scale();
        if self.check.is_none() && roll < p && self.svc.reserve_check() {
            let variant = first + (rand64() % n) as u32;
            self.check = Some(PendingCheck { template_id, variant, started: Instant::now(), buf: Vec::new(), next_seq: 0 });
            self.held.entry((template_id, variant)).or_default();
            self.send(Msg::GetTransactions(GetTransactions { template_id, variant })).await?;
        }
        Ok(())
    }

    fn drop_template(&mut self, id: u32) {
        if let Some(chunks) = self.templates.remove(&id) {
            self.tpl_bytes = self.tpl_bytes.saturating_sub(template_bytes(&chunks));
        }
        self.stale_since.remove(&id);
    }

    fn find(&self, template_id: u32, variant: u32) -> Option<&Announced> {
        find_in(&self.templates, template_id, variant)
    }

    async fn shares(&mut self, m: Shares) -> Result<()> {
        let Some(ch) = self.channels.get(&m.channel) else {
            self.strikes += 1;
            return self.send(reject(code::UNKNOWN_CHANNEL, "unknown channel")).await;
        };
        let (target, account, diff) = (ch.target, ch.account.clone(), ch.diff);
        let mut rejects: Vec<(u32, u8)> = Vec::new();
        let mut accepted = 0u32;
        for (i, item) in m.items.iter().enumerate() {
            let Some(a) = find_in(&self.templates, m.template_id, item.variant) else {
                rejects.push((i as u32, if self.templates.contains_key(&m.template_id) { reject::UNKNOWN_VARIANT } else { reject::UNKNOWN_TEMPLATE }));
                continue;
            };
            match check_share(a, m.template_id, item, &target, &mut self.seen) {
                Verdict::Reject(c) => rejects.push((i as u32, c)),
                Verdict::Share { hash, block } => {
                    let fresh = { let mut g = self.svc.seen.lock().unwrap(); !g.1.contains(&hash) && g.0.insert(hash) };
                    if !fresh {
                        rejects.push((i as u32, reject::DUPLICATE));
                        continue;
                    }
                    accepted += 1;
                    match self.held.get_mut(&(m.template_id, item.variant)) {
                        Some(h) if h.len() < 100_000 => h.push((account.clone(), diff)),
                        _ => self.svc.credit(&account, diff),
                    }
                    if block {
                        self.svc.pool.event("info", format!("photon share meets the network target (height {}, channel {})", a.ann.height, m.channel));
                    }
                }
            }
        }
        if !rejects.is_empty() {
            tracing::debug!("photon {}: template {} shares {} rejected, first {:?} (book tip height {})", self.peer, m.template_id, rejects.len(), &rejects[..rejects.len().min(3)], self.book.tip_height);
        }
        self.svc.stats.accepted.fetch_add(accepted as u64, Ordering::Relaxed);
        self.svc.stats.rejected.fetch_add(rejects.len() as u64, Ordering::Relaxed);
        if let Some(c) = self.channels.get_mut(&m.channel) {
            c.accepted += accepted;
        }
        if accepted == 0 && m.items.len() >= 20 {
            self.strikes += 1;
        }
        self.send(Msg::ShareAck(ShareAck { channel: m.channel, template_id: m.template_id, accepted, rejects })).await
    }

    fn release(&mut self, key: (u32, u32)) {
        if let Some(h) = self.held.remove(&key) {
            for (account, diff) in h {
                self.svc.credit(&account, diff);
            }
        }
    }

    fn check_passed(&mut self, key: (u32, u32)) -> Result<()> {
        self.svc.trust.lock().unwrap().record(&self.keys, true, PhotonService::now());
        self.refresh_contrib();
        self.svc.stats.checks_ok.fetch_add(1, Ordering::Relaxed);
        self.release(key);
        Ok(())
    }

    async fn fail_check(&mut self, key: (u32, u32), why: &str) -> Result<()> {
        self.held.remove(&key);
        let banned = self.svc.trust.lock().unwrap().record(&self.keys, false, PhotonService::now());
        self.refresh_contrib();
        self.svc.stats.checks_failed.fetch_add(1, Ordering::Relaxed);
        self.svc.pool.event("warn", format!("photon spot check failed for {} (template {}, variant {}): {why}", self.peer, key.0, key.1));
        if banned {
            return self.disconnect(code::BANNED, "repeated attestation check failures").await;
        }
        self.send(reject(code::CHECK_FAILED, &why.chars().take(200).collect::<String>())).await
    }

    async fn transactions(&mut self, t: Transactions) -> Result<()> {
        let max = self.svc.cfg.max_check_bytes;
        let Some(c) = self.check.as_mut() else {
            return Ok(());
        };
        if t.template_id != c.template_id || t.variant != c.variant || t.seq != c.next_seq {
            return Ok(());
        }
        if c.buf.len() + t.data.len() > max {
            let key = (c.template_id, c.variant);
            self.check = None;
            return self.fail_check(key, "transaction data exceeds the limit").await;
        }
        c.buf.extend_from_slice(&t.data);
        c.next_seq += 1;
        if !t.last {
            return Ok(());
        }
        let c = self.check.take().unwrap();
        let key = (c.template_id, c.variant);
        let assembled = self.assemble_check(&c);
        // the node is only asked once a slot is free; waiting for the slot is not counted as validation time
        let block_hex = match assembled {
            Ok(b) => b,
            Err(e) => return self.fail_check(key, &format!("{e:#}")).await,
        };
        let svc = self.svc.clone();
        let slot = svc.check_slots.acquire().await?;
        let node_start = Instant::now();
        let outcome: Result<Option<String>> = match tokio::time::timeout(Duration::from_secs(60), svc.pool.node.proposal(&block_hex)).await {
            Ok(r) => r,
            Err(_) => Err(anyhow!("node did not answer in time")),
        };
        drop(slot);
        let (node_us, total_us) = (node_start.elapsed().as_micros() as u64, c.started.elapsed().as_micros() as u64);
        let st = &self.svc.stats;
        st.check_node_us.fetch_add(node_us, Ordering::Relaxed);
        st.check_total_us.fetch_add(total_us, Ordering::Relaxed);
        st.check_max_us.fetch_max(total_us, Ordering::Relaxed);
        st.check_bytes.fetch_add(c.buf.len() as u64, Ordering::Relaxed);
        st.check_timed.fetch_add(1, Ordering::Relaxed);
        match outcome {
            // "duplicate": the node already has exactly this block, which is the strongest possible pass
            Ok(None) => self.check_passed(key),
            Ok(Some(reason)) if reason == "duplicate" => self.check_passed(key),
            Ok(Some(reason)) if reason.contains("inconclusive") => {
                self.svc.pool.event("info", format!("photon spot check inconclusive for {}: {reason}", self.peer));
                self.svc.stats.checks_inconclusive.fetch_add(1, Ordering::Relaxed);
                self.release(key);
                Ok(())
            }
            Ok(Some(reason)) => self.fail_check(key, &reason).await,
            // a node that cannot answer says nothing about the gateway
            Err(e) => {
                self.svc.pool.event("warn", format!("photon spot check could not be validated by the node: {e:#}"));
                self.svc.stats.checks_inconclusive.fetch_add(1, Ordering::Relaxed);
                self.release(key);
                Ok(())
            }
        }
    }

    /// Assembles the announced variant with the delivered transactions and lets the node validate it, attestation included.
    fn assemble_check(&self, c: &PendingCheck) -> Result<String> {
        let a = self.find(c.template_id, c.variant).ok_or_else(|| anyhow!("template no longer known"))?;
        let mut txs = Vec::new();
        let (mut p, mut n) = (0usize, 0u64);
        while p < c.buf.len() {
            if p + 4 > c.buf.len() {
                bail!("transaction stream truncated");
            }
            let len = u32::from_le_bytes(c.buf[p..p + 4].try_into().unwrap()) as usize;
            p += 4;
            if len == 0 || p + len > c.buf.len() {
                bail!("transaction record out of range");
            }
            txs.extend_from_slice(&c.buf[p..p + len]);
            p += len;
            n += 1;
        }
        let item = ShareItem { variant: c.variant, nonce: 0, ntime_delta: 0, version_bits: a.ann.version & VERSION_MASK };
        let header = a.header(&item)?;
        let cb = &a.ann.coinbases[(c.variant - a.ann.first_variant) as usize];
        let mut block = Vec::with_capacity(80 + cb.len() + txs.len() + 80);
        block.extend_from_slice(&header);
        put_varint(&mut block, 1 + n);
        block.extend_from_slice(&to_witness(cb));
        block.extend_from_slice(&txs);
        Ok(hex::encode(block))
    }

    async fn proof(&mut self, g: GetProof) -> Result<()> {
        self.proofs += 1;
        if self.proofs > 8 * (self.channels.len() as u32 + 1) {
            self.strikes += 1;
            return self.send(reject(code::PROTOCOL, "too many proof requests")).await;
        }
        let entry = { self.svc.recent.lock().unwrap().iter().rev().find(|e| e.plan.height == g.height).cloned() };
        let Some(e) = entry else {
            return self.send(reject(code::NOT_FOUND, "no accounting state for that height")).await;
        };
        let Some(idx) = e.tree.index_of(&g.script) else {
            return self.send(reject(code::NOT_FOUND, "script has no credit in that state")).await;
        };
        let (credit, siblings) = (e.tree.credit(idx).unwrap_or(0), e.tree.proof(idx).unwrap_or_default());
        debug_assert!(merkle::verify(&e.plan.accounting_root, e.tree.len() as u32, idx as u32, &g.script, credit, &siblings));
        self.send(Msg::Proof(Proof { height: g.height, root: e.plan.accounting_root, leaves: e.tree.len() as u32, index: idx as u32, credit, siblings })).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announced_difficulty_must_equal_the_nodes() {
        assert!(check_announce_terms(0x1d00ffff, 0x1d00ffff).is_ok());
        assert!(check_announce_terms(0x207fffff, 0x1d00ffff).is_err(), "an easier target would turn every hash into a block and bypass the share target");
        assert!(check_announce_terms(0x1d00ffff, 0).is_err(), "no network target known yet");
    }

    #[test]
    fn address_keys_group_ipv6_by_64_bits() {
        let a: std::net::IpAddr = "2001:db8:1:2:aaaa:bbbb:cccc:dddd".parse().unwrap();
        let b: std::net::IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let c: std::net::IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(ip_key(a), ip_key(b));
        assert_ne!(ip_key(a), ip_key(c));
        assert_eq!(ip_key("192.0.2.7".parse().unwrap()), "ip:192.0.2.7");
    }

    #[test]
    fn payout_scripts_are_limited_to_witness_programs() {
        assert!(valid_payout_script(&[&[0u8, 20][..], &[1u8; 20][..]].concat()));
        assert!(valid_payout_script(&[&[0u8, 32][..], &[1u8; 32][..]].concat()));
        assert!(valid_payout_script(&[&[0x51u8, 32][..], &[1u8; 32][..]].concat()));
        assert!(!valid_payout_script(&[0x6a, 0x01, 0x00]), "OP_RETURN");
        assert!(!valid_payout_script(&[0x51]), "anyone-can-spend");
        assert!(!valid_payout_script(&[&[0u8, 21][..], &[1u8; 21][..]].concat()));
    }

    #[test]
    fn selection_is_proportional_and_stays_within_the_budget() {
        // everybody trusted: sum 40, budget 8 -> divide by 5, expected checks = 8
        let ps = vec![0.02; 2000];
        let sum: f64 = ps.iter().sum();
        let scale = scale_for(sum, 8);
        assert!((scale - 5.0).abs() < 1e-9);
        assert!((ps.iter().map(|p| p / scale).sum::<f64>() - 8.0).abs() < 1e-9);
        // a new gateway (0.5) is checked 25 times as often as a trusted one (0.02)
        assert!((0.5f64 / 0.02 - 25.0).abs() < 1e-9);
        // small pools: the budget is not the limit, probabilities stay as they are
        assert_eq!(scale_for(1.0, 8), 1.0);
        assert_eq!(scale_for(0.0, 0), 1.0);
    }

    #[test]
    fn retarget_follows_the_share_rate() {
        // 10 s goal; 90 shares in 30 s = ten times too fast -> difficulty x4 (capped step)
        assert_eq!(retarget(100.0, 90, 30.0, 10.0, 0.001, 1e12), Some(400.0));
        // 1 share in 60 s is slower than the goal of 6 -> divide
        let d = retarget(100.0, 1, 60.0, 10.0, 0.001, 1e12).unwrap();
        assert!(d < 100.0 && d >= 25.0);
        // on target or too early: unchanged
        assert_eq!(retarget(100.0, 3, 30.0, 10.0, 0.001, 1e12), None);
        assert_eq!(retarget(100.0, 90, 10.0, 10.0, 0.001, 1e12), None);
        // limits
        assert_eq!(retarget(100.0, 90, 30.0, 10.0, 0.001, 150.0), Some(150.0));
    }

    #[test]
    fn the_key_file_is_created_once_and_reloaded() {
        let dir = std::env::temp_dir().join(format!("photon-key-{}", rand64()));
        let path = dir.to_str().unwrap().to_string();
        let a = load_or_create_key(&path).unwrap();
        let b = load_or_create_key(&path).unwrap();
        assert_eq!(a.public, b.public);
        assert_eq!(a.private, b.private);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_file(&path).unwrap();
    }
}
