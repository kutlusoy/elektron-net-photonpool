//! Photon gateway: runs next to the miner's own (patched) node, builds the coinbase from the pool's plan,
//! lets the node attest every extranonce variant, announces them and submits shares; found blocks go to the own node.
//!
//! This version has a built-in CPU miner for tests (`software_miner`). The Stratum V1 front end for ASICs is not part of it yet.
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
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

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
    #[serde(default = "d_variants")]
    pub variants: u32,
    #[serde(default = "d_diff")]
    pub share_difficulty: f64,
    /// CPU search for tests and regtest. Real hardware is attached through the (not yet available) Stratum front end.
    #[serde(default)]
    pub software_miner: bool,
    /// Stop after this many blocks found by this gateway (tests).
    #[serde(default)]
    pub stop_after_blocks: Option<u32>,
    /// Stratum V1 front end for ASICs (`[stratum] listen = "0.0.0.0:3333"`).
    #[serde(default)]
    pub stratum: StratumGwCfg,
    /// Test only: announce coinbases with a corrupted attestation to exercise the pool's spot check.
    #[serde(default)]
    pub corrupt_attestation: bool,
}

fn d_fee() -> f64 { 1.0 }
fn d_nmax() -> u16 { 128 }
fn d_minppm() -> u32 { 100 }
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

