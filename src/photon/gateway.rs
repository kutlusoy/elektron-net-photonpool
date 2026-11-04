//! Photon gateway: runs next to the miner's own (patched) node, builds the coinbase from the pool's plan,
//! lets the node attest every extranonce variant, announces them and submits shares; found blocks go to the own node.
//!
//! Has a Stratum V1 front end for ASICs and a built-in CPU miner for tests (`software_miner`). A lost pool connection is re-established with
//! backoff; the ASICs keep working on the current template (and the next one while the plans last), shares of the outage are lost.
use super::coinbase::{node_candidate, parse, to_witness};
use super::merkle;
use super::msg::*;
use super::noise::{self, NoiseConn};
use super::plan::{check_plan, Rules};
use super::gateway_stratum::{self, Fwd, Shared, StratumGwCfg};
use super::verify::{bits_to_target, VERSION_MASK};
use super::PROTOCOL_VERSION;
use crate::addr::script_for_address;
use crate::diff::target_from_difficulty;
use crate::pool::Epoch;
use crate::rpc::NodeRpc;
use crate::template::{BatchTemplate, Candidate, TxSet};
use crate::template::{header_hash, meets_target};
use crate::tx::{merkle_branch, merkle_root, put_varint, sha256d};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Deserialize, Clone)]
pub struct GatewayCfg {
    pub node_rpc_url: String,
    pub node_auth: String,
    /// `host:port` of the pool's Photon listener.
    pub pool_addr: String,
    /// Hex of the pool's Noise public key (pinned).
    pub pool_key: String,
    /// `mainnet`, `testnet` or `regtest`.
    pub network: String,
    pub payout_address: String,
    /// Agreed pool terms; a plan with other terms is refused.
    pub fee_address: String,
    #[serde(default = "d_fee")]
    pub fee_pct: f64,
    #[serde(default = "d_nmax")]
    pub max_outputs: u16,
    #[serde(default = "d_minppm")]
    pub min_ppm: u32,
    /// Smallest payout output in leptons that the pool applies (agreed terms, like the fee).
    #[serde(default = "d_minlep")]
    pub min_output_leptons: u64,
    #[serde(default = "d_variants")]
    pub variants: u32,
    #[serde(default = "d_diff")]
    pub share_difficulty: f64,
    /// CPU search for tests and regtest. Real hardware is attached through the (not yet available) Stratum front end.
    #[serde(default)]
    pub software_miner: bool,
    /// Reconnect to the pool after a lost connection (default true); wrong key, ban and protocol mismatch are never retried.
    #[serde(default = "d_true")]
    pub reconnect: bool,
    /// Longest wait between reconnect attempts.
    #[serde(default = "d_maxwait")]
    pub max_reconnect_secs: u64,
    /// Stop after this many blocks found by this gateway (tests).
    #[serde(default)]
    pub stop_after_blocks: Option<u32>,
    /// Stratum V1 front end for ASICs (`[stratum] listen = "0.0.0.0:3333"`).
    #[serde(default)]
    pub stratum: StratumGwCfg,
    /// Test only: announce (and mine with) another compact target than the node's.
    #[serde(default)]
    pub announce_bits_override: Option<u32>,
    /// Test only: announce coinbases with a corrupted attestation to exercise the pool's spot check.
    #[serde(default)]
    pub corrupt_attestation: bool,
}

fn d_true() -> bool { true }
fn d_maxwait() -> u64 { 30 }
fn d_fee() -> f64 { 1.0 }
fn d_nmax() -> u16 { 128 }
fn d_minppm() -> u32 { 100 }
fn d_minlep() -> u64 { 100_000 }
fn d_variants() -> u32 { 16 }
fn d_diff() -> f64 { 0.001 }

struct Tpl {
    id: u32,
    height: u32,
    tip: [u8; 32],
    version: u32,
    bits: u32,
    curtime: u32,
    branch: Vec<[u8; 32]>,
    coinbases: Vec<Vec<u8>>,
    txids_root: Vec<[u8; 32]>,
    tx_data: Vec<Vec<u8>>,
    epoch: Arc<Epoch>,
}

