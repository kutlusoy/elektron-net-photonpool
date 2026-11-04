//! Load generator: many simulated Photon gateways against one pool (`photond photon-load`). Test tool, not for production.
//!
//! One node call per tip attests the coinbase variants of all simulated gateways; every gateway announces its own variants,
//! answers spot checks with the node's real transaction set and sends shares at a configurable rate.
use super::coinbase::node_candidate;
use super::msg::*;
use super::noise::{self, NoiseConn};
use super::verify::{bits_to_target, VERSION_MASK};
use super::PROTOCOL_VERSION;
use crate::rpc::NodeRpc;
use crate::template::{header_hash, meets_target};
use crate::tx::{merkle_branch, merkle_root, sha256d};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::watch;

#[derive(Deserialize, Clone)]
pub struct LoadCfg {
    pub pool_addr: String,
    pub pool_key: String,
    pub node_rpc_url: String,
    pub node_auth: String,
    pub gateways: usize,
    #[serde(default = "d_variants")]
    pub variants: u32,
    /// Shares per second per gateway.
    #[serde(default = "d_rate")]
    pub shares_per_sec: f64,
    pub seconds: u64,
    /// New connections per second while ramping up.
    #[serde(default = "d_ramp")]
    pub ramp_per_sec: usize,
}
fn d_variants() -> u32 { 2 }
fn d_rate() -> f64 { 0.2 }
fn d_ramp() -> usize { 300 }

struct Tmpl {
    id: u32,
    height: u32,
    tip: [u8; 32],
    version: u32,
    bits: u32,
    curtime: u32,
    branch: Vec<[u8; 32]>,
    /// coinbases[gateway][variant]
    coinbases: Vec<Vec<Vec<u8>>>,
    tx_chunks: Vec<Vec<u8>>,
    tx_bytes: usize,
}

#[derive(Default)]
struct Counters {
    connected: AtomicU64,
    failed_connect: AtomicU64,
    dropped: AtomicU64,
    announces: AtomicU64,
    shares_sent: AtomicU64,
    shares_acked: AtomicU64,
    shares_rejected: AtomicU64,
    checks_served: AtomicU64,
    bytes_served: AtomicU64,
    rejects: AtomicU64,
}

#[derive(Serialize)]
struct Report {
    gateways: usize,
    connected: u64,
    failed_connect: u64,
    dropped: u64,
    announces: u64,
    shares_sent: u64,
    shares_acked: u64,
    shares_rejected: u64,
    checks_served: u64,
    mbytes_served: f64,
    rejects: u64,
    templates: u32,
    template_build_ms_avg: f64,
    tx_per_template_avg: f64,
}

struct Shared {
    plans: Mutex<BTreeMap<u32, PayoutPlan>>,
    tmpl: watch::Sender<Option<Arc<Tmpl>>>,
    c: Counters,
}

fn rev(h: &str) -> Result<[u8; 32]> {
    let mut b: [u8; 32] = hex::decode(h)?.try_into().map_err(|_| anyhow!("32 bytes expected"))?;
    b.reverse();
    Ok(b)
}

fn script_of(i: usize) -> Vec<u8> {
    let h = sha256d(&(i as u64).to_le_bytes());
    let mut s = vec![0x00, 0x14];
    s.extend_from_slice(&h[..20]);
    s
}