pub async fn run(cfg: GatewayCfg) -> Result<()> {
    let net = match cfg.network.as_str() {
        "mainnet" => "be",
        "testnet" => "tb",
        "regtest" => "bcrt",
        o => bail!("unknown network {o}"),
    };
    let node = NodeRpc::new(&cfg.node_rpc_url, &cfg.node_auth)?;
    let my_script = script_for_address(&cfg.payout_address, net)?;
    let rules = Rules { n_max: cfg.max_outputs, min_ppm: cfg.min_ppm, fee_ppm: (cfg.fee_pct * 10_000.0).round() as u32, fee_script: script_for_address(&cfg.fee_address, net)? };
    let pin: [u8; 32] = hex::decode(&cfg.pool_key)?.try_into().map_err(|_| anyhow!("pool_key must be 32 bytes of hex"))?;

    let tcp = TcpStream::connect(&cfg.pool_addr).await.with_context(|| format!("connecting to {}", cfg.pool_addr))?;
    tcp.set_nodelay(true).ok();
    let mut conn: NoiseConn<TcpStream> = noise::connect(tcp, Some(pin)).await?;
    conn.send(&Msg::Setup(Setup { version: PROTOCOL_VERSION, features: 0, max_frame: super::wire::MAX_FRAME as u32 }).encode()).await?;
    expect_ack(&mut conn).await?;
    conn.send(&Msg::OpenChannel(OpenChannel { request: 1, payout_script: my_script.clone(), target: target_from_difficulty(cfg.share_difficulty) }).encode()).await?;

    let node = Arc::new(node);
    let (epoch_tx, epoch_rx) = watch::channel::<Option<Arc<Epoch>>>(None);
    let (target_tx, target_rx) = watch::channel(target_from_difficulty(cfg.share_difficulty));
    let (fwd_tx, mut fwd_rx) = mpsc::unbounded_channel::<Fwd>();
    if !cfg.stratum.listen.is_empty() {
        let shared = Arc::new(Shared { epoch: epoch_rx, pool_target: target_rx, fwd: fwd_tx.clone(), node: node.clone(), stats: Default::default() });
        let scfg = cfg.stratum.clone();
        tokio::spawn(async move {
            if let Err(e) = gateway_stratum::serve(scfg, shared).await {
                tracing::error!("gateway stratum stopped: {e:#}");
            }
        });
    }
    let mut channel: Option<u32> = None;
    let mut target = target_from_difficulty(cfg.share_difficulty);
    let mut plans: BTreeMap<u32, PayoutPlan> = BTreeMap::new();
    let mut commits: HashMap<u32, EpochCommit> = HashMap::new();
    let mut tpl: Option<Tpl> = None;
    let mut next_id = 1u32;
    let (mut nonce_cursor, mut variant_cursor) = (0u32, 0usize);
    let mut pending: Vec<(u32, ShareItem)> = Vec::new();
    let mut last_tip = String::new();
    let mut poll = tokio::time::interval(Duration::from_millis(200));
    let mut blocks_found = 0u32;
    let mut rejects_logged = 0u32;
    let mut mining = false;
    let mut my_proof_checked: std::collections::HashSet<u32> = Default::default();

    loop {
        tokio::select! {
            biased;
            f = conn.recv() => {
                match Msg::decode(&f?)? {
                    Msg::OpenChannelAck(a) => { channel = Some(a.channel); target = a.target; target_tx.send_replace(a.target); tracing::info!("gateway: channel {} open, plans from height {}", a.channel, a.plan_height); }
                    Msg::SetTarget(t) => { target = t.target; target_tx.send_replace(t.target); }
                    Msg::NewTip(_) => {}
                    Msg::PayoutPlan(p) => {
                        match check_plan(&p, &rules) {
                            Ok(()) => { plans.insert(p.height, p); }
                            Err(e) => tracing::warn!("gateway: refusing plan for height {}: {e:#}", p.height),
                        }
                    }
                    Msg::EpochCommit(c) => {
                        if let Some(p) = plans.get(&c.height) {
                            if p.accounting_root != c.root { bail!("EpochCommit root differs from the plan"); }
                        }
                        commits.insert(c.height, c.clone());
                        conn.send(&Msg::GetProof(GetProof { height: c.height, script: my_script.clone() }).encode()).await?;
                    }
                    Msg::Proof(pr) => {
                        let root = commits.get(&pr.height).map(|c| c.root).unwrap_or(pr.root);
                        if root != pr.root || !merkle::verify(&pr.root, pr.leaves, pr.index, &my_script, pr.credit, &pr.siblings) {
                            bail!("accounting proof for height {} does not verify", pr.height);
                        }
                        if my_proof_checked.insert(pr.height) { tracing::info!("gateway: my credit {} verified against the committed root at height {}", pr.credit, pr.height); }
                    }
                    Msg::ShareAck(a) => {
                        if !a.rejects.is_empty() && rejects_logged < 5 {
                            rejects_logged += 1;
                            tracing::warn!("gateway: {} shares accepted, {} rejected (first codes {:?})", a.accepted, a.rejects.len(), &a.rejects[..a.rejects.len().min(5)]);
                        }
                    }
                    Msg::GetTransactions(g) => {
                        if let Some(t) = tpl.as_ref().filter(|t| t.id == g.template_id) {
                            let mut stream = Vec::new();
                            for d in &t.tx_data { stream.extend_from_slice(&(d.len() as u32).to_le_bytes()); stream.extend_from_slice(d); }
                            let chunks: Vec<&[u8]> = if stream.is_empty() { vec![&[][..]] } else { stream.chunks(MAX_TX_CHUNK).collect() };
                            for (i, c) in chunks.iter().enumerate() {
                                conn.send(&Msg::Transactions(Transactions { template_id: g.template_id, variant: g.variant, seq: i as u32, last: i + 1 == chunks.len(), data: c.to_vec() }).encode()).await?;
                            }
                        }
                    }
                    Msg::Reject(r) if r.code == 8 => tracing::debug!("gateway: {}", r.reason),
                    Msg::Reject(r) => tracing::warn!("gateway: pool rejected: {} ({})", r.reason, r.code),
                    Msg::Disconnect(r) => bail!("pool disconnected us: {}", r.reason),
                    _ => {}
                }
            }
            Some(ev) = fwd_rx.recv() => {
                let mut events = vec![ev];
                while let Ok(more) = fwd_rx.try_recv() { events.push(more); }
                let mut blocks = vec![];
                for ev in events {
                    match ev {
                        // shares of the previous template stay valid for the pool's grace period, so they are sent under their own template id
                        Fwd::Share { template_id, item } => pending.push((template_id, item)),
                        Fwd::Block { template_id, variant, height, hash } => blocks.push(BlockFound { template_id, variant, height, hash }),
                    }
                }
                // shares go out immediately: a block changes the tip and the pool drops templates of the old tip
                if let Some(ch) = channel { send_pending(&mut conn, ch, &mut pending).await?; }
                for b in blocks {
                    blocks_found += 1;
                    conn.send(&Msg::BlockFound(b).encode()).await?;
                    if cfg.stop_after_blocks.map_or(false, |n| blocks_found >= n) { return Ok(()); }
                }
            }
            _ = poll.tick() => {
                let best = node.best_block_hash().await?;
                if best != last_tip {
                    epoch_tx.send_replace(None);
                    let height = node.call("getblockcount", json!([])).await?.as_u64().unwrap_or(0) as u32 + 1;
                    if let (Some(plan), Some(ch)) = (plans.get(&height).cloned(), channel) {
                        let _ = ch;
                        match build_template(&node, &cfg, &plan, next_id).await {
                            Ok(t) => {
                                announce(&mut conn, &t, &plan).await?;
                                epoch_tx.send_replace(Some(t.epoch.clone()));
                                tpl = Some(t);
                                mining = true;
                                next_id += 1;
                                last_tip = best;
                                (nonce_cursor, variant_cursor) = (0, 0);
                                pending.clear();
                            }
                            Err(e) => tracing::warn!("gateway: template failed: {e:#}"),
                        }
                    }
                }
                if let Some(ch) = channel { send_pending(&mut conn, ch, &mut pending).await?; }
            }
            _ = tokio::task::yield_now(), if cfg.software_miner && mining && tpl.is_some() && channel.is_some() => {
                let t = tpl.as_ref().unwrap();
                let net_target = bits_to_target(t.bits);
                for _ in 0..20_000 {
                    let hash = header_hash(&t.header(variant_cursor, nonce_cursor));
                    let block = meets_target(&hash, &net_target);
                    if block || meets_target(&hash, &target) {
                        pending.push((t.id, ShareItem { variant: variant_cursor as u32, nonce: nonce_cursor, ntime_delta: 0, version_bits: t.version & VERSION_MASK }));
                    }
                    if block {
                        let hex_block = hex::encode(t.block(variant_cursor, nonce_cursor));
                        let res = node.submit_block(&hex_block).await?;
                        tracing::info!("gateway: block at height {} submitted: {:?}", t.height, res);
                        if res.is_none() {
                            blocks_found += 1;
                            conn.send(&Msg::BlockFound(BlockFound { template_id: t.id, variant: variant_cursor as u32, height: t.height, hash }).encode()).await?;
                        }
                        if let Some(ch) = channel { send_pending(&mut conn, ch, &mut pending).await?; }
                        mining = false;
                        if cfg.stop_after_blocks.map_or(false, |n| blocks_found >= n) { return Ok(()); }
                        break;
                    }
                    variant_cursor += 1;
                    if variant_cursor >= t.coinbases.len() { variant_cursor = 0; nonce_cursor = nonce_cursor.wrapping_add(1); }
                    if pending.len() >= 256 { break; }
                }
            }
        }
    }
}