impl Tpl {
    fn header(&self, variant: usize, nonce: u32) -> [u8; 80] {
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&self.version.to_le_bytes());
        h[4..36].copy_from_slice(&self.tip);
        h[36..68].copy_from_slice(&self.txids_root[variant]);
        h[68..72].copy_from_slice(&self.curtime.to_le_bytes());
        h[72..76].copy_from_slice(&self.bits.to_le_bytes());
        h[76..80].copy_from_slice(&nonce.to_le_bytes());
        h
    }

    fn block(&self, variant: usize, nonce: u32) -> Vec<u8> {
        let mut b = self.header(variant, nonce).to_vec();
        put_varint(&mut b, 1 + self.tx_data.len() as u64);
        b.extend_from_slice(&to_witness(&self.coinbases[variant]));
        for t in &self.tx_data {
            b.extend_from_slice(t);
        }
        b
    }
}

fn rev(hex_str: &str) -> Result<[u8; 32]> {
    let mut b: [u8; 32] = hex::decode(hex_str)?.try_into().map_err(|_| anyhow!("expected 32 bytes"))?;
    b.reverse();
    Ok(b)
}

/// Errors that retrying cannot fix: wrong pool key, ban, protocol mismatch.
#[derive(Debug)]
struct Fatal(String);
impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Fatal {}

/// State that survives a lost pool connection: the node link, the plans, the current template (so ASICs keep working on it),
/// and the channel table with gateway-local ids (the pool's channel ids change with every connection).
struct Gw {
    cfg: GatewayCfg,
    node: Arc<NodeRpc>,
    rules: Rules,
    pin: [u8; 32],
    epoch_tx: watch::Sender<Option<Arc<Epoch>>>,
    targets: Arc<RwLock<HashMap<u32, [u8; 32]>>>,
    fwd_rx: mpsc::UnboundedReceiver<Fwd>,
    locals: HashMap<Vec<u8>, u32>,
    scripts: BTreeMap<u32, Vec<u8>>,
    next_local: u32,
    plans: BTreeMap<u32, PayoutPlan>,
    tpl: Option<Tpl>,
    next_id: u32,
    last_tip: String,
    mining: bool,
    cursor: (u32, usize),
    blocks_found: u32,
}

/// State of one pool connection.
#[derive(Default)]
struct Link {
    pool_of_local: HashMap<u32, u32>,
    local_of_pool: HashMap<u32, u32>,
    waiters: HashMap<u32, Vec<oneshot::Sender<std::result::Result<u32, String>>>>,
    open_order: VecDeque<u32>,
    proof_queue: VecDeque<Vec<u8>>,
    proof_checked: std::collections::HashSet<(u32, Vec<u8>)>,
    pending: Vec<(u32, u32, ShareItem)>,
    commits: HashMap<u32, EpochCommit>,
    announced: Option<u32>,
    rejects_logged: u32,
}