pub async fn run(cfg: LoadCfg) -> Result<()> {
    let node = Arc::new(NodeRpc::new(&cfg.node_rpc_url, &cfg.node_auth)?);
    let pin: [u8; 32] = hex::decode(&cfg.pool_key)?.try_into().map_err(|_| anyhow!("pool_key must be 32 bytes"))?;
    let (tx, _) = watch::channel(None);
    let sh = Arc::new(Shared { plans: Mutex::new(BTreeMap::new()), tmpl: tx, c: Counters::default() });
    let deadline = Instant::now() + Duration::from_secs(cfg.seconds);

    let provider = tokio::spawn(provider(cfg.clone(), node.clone(), sh.clone(), deadline));
    let mut tasks = Vec::new();
    for i in 0..cfg.gateways {
        let (gcfg, sh) = (cfg.clone(), sh.clone());
        tasks.push(tokio::spawn(async move {
            match gateway(i, gcfg, pin, sh.clone(), deadline).await {
                Ok(()) => {}
                Err(e) => {
                    sh.c.dropped.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!("gateway {i}: {e:#}");
                }
            }
        }));
        if (i + 1) % cfg.ramp_per_sec.max(1) == 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    for t in tasks {
        let _ = t.await;
    }
    let (n_tmpl, build_ms, txs) = provider.await??;
    let c = &sh.c;
    let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
    let r = Report {
        gateways: cfg.gateways, connected: g(&c.connected), failed_connect: g(&c.failed_connect), dropped: g(&c.dropped), announces: g(&c.announces), shares_sent: g(&c.shares_sent), shares_acked: g(&c.shares_acked),
        shares_rejected: g(&c.shares_rejected), checks_served: g(&c.checks_served), mbytes_served: g(&c.bytes_served) as f64 / 1e6, rejects: g(&c.rejects), templates: n_tmpl,
        template_build_ms_avg: if n_tmpl > 0 { build_ms / n_tmpl as f64 } else { 0.0 }, tx_per_template_avg: if n_tmpl > 0 { txs / n_tmpl as f64 } else { 0.0 },
    };
    println!("{}", serde_json::to_string(&r)?);
    Ok(())
}

async fn provider(cfg: LoadCfg, node: Arc<NodeRpc>, sh: Arc<Shared>, deadline: Instant) -> Result<(u32, f64, f64)> {
    let (mut last, mut id, mut build_ms, mut txs_total) = (String::new(), 0u32, 0.0f64, 0.0f64);
    'tips: while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let Ok(best) = node.best_block_hash().await else { continue };
        if best == last {
            continue;
        }
        let Ok(h) = node.call("getblockcount", json!([])).await else { continue };
        let height = h.as_u64().unwrap_or(0) as u32 + 1;
        let plan = { sh.plans.lock().unwrap().get(&height).cloned() };
        let Some(plan) = plan else { continue };
        let t0 = Instant::now();
        let probe = match node.call("getattestationbatch", json!([{"candidates": [{"outputs": [{"scriptPubKey": hex::encode(&plan.fee_script)}]}]}])).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("load: node busy, retrying: {e:#}");
                continue;
            }
        };
        let reward = probe["coinbasevalue"].as_u64().ok_or_else(|| anyhow!("no coinbasevalue"))?;
        let total = cfg.gateways * cfg.variants as usize;
        let mut cands = Vec::with_capacity(total);
        for g in 0..cfg.gateways {
            for v in 0..cfg.variants {
                let mut suffix = (g as u32).to_le_bytes().to_vec();
                suffix.extend_from_slice(&(v as u16).to_le_bytes());
                cands.push(node_candidate(&plan, reward, &suffix));
            }
        }
        let bt = match super::coinbase::attest_all(&node, &cands).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("load: node busy, retrying: {e:#}");
                continue 'tips;
            }
        };
        let mut coinbases: Vec<Vec<u8>> = Vec::with_capacity(total);
        for c in bt["candidates"].as_array().ok_or_else(|| anyhow!("no candidates"))? {
            coinbases.push(hex::decode(c["coinbase_hex"].as_str().ok_or_else(|| anyhow!("coinbase_hex"))?)?);
        }
        let txs = bt["transactions"].as_array().ok_or_else(|| anyhow!("no transactions"))?;
        let mut stream = Vec::new();
        let mut txids = Vec::new();
        for t in txs {
            let d = hex::decode(t["data"].as_str().unwrap_or(""))?;
            stream.extend_from_slice(&(d.len() as u32).to_le_bytes());
            stream.extend_from_slice(&d);
            txids.push(rev(t["txid"].as_str().unwrap_or(""))?);
        }
        let tx_chunks: Vec<Vec<u8>> = if stream.is_empty() { vec![vec![]] } else { stream.chunks(MAX_TX_CHUNK).map(|c| c.to_vec()).collect() };
        id += 1;
        let per: Vec<Vec<Vec<u8>>> = coinbases.chunks(cfg.variants as usize).map(|c| c.to_vec()).collect();
        let t = Tmpl {
            id, height, tip: rev(bt["tip"].as_str().unwrap_or(""))?, version: bt["version"].as_i64().unwrap_or(0) as u32, bits: u32::from_str_radix(bt["bits"].as_str().unwrap_or("0"), 16)?,
            curtime: bt["curtime"].as_u64().unwrap_or(0) as u32, branch: merkle_branch(&txids), coinbases: per, tx_bytes: stream.len(), tx_chunks,
        };
        build_ms += t0.elapsed().as_secs_f64() * 1000.0;
        txs_total += txids.len() as f64;
        eprintln!("load: template {id} height {height}: {} transactions ({} KB), {} candidates, built in {:.0} ms", txids.len(), t.tx_bytes / 1000, total, t0.elapsed().as_secs_f64() * 1000.0);
        sh.tmpl.send_replace(Some(Arc::new(t)));
        last = best;
    }
    Ok((id, build_ms, txs_total))
}