async fn send_pending(conn: &mut NoiseConn<TcpStream>, channel: u32, pending: &mut Vec<(u32, ShareItem)>) -> Result<()> {
    let mut by_template: BTreeMap<u32, Vec<ShareItem>> = BTreeMap::new();
    for (id, item) in pending.drain(..) {
        by_template.entry(id).or_default().push(item);
    }
    for (template_id, items) in by_template {
        for chunk in items.chunks(MAX_SHARES_PER_MSG) {
            conn.send(&Msg::Shares(Shares { channel, template_id, items: chunk.to_vec() }).encode()).await?;
        }
    }
    Ok(())
}

async fn expect_ack(conn: &mut NoiseConn<TcpStream>) -> Result<()> {
    match Msg::decode(&tokio::time::timeout(Duration::from_secs(10), conn.recv()).await.map_err(|_| anyhow!("no SetupAck"))??)? {
        Msg::SetupAck(_) => Ok(()),
        Msg::Disconnect(r) => bail!("pool refused: {}", r.reason),
        _ => bail!("unexpected reply to Setup"),
    }
}

async fn build_template(node: &Arc<NodeRpc>, cfg: &GatewayCfg, plan: &PayoutPlan, id: u32) -> Result<Tpl> {
    // the reward is only known from the node's template; the trivial candidate costs one cheap call
    let probe = node.call("getattestationbatch", json!([{"candidates": [{"outputs": [{"scriptPubKey": hex::encode(&plan.fee_script)}]}]}])).await?;
    let reward = probe["coinbasevalue"].as_u64().ok_or_else(|| anyhow!("no coinbasevalue"))?;
    let height = probe["height"].as_u64().unwrap_or(0) as u32;
    if height != plan.height {
        bail!("node template is for height {height}, plan is for {}", plan.height);
    }
    let cands: Vec<Value> = (0..cfg.variants).map(|v| node_candidate(plan, reward, &v.to_le_bytes())).collect();
    let bt = node.call("getattestationbatch", json!([{"candidates": cands}])).await?;
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
}

async fn announce(conn: &mut NoiseConn<TcpStream>, t: &Tpl, _plan: &PayoutPlan) -> Result<()> {
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