pub async fn run(cfg: GatewayCfg) -> Result<()> {
    let net = match cfg.network.as_str() {
        "mainnet" => "be",
        "testnet" => "tb",
        "regtest" => "bcrt",
        o => bail!("unknown network {o}"),
    };
    let node = Arc::new(NodeRpc::new(&cfg.node_rpc_url, &cfg.node_auth)?);
    let my_script = script_for_address(&cfg.payout_address, net)?;
    let rules = Rules { n_max: cfg.max_outputs, min_ppm: cfg.min_ppm, min_output_leptons: cfg.min_output_leptons, fee_ppm: (cfg.fee_pct * 10_000.0).round() as u32, fee_script: script_for_address(&cfg.fee_address, net)? };
    let pin: [u8; 32] = hex::decode(&cfg.pool_key)?.try_into().map_err(|_| anyhow!("pool_key must be 32 bytes of hex"))?;

    if cfg.corrupt_attestation {
        tracing::warn!("gateway: corrupt_attestation is set - TEST ONLY, the announced coinbases are invalid and the pool will ban this payout address");
    }
    if cfg.announce_bits_override.is_some() {
        tracing::warn!("gateway: announce_bits_override is set - TEST ONLY, the pool will refuse the announces");
    }
    if cfg.software_miner {
        tracing::warn!("gateway: software_miner is set - CPU mining for tests only");
    }
    let (epoch_tx, epoch_rx) = watch::channel::<Option<Arc<Epoch>>>(None);
    let targets: Arc<RwLock<HashMap<u32, [u8; 32]>>> = Arc::new(RwLock::new(HashMap::new()));
    let (fwd_tx, fwd_rx) = mpsc::unbounded_channel::<Fwd>();
    if !cfg.stratum.listen.is_empty() {
        let shared = Arc::new(Shared { epoch: epoch_rx, targets: targets.clone(), hrp: net.to_string(), default_script: my_script.clone(), fwd: fwd_tx.clone(), node: node.clone(), stats: Default::default() });
        let scfg = cfg.stratum.clone();
        tokio::spawn(async move {
            if let Err(e) = gateway_stratum::serve(scfg, shared).await {
                tracing::error!("gateway stratum stopped: {e:#}");
            }
        });
    }
    let (reconnect, max_wait) = (cfg.reconnect, cfg.max_reconnect_secs.max(1));
    let mut gw = Gw {
        cfg, node, rules, pin, epoch_tx, targets, fwd_rx, locals: HashMap::from([(my_script.clone(), 1)]), scripts: BTreeMap::from([(1, my_script)]), next_local: 2, plans: BTreeMap::new(), tpl: None,
        next_id: 1, last_tip: String::new(), mining: false, cursor: (0, 0), blocks_found: 0,
    };
    let mut backoff = 1u64;
    loop {
        let started = std::time::Instant::now();
        let err = match gw.connection().await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        if err.downcast_ref::<Fatal>().is_some() || !reconnect {
            return Err(err);
        }
        if started.elapsed() > Duration::from_secs(30) {
            backoff = 1;
        }
        let jitter = (started.elapsed().subsec_nanos() as u64 % 700) as f64 / 1000.0;
        let wait = Duration::from_secs_f64(backoff as f64 + jitter);
        tracing::warn!("gateway: pool connection lost: {err:#}; reconnecting in {:.1} s (ASICs keep working on the current template while plans last)", wait.as_secs_f64());
        backoff = (backoff * 2).min(max_wait);
        gw.offline(wait).await;
    }
}