async fn gateway(index: usize, cfg: LoadCfg, pin: [u8; 32], sh: Arc<Shared>, deadline: Instant) -> Result<()> {
    let tcp = match TcpStream::connect(&cfg.pool_addr).await {
        Ok(t) => t,
        Err(e) => {
            sh.c.failed_connect.fetch_add(1, Ordering::Relaxed);
            bail!("connect: {e}");
        }
    };
    tcp.set_nodelay(true).ok();
    let mut conn: NoiseConn<TcpStream> = match tokio::time::timeout(Duration::from_secs(30), noise::connect(tcp, Some(pin))).await {
        Ok(Ok(c)) => c,
        _ => {
            sh.c.failed_connect.fetch_add(1, Ordering::Relaxed);
            bail!("handshake failed");
        }
    };
    conn.send(&Msg::Setup(Setup { version: PROTOCOL_VERSION, features: 0, max_frame: super::wire::MAX_FRAME as u32 }).encode()).await?;
    conn.send(&Msg::OpenChannel(OpenChannel { request: 1, payout_script: script_of(index), target: [0xff; 32] }).encode()).await?;
    sh.c.connected.fetch_add(1, Ordering::Relaxed);

    let mut rx = sh.tmpl.subscribe();
    let (mut channel, mut target) = (None::<u32>, [0xffu8; 32]);
    let mut tmpl: Option<Arc<Tmpl>> = None;
    let interval = Duration::from_secs_f64(1.0 / cfg.shares_per_sec.max(0.001));
    let mut next_share = tokio::time::Instant::now() + interval.mul_f64(0.2 + (index % 97) as f64 / 97.0 * 1.6);
    let mut seed = (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let end = tokio::time::Instant::now() + deadline.saturating_duration_since(Instant::now());
    loop {
        tokio::select! {
            f = conn.recv() => {
                match Msg::decode(&f?)? {
                    Msg::OpenChannelAck(a) => { channel = Some(a.channel); target = a.target; }
                    Msg::SetTarget(t) => target = t.target,
                    Msg::PayoutPlan(p) => { sh.plans.lock().unwrap().insert(p.height, p); }
                    Msg::GetTransactions(g) => {
                        if let Some(t) = tmpl.as_ref().filter(|t| t.id == g.template_id) {
                            for (i, c) in t.tx_chunks.iter().enumerate() {
                                conn.send(&Msg::Transactions(Transactions { template_id: t.id, variant: g.variant, seq: i as u32, last: i + 1 == t.tx_chunks.len(), data: c.clone() }).encode()).await?;
                            }
                            sh.c.checks_served.fetch_add(1, Ordering::Relaxed);
                            sh.c.bytes_served.fetch_add(t.tx_bytes as u64, Ordering::Relaxed);
                        }
                    }
                    Msg::ShareAck(a) => { sh.c.shares_acked.fetch_add(a.accepted as u64, Ordering::Relaxed); sh.c.shares_rejected.fetch_add(a.rejects.len() as u64, Ordering::Relaxed); }
                    Msg::Reject(_) => { sh.c.rejects.fetch_add(1, Ordering::Relaxed); }
                    Msg::Disconnect(r) => bail!("disconnected: {}", r.reason),
                    _ => {}
                }
            }
            r = rx.changed() => {
                r?;
                let t = rx.borrow_and_update().clone();
                if let Some(t) = t {
                    let covs = &t.coinbases[index];
                    conn.send(&Msg::TemplateAnnounce(TemplateAnnounce { tip: t.tip, template_id: t.id, height: t.height, version: t.version, bits: t.bits, curtime: t.curtime, branch: t.branch.clone(), first_variant: 0, coinbases: covs.clone() }).encode()).await?;
                    sh.c.announces.fetch_add(1, Ordering::Relaxed);
                    tmpl = Some(t);
                }
            }
            _ = tokio::time::sleep_until(next_share), if tmpl.is_some() && channel.is_some() => {
                next_share += interval;
                let t = tmpl.as_ref().unwrap();
                seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
                let variant = (seed % cfg.variants as u64) as usize;
                let root = merkle_root(&sha256d(&t.coinbases[index][variant]), &t.branch);
                let net = bits_to_target(t.bits);
                let mut nonce = (seed >> 16) as u32;
                for _ in 0..200_000 {
                    let mut h = [0u8; 80];
                    h[0..4].copy_from_slice(&t.version.to_le_bytes());
                    h[4..36].copy_from_slice(&t.tip);
                    h[36..68].copy_from_slice(&root);
                    h[68..72].copy_from_slice(&t.curtime.to_le_bytes());
                    h[72..76].copy_from_slice(&t.bits.to_le_bytes());
                    h[76..80].copy_from_slice(&nonce.to_le_bytes());
                    let hash = header_hash(&h);
                    if meets_target(&hash, &target) || meets_target(&hash, &net) {
                        conn.send(&Msg::Shares(Shares { channel: channel.unwrap(), template_id: t.id, items: vec![ShareItem { variant: variant as u32, nonce, ntime_delta: 0, version_bits: t.version & VERSION_MASK }] }).encode()).await?;
                        sh.c.shares_sent.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                    nonce = nonce.wrapping_add(1);
                }
            }
            _ = tokio::time::sleep_until(end) => return Ok(()),
        }
    }
}