impl Gw {
    /// Waits out the reconnect delay while keeping the ASICs supplied with work.
    async fn offline(&mut self, dur: Duration) {
        let end = tokio::time::Instant::now() + dur;
        let mut link = Link::default();
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(end) => return,
                Some(ev) = self.fwd_rx.recv() => match ev {
                    Fwd::Open { script, reply } => {
                        let _ = reply.send(self.locals.get(&script).copied().ok_or_else(|| "the pool connection is down, try again shortly".to_string()));
                    }
                    Fwd::Block { height, .. } => tracing::info!("gateway: block at height {height} found while the pool is unreachable; it pays the last known plan"),
                    Fwd::Share { .. } => {}
                },
                _ = tick.tick() => { let _ = self.maintain(None, &mut link).await; }
            }
        }
    }

    /// Builds (and, when connected, announces) the template for the node's tip; errors of the node are logged, errors of the pool link are returned.
    async fn maintain(&mut self, mut conn: Option<&mut NoiseConn<TcpStream>>, link: &mut Link) -> Result<()> {
        let best = match self.node.best_block_hash().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("gateway: node not reachable: {e:#}");
                return Ok(());
            }
        };
        if best != self.last_tip {
            self.epoch_tx.send_replace(None);
            let height = match self.node.call("getblockcount", json!([])).await {
                Ok(v) => v.as_u64().unwrap_or(0) as u32 + 1,
                Err(_) => return Ok(()),
            };
            let default_open = conn.is_none() || link.pool_of_local.contains_key(&1);
            if let (Some(plan), true) = (self.plans.get(&height).cloned(), default_open) {
                match build_template(&self.node, &self.cfg, &plan, self.next_id).await {
                    Ok(t) => {
                        if let Some(c) = conn.as_deref_mut() {
                            announce(c, &t).await?;
                            link.announced = Some(t.id);
                        }
                        self.epoch_tx.send_replace(Some(t.epoch.clone()));
                        self.tpl = Some(t);
                        self.mining = true;
                        self.next_id += 1;
                        self.last_tip = best;
                        self.cursor = (0, 0);
                        link.pending.clear();
                    }
                    Err(e) => tracing::warn!("gateway: template failed: {e:#}"),
                }
            }
        } else if let (Some(c), Some(t)) = (conn.as_deref_mut(), self.tpl.as_ref()) {
            // a new pool connection knows nothing about the template of the current tip
            if link.announced != Some(t.id) && link.pool_of_local.contains_key(&1) {
                announce(c, t).await?;
                link.announced = Some(t.id);
            }
        }
        Ok(())
    }

    async fn connection(&mut self) -> Result<()> {
        let cfg = self.cfg.clone();
        let tcp = TcpStream::connect(&cfg.pool_addr).await.with_context(|| format!("connecting to {}", cfg.pool_addr))?;
        tcp.set_nodelay(true).ok();
        let mut conn: NoiseConn<TcpStream> = match noise::connect(tcp, Some(self.pin)).await {
            Ok(c) => c,
            Err(e) if e.to_string().contains("pinned key") => return Err(Fatal(format!("{e:#}")).into()),
            Err(e) => return Err(e),
        };
        conn.send(&Msg::Setup(Setup { version: PROTOCOL_VERSION, features: 0, max_frame: super::wire::MAX_FRAME as u32 }).encode()).await?;
        expect_ack(&mut conn).await?;
        let mut link = Link::default();
        // (re)open every known channel; the request number is the gateway-local channel id
        for (local, script) in &self.scripts {
            conn.send(&Msg::OpenChannel(OpenChannel { request: *local, payout_script: script.clone(), target: target_from_difficulty(cfg.share_difficulty) }).encode()).await?;
            link.open_order.push_back(*local);
        }
        let mut poll = tokio::time::interval(Duration::from_millis(200));

        loop {
            tokio::select! {
                biased;
                f = conn.recv() => {
                    match Msg::decode(&f?)? {
                        Msg::OpenChannelAck(a) => {
                            let local = a.request;
                            self.targets.write().unwrap().insert(local, a.target);
                            link.pool_of_local.insert(local, a.channel);
                            link.local_of_pool.insert(a.channel, local);
                            link.open_order.retain(|r| *r != local);
                            for w in link.waiters.remove(&local).unwrap_or_default() { let _ = w.send(Ok(local)); }
                            tracing::info!("gateway: channel {} open as local {} , plans from height {}", a.channel, local, a.plan_height);
                        }
                        Msg::SetTarget(t) => {
                            if let Some(local) = link.local_of_pool.get(&t.channel) { self.targets.write().unwrap().insert(*local, t.target); }
                        }
                        Msg::NewTip(_) => {}
                        Msg::PayoutPlan(p) => {
                            match check_plan(&p, &self.rules) {
                                Ok(()) => { self.plans.insert(p.height, p); }
                                Err(e) => tracing::warn!("gateway: refusing plan for height {}: {e:#}", p.height),
                            }
                            while self.plans.len() > 8 { let k = *self.plans.keys().next().unwrap(); self.plans.remove(&k); }
                        }
                        Msg::EpochCommit(c) => {
                            if let Some(p) = self.plans.get(&c.height) {
                                if p.accounting_root != c.root { return Err(Fatal("EpochCommit root differs from the plan".into()).into()); }
                            }
                            link.commits.insert(c.height, c.clone());
                            // every channel's owner can check its own credit; the pool answers each request with a Proof or a Reject(8), in order
                            let opened: Vec<Vec<u8>> = link.pool_of_local.keys().filter_map(|l| self.scripts.get(l).cloned()).collect();
                            for script in opened {
                                link.proof_queue.push_back(script.clone());
                                conn.send(&Msg::GetProof(GetProof { height: c.height, script }).encode()).await?;
                            }
                        }
                        Msg::Proof(pr) => {
                            let Some(script) = link.proof_queue.pop_front() else { continue };
                            let root = link.commits.get(&pr.height).map(|c| c.root).unwrap_or(pr.root);
                            if root != pr.root || !merkle::verify(&pr.root, pr.leaves, pr.index, &script, pr.credit, &pr.siblings) {
                                return Err(Fatal(format!("accounting proof for height {} does not verify", pr.height)).into());
                            }
                            if link.proof_checked.insert((pr.height, script.clone())) { tracing::info!("gateway: credit {} of {} verified against the committed root at height {}", pr.credit, hex::encode(&script), pr.height); }
                        }
                        Msg::ShareAck(a) => {
                            if !a.rejects.is_empty() && link.rejects_logged < 5 {
                                link.rejects_logged += 1;
                                tracing::warn!("gateway: {} shares accepted, {} rejected (first codes {:?})", a.accepted, a.rejects.len(), &a.rejects[..a.rejects.len().min(5)]);
                            }
                        }
                        Msg::GetTransactions(g) => {
                            if let Some(t) = self.tpl.as_ref().filter(|t| t.id == g.template_id) {
                                let mut stream = Vec::new();
                                for d in &t.tx_data { stream.extend_from_slice(&(d.len() as u32).to_le_bytes()); stream.extend_from_slice(d); }
                                let chunks: Vec<&[u8]> = if stream.is_empty() { vec![&[][..]] } else { stream.chunks(MAX_TX_CHUNK).collect() };
                                for (i, c) in chunks.iter().enumerate() {
                                    conn.send(&Msg::Transactions(Transactions { template_id: g.template_id, variant: g.variant, seq: i as u32, last: i + 1 == chunks.len(), data: c.to_vec() }).encode()).await?;
                                }
                            }
                        }
                        Msg::Reject(r) if r.code == 8 => { link.proof_queue.pop_front(); tracing::debug!("gateway: {}", r.reason); }
                        // refusals of OpenChannel arrive in request order
                        Msg::Reject(r) if matches!(r.code, 1 | 2 | 3) => {
                            if let Some(local) = link.open_order.pop_front() {
                                if local == 1 { return Err(Fatal(format!("pool refused the default channel: {}", r.reason)).into()); }
                                if let Some(script) = self.scripts.remove(&local) { self.locals.remove(&script); }
                                self.targets.write().unwrap().remove(&local);
                                for w in link.waiters.remove(&local).unwrap_or_default() { let _ = w.send(Err(r.reason.clone())); }
                            }
                        }
                        Msg::Reject(r) => tracing::warn!("gateway: pool rejected: {} ({})", r.reason, r.code),
                        Msg::Disconnect(r) if matches!(r.code, 21 | 23) => return Err(Fatal(format!("pool disconnected us: {}", r.reason)).into()),
                        Msg::Disconnect(r) => bail!("pool disconnected us: {}", r.reason),
                        _ => {}
                    }
                }
                Some(ev) = self.fwd_rx.recv() => {
                    let mut events = vec![ev];
                    while let Ok(more) = self.fwd_rx.try_recv() { events.push(more); }
                    let mut blocks = vec![];
                    for ev in events {
                        match ev {
                            // shares of the previous template stay valid for the pool's grace period, so they are sent under their own template id
                            Fwd::Share { channel: local, template_id, item } => link.pending.push((local, template_id, item)),
                            Fwd::Block { template_id, variant, height, hash } => blocks.push(BlockFound { template_id, variant, height, hash }),
                            Fwd::Open { script, reply } => {
                                if let Some(&local) = self.locals.get(&script) {
                                    if link.pool_of_local.contains_key(&local) { let _ = reply.send(Ok(local)); } else { link.waiters.entry(local).or_default().push(reply); }
                                } else {
                                    let local = self.next_local;
                                    self.next_local += 1;
                                    self.locals.insert(script.clone(), local);
                                    self.scripts.insert(local, script.clone());
                                    conn.send(&Msg::OpenChannel(OpenChannel { request: local, payout_script: script, target: target_from_difficulty(cfg.share_difficulty) }).encode()).await?;
                                    link.open_order.push_back(local);
                                    link.waiters.entry(local).or_default().push(reply);
                                }
                            }
                        }
                    }
                    // shares go out immediately: a block changes the tip and the pool drops templates of the old tip
                    send_pending(&mut conn, &mut link).await?;
                    for b in blocks {
                        self.blocks_found += 1;
                        conn.send(&Msg::BlockFound(b).encode()).await?;
                        if cfg.stop_after_blocks.map_or(false, |n| self.blocks_found >= n) { return Ok(()); }
                    }
                }
                _ = poll.tick() => {
                    self.maintain(Some(&mut conn), &mut link).await?;
                    send_pending(&mut conn, &mut link).await?;
                }
                _ = tokio::task::yield_now(), if cfg.software_miner && self.mining && self.tpl.is_some() && link.pool_of_local.contains_key(&1) => {
                    let t = self.tpl.as_ref().unwrap();
                    let net_target = bits_to_target(t.bits);
                    let own_target = self.targets.read().unwrap().get(&1).copied().unwrap_or([0u8; 32]);
                    let (mut variant_cursor, mut nonce_cursor) = (self.cursor.1, self.cursor.0);
                    let mut found: Option<(usize, u32, [u8; 32])> = None;
                    for _ in 0..20_000 {
                        let hash = header_hash(&t.header(variant_cursor, nonce_cursor));
                        let block = meets_target(&hash, &net_target);
                        if block || meets_target(&hash, &own_target) {
                            link.pending.push((1, t.id, ShareItem { variant: variant_cursor as u32, nonce: nonce_cursor, ntime_delta: 0, version_bits: t.version & VERSION_MASK }));
                        }
                        if block {
                            found = Some((variant_cursor, nonce_cursor, hash));
                            break;
                        }
                        variant_cursor += 1;
                        if variant_cursor >= t.coinbases.len() { variant_cursor = 0; nonce_cursor = nonce_cursor.wrapping_add(1); }
                        if link.pending.len() >= 256 { break; }
                    }
                    self.cursor = (nonce_cursor, variant_cursor);
                    if let Some((variant, nonce, hash)) = found {
                        let (tid, height) = (t.id, t.height);
                        let hex_block = hex::encode(t.block(variant, nonce));
                        let res = self.node.submit_block(&hex_block).await?;
                        tracing::info!("gateway: block at height {height} submitted: {:?}", res);
                        if res.is_none() {
                            self.blocks_found += 1;
                            conn.send(&Msg::BlockFound(BlockFound { template_id: tid, variant: variant as u32, height, hash }).encode()).await?;
                        }
                        send_pending(&mut conn, &mut link).await?;
                        self.mining = false;
                        if cfg.stop_after_blocks.map_or(false, |n| self.blocks_found >= n) { return Ok(()); }
                    }
                }
            }
        }
    }
}

async fn send_pending(conn: &mut NoiseConn<TcpStream>, link: &mut Link) -> Result<()> {
    let mut groups: BTreeMap<(u32, u32), Vec<ShareItem>> = BTreeMap::new();
    for (local, template_id, item) in link.pending.drain(..) {
        // shares of a channel that is not open on this connection (yet) are dropped
        if let Some(pool_channel) = link.pool_of_local.get(&local) {
            groups.entry((*pool_channel, template_id)).or_default().push(item);
        }
    }
    for ((channel, template_id), items) in groups {
        for chunk in items.chunks(MAX_SHARES_PER_MSG) {
            conn.send(&Msg::Shares(Shares { channel, template_id, items: chunk.to_vec() }).encode()).await?;
        }
    }
    Ok(())
}

async fn expect_ack(conn: &mut NoiseConn<TcpStream>) -> Result<()> {
    match Msg::decode(&tokio::time::timeout(Duration::from_secs(10), conn.recv()).await.map_err(|_| anyhow!("no SetupAck"))??)? {
        Msg::SetupAck(_) => Ok(()),
        Msg::Disconnect(r) if matches!(r.code, 21 | 23) => Err(Fatal(format!("pool refused: {}", r.reason)).into()),
        Msg::Disconnect(r) => bail!("pool refused: {}", r.reason),
        _ => bail!("unexpected reply to Setup"),
    }
}

async fn build_template(node: &Arc<NodeRpc>, cfg: &GatewayCfg, plan: &PayoutPlan, id: u32) -> Result<Tpl> {
    // the reward is only known from the node's template; the trivial candidate costs one cheap call
    let probe = node.call("getattestationbatch", json!([{"candidates": [{"outputs": [{"scriptPubKey": hex::encode(&plan.fee_script)}]}]}])).await?;
    let reward = probe["coinbasevalue"].as_u64().ok_or_else(|| anyhow!("no coinbasevalue"))?;
    // the plan's reference reward must be about the node's reward, otherwise the minimum output amount could be bent
    if plan.ref_reward == 0 || plan.ref_reward > reward.saturating_mul(4) || plan.ref_reward.saturating_mul(4) < reward {
        bail!("plan reference reward {} is implausible for the node's reward {}", plan.ref_reward, reward);
    }
    let height = probe["height"].as_u64().unwrap_or(0) as u32;
    if height != plan.height {
        bail!("node template is for height {height}, plan is for {}", plan.height);
    }
    let cands: Vec<Value> = (0..cfg.variants).map(|v| node_candidate(plan, reward, &v.to_le_bytes())).collect();
    let bt = super::coinbase::attest_all(node, &cands).await?;
    if bt["coinbasevalue"].as_u64() != Some(reward) || bt["height"].as_u64() != Some(height as u64) {
        bail!("template changed between the two node calls");
    }
    let txs = bt["transactions"].as_array().ok_or_else(|| anyhow!("no transactions"))?;
    let tx_data: Vec<Vec<u8>> = txs.iter().map(|t| Ok(hex::decode(t["data"].as_str().ok_or_else(|| anyhow!("tx data"))?)?)).collect::<Result<_>>()?;
    let txids: Vec<[u8; 32]> = txs.iter().map(|t| rev(t["txid"].as_str().unwrap_or(""))).collect::<Result<_>>()?;
    let branch = merkle_branch(&txids);
    let coinbases: Vec<Vec<u8>> = bt["candidates"].as_array().ok_or_else(|| anyhow!("no candidates"))?.iter().map(|c| Ok(hex::decode(c["coinbase_hex"].as_str().ok_or_else(|| anyhow!("coinbase_hex"))?)?)).collect::<Result<_>>()?;
    let mut coinbases = coinbases;
    for cb in coinbases.iter_mut() {
        let parsed = parse(cb)?;
        if cfg.corrupt_attestation {
            let att = &parsed.outputs[parsed.outputs.len() - 3].1;
            let at = cb.windows(att.len()).position(|w| w == att.as_slice()).ok_or_else(|| anyhow!("attestation not found"))?;
            cb[at + att.len() - 1] ^= 0x01;
        }
    }
    let txids_root = coinbases.iter().map(|cb| merkle_root(&sha256d(cb), &branch)).collect();
    let template: BatchTemplate = serde_json::from_value(bt.clone()).context("parsing the node's template")?;
    let txset = Arc::new(TxSet::from_gbt(&template.transactions)?);
    let candidates = coinbases.iter().map(|cb| Candidate::from_coinbase(&template, &txset, cb)).collect::<Result<Vec<_>>>()?;
    let epoch = Arc::new(Epoch::new(id as u64, bt["tip"].as_str().unwrap_or_default().to_string(), height, candidates));
    Ok(Tpl {
        epoch,
        id, height, tip: rev(bt["tip"].as_str().ok_or_else(|| anyhow!("tip"))?)?, version: bt["version"].as_i64().unwrap_or(0) as u32,
        bits: u32::from_str_radix(bt["bits"].as_str().ok_or_else(|| anyhow!("bits"))?, 16)?, curtime: bt["curtime"].as_u64().unwrap_or(0) as u32,
        branch, coinbases, txids_root, tx_data,
    })
    .map(|mut t| {
        if let Some(b) = cfg.announce_bits_override {
            t.bits = b;
        }
        t
    })
}

async fn announce(conn: &mut NoiseConn<TcpStream>, t: &Tpl) -> Result<()> {
    let mut i = 0usize;
    while i < t.coinbases.len() {
        let (mut bytes, mut n) = (0usize, 0usize);
        while i + n < t.coinbases.len() && n < MAX_VARIANTS_PER_ANNOUNCE && bytes + t.coinbases[i + n].len() + 4 <= 48_000 {
            bytes += t.coinbases[i + n].len() + 4;
            n += 1;
        }
        if n == 0 {
            bail!("a single coinbase does not fit into a frame");
        }
        conn.send(&Msg::TemplateAnnounce(TemplateAnnounce {
            tip: t.tip, template_id: t.id, height: t.height, version: t.version, bits: t.bits, curtime: t.curtime, branch: t.branch.clone(), first_variant: i as u32, coinbases: t.coinbases[i..i + n].to_vec(),
        }).encode()).await?;
        i += n;
    }
    Ok(())
}
