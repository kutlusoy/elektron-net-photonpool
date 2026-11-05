use crate::config::Mode;
use crate::diff::difficulty_from_target;
use crate::ledger::{self, BlockRec, ShareRow, LEPTON};
use crate::pool::{unix_now, Pool};
use crate::rpc::NodeRpc;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

pub const MATURITY_CONFIRMATIONS: i64 = 101;
const ADDRESS_LABEL: &str = "photon-pool";

pub fn minute_now() -> i64 {
    (unix_now() / 60.0) as i64
}

/// Opens a handle to the payout wallet of the node (`/wallet/<name>`).
pub fn wallet_rpc(base_url: &str, auth: &str, name: &str) -> Result<NodeRpc> {
    NodeRpc::new(&format!("{}/wallet/{}", base_url.trim_end_matches('/'), name), auth)
}

/// Makes sure the wallet is loaded (creating it if needed) and holds `count` addresses labelled for the pool.
pub async fn provision_wallet_addresses(node: &NodeRpc, wallet: &NodeRpc, name: &str, count: usize, pass_env: &str) -> Result<Vec<String>> {
    let loaded = node.call("listwallets", json!([])).await?;
    if !loaded.as_array().map(|a| a.iter().any(|w| w == name)).unwrap_or(false) {
        if node.call("loadwallet", json!([name])).await.is_err() {
            node.call("createwallet", json!([name])).await.with_context(|| format!("cannot load or create wallet {name}"))?;
        }
    }
    let mut addrs: Vec<String> = match wallet.call("getaddressesbylabel", json!([ADDRESS_LABEL])).await {
        Ok(v) => v.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    addrs.sort();
    let mut unlocked = false;
    while addrs.len() < count {
        let a = match wallet.call("getnewaddress", json!([ADDRESS_LABEL, "bech32"])).await {
            Ok(a) => a,
            Err(e) if !unlocked && e.to_string().contains("-13") => {
                let pass = std::env::var(pass_env).unwrap_or_default();
                wallet.call("walletpassphrase", json!([pass, 120])).await.context("the wallet is locked and cannot be unlocked")?;
                unlocked = true;
                wallet.call("getnewaddress", json!([ADDRESS_LABEL, "bech32"])).await?
            }
            Err(e) => return Err(e),
        };
        addrs.push(a.as_str().ok_or_else(|| anyhow!("bad getnewaddress"))?.to_string());
    }
    if unlocked {
        let _ = wallet.call("walletlock", json!([])).await;
    }
    addrs.sort();
    Ok(addrs)
}

impl Pool {
    pub fn agg_add(&self, minute: i64, account: &str, mode: Mode, diff: f64, cnt: u64) {
        let mut m = self.agg.lock().unwrap();
        let e = m.entry((minute, account.to_string(), mode as u8)).or_default();
        e.0 += diff;
        e.1 += cnt;
    }

    /// Writes the in-memory share aggregates to the ledger; PPS rows wait while no network difficulty is known.
    pub async fn flush_shares(&self) -> Result<()> {
        let epoch = self.epoch.borrow().clone();
        let pps_value = epoch.as_ref().and_then(|e| e.candidates.first()).map(|c| {
            let net = c.subsidy() as f64 * (1.0 - self.cfg.payout.fee(Mode::Pps) / 100.0);
            (net / difficulty_from_target(&c.target), net as i64)
        });
        let rows: Vec<ShareRow> = {
            let mut m = self.agg.lock().unwrap();
            let keys: Vec<_> = m.keys().filter(|k| pps_value.is_some() || k.2 != Mode::Pps as u8).cloned().collect();
            keys.into_iter()
                .filter_map(|k| m.remove(&k).map(|(d, c)| ShareRow { minute: k.0, account: k.1, mode: k.2 as i64, diff: d, count: c }))
                .collect()
        };
        if rows.is_empty() {
            return Ok(());
        }
        self.ledger.call(move |c| ledger::add_shares(c, &rows, pps_value)).await
    }

    /// Persists an accepted block and, for PPLNS finders, its pending credits.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_block(&self, epoch_height: u32, reward: i64, subsidy: i64, net_diff: f64, hash: String, account: String, worker: String, mode: Mode) -> Result<()> {
        self.flush_shares().await?;
        let rec = BlockRec {
            hash,
            height: epoch_height,
            account,
            worker,
            mode: mode as i64,
            reward,
            subsidy,
            net_diff,
            found_at: unix_now() as i64,
        };
        let (wf, fee) = (self.cfg.payout.pplns.window_factor, self.cfg.payout.fee(Mode::Pplns));
        let credits = self.ledger.call(move |c| ledger::record_block(c, &rec, wf, fee)).await?;
        if !credits.is_empty() {
            self.event("info", format!("block {epoch_height}: {} PPLNS credits recorded", credits.len()));
        }
        Ok(())
    }

    pub async fn run_accounting(self: Arc<Self>) {
        let (a, b, c) = (self.clone(), self.clone(), self.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            let mut last_prune = std::time::Instant::now();
            loop {
                tick.tick().await;
                if let Err(e) = a.flush_shares().await {
                    a.event("err", format!("share flush failed: {e}"));
                }
                if last_prune.elapsed() > Duration::from_secs(3600) {
                    last_prune = std::time::Instant::now();
                    let cutoff = minute_now() - a.cfg.ledger.keep_days * 1440;
                    let _ = a.ledger.call(move |c| ledger::prune_shares(c, cutoff)).await;
                    let stats_cutoff = minute_now() - a.cfg.ledger.stats_keep_days * 1440;
                    let _ = a.ledger.call(move |c| ledger::prune_history(c, stats_cutoff)).await;
                }
            }
        });
        tokio::spawn(async move { b.maturity_loop().await });
        c.payout_loop().await;
    }

    async fn maturity_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(self.cfg.payout.maturity_poll_secs.max(1)));
        loop {
            tick.tick().await;
            if let Err(e) = self.check_blocks().await {
                self.event("warn", format!("block maturity check failed: {e}"));
            }
        }
    }

    pub async fn check_blocks(&self) -> Result<()> {
        for hash in self.ledger.call(|c| ledger::open_blocks(c)).await? {
            let h = match self.node.call("getblockheader", json!([hash])).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let conf = h["confirmations"].as_i64().unwrap_or(0);
            let status = if conf < 0 {
                "orphaned"
            } else if conf >= MATURITY_CONFIRMATIONS {
                "mature"
            } else {
                "maturing"
            };
            let hh = hash.clone();
            self.ledger.call(move |c| ledger::set_block_status(c, &hh, status)).await?;
            if status != "maturing" {
                self.event("info", format!("block {}.. is {status}", &hash[..16.min(hash.len())]));
            }
        }
        Ok(())
    }

    async fn payout_loop(self: Arc<Self>) {
        let (mut warned, mut streak) = (false, 0u32);
        loop {
            if let Err(e) = self.recover_payouts().await {
                self.event("warn", format!("payout recovery: {e}"));
            }
            if let Err(e) = self.update_pps_guard().await {
                self.event("warn", format!("PPS guard: {e}"));
            }
            let mut failed = false;
            match self.run_payouts().await {
                Ok(f) => failed = f,
                Err(e) => {
                    if self.wallet.is_none() {
                        if !warned {
                            self.event("info", "no payout wallet configured, payouts and PPS are disabled".into());
                            warned = true;
                        }
                    } else {
                        self.event("err", format!("payout run failed: {e}"));
                    }
                }
            }
            streak = if failed { (streak + 1).min(6) } else { 0 };
            tokio::time::sleep(Duration::from_secs(self.cfg.payout.interval_secs.max(1) << streak)).await;
        }
    }

    /// PPS is only served while the wallet holds the reserve above all liabilities and the PPS hashrate share is within its cap.
    async fn update_pps_guard(&self) -> Result<()> {
        let ready = match &self.wallet {
            None => false,
            Some(w) => {
                let balance = (w.call("getbalance", json!([])).await?.as_f64().unwrap_or(0.0) * LEPTON as f64).round() as i64;
                let liabilities = self.ledger.call(|c| ledger::liabilities(c)).await?;
                let free = balance - liabilities;
                let (pps, total) = {
                    let l = self.live.lock().unwrap();
                    (l.pps_hashrate, l.hashrate)
                };
                let within_cap = total <= 0.0 || pps / total <= self.cfg.payout.pps.max_hashrate_share;
                self.treasury_free.store(free, Ordering::Relaxed);
                self.cfg.payout.pps.enabled && free as f64 >= self.cfg.payout.pps.reserve_min * LEPTON as f64 && within_cap
            }
        };
        if self.pps_ready.swap(ready, Ordering::Relaxed) != ready {
            self.event("info", format!("PPS is now {}", if ready { "available" } else { "paused" }));
        }
        Ok(())
    }

    /// Pending payouts left by a crash are matched against the wallet by their comment and either completed or reverted.
    async fn recover_payouts(&self) -> Result<()> {
        let Some(w) = &self.wallet else { return Ok(()) };
        let pending = self.ledger.call(|c| ledger::pending_payouts(c)).await?;
        if pending.is_empty() {
            return Ok(());
        }
        let txs = w.call("listtransactions", json!(["*", 1000, 0, true])).await?;
        let mut by_comment: HashMap<String, String> = HashMap::new();
        for t in txs.as_array().into_iter().flatten() {
            if let (Some(c), Some(id)) = (t["comment"].as_str(), t["txid"].as_str()) {
                by_comment.insert(c.to_string(), id.to_string());
            }
        }
        for id in pending {
            match by_comment.get(&payout_comment(id)).cloned() {
                Some(txid) => {
                    self.ledger.call(move |c| ledger::finish_payout(c, id, &txid)).await?;
                    self.event("info", format!("payout {id} recovered from the wallet"));
                }
                None => {
                    self.ledger.call(move |c| ledger::fail_payout(c, id)).await?;
                    self.event("warn", format!("payout {id} was never broadcast, balances restored"));
                }
            }
        }
        Ok(())
    }

    /// Returns true when at least one payout transaction failed and was reverted.
    async fn run_payouts(&self) -> Result<bool> {
        let Some(w) = &self.wallet else { bail!("no wallet") };
        let min = self.cfg.payout.min_payout_leptons;
        let payable = self.ledger.call(move |c| ledger::payable(c, min.max(1))).await?;
        if payable.is_empty() {
            return Ok(false);
        }
        let mut any_failed = false;
        if self.cfg.payout.dry_run {
            let total: i64 = payable.iter().map(|(_, a)| a).sum();
            self.event("info", format!("dry run: would pay {} accounts, {:.8} coins in total", payable.len(), total as f64 / LEPTON as f64));
            return Ok(false);
        }
        for chunk in payable.chunks(self.cfg.payout.max_outputs_per_tx.max(1)) {
            let items = chunk.to_vec();
            let id = self.ledger.call({
                let items = items.clone();
                move |c| ledger::begin_payout(c, &items, unix_now() as i64)
            }).await?;
            let mut amounts = serde_json::Map::new();
            for (acc, a) in &items {
                amounts.insert(acc.clone(), json!(*a as f64 / LEPTON as f64));
            }
            let unlocked = self.unlock_wallet(w).await;
            let params = match self.cfg.payout.fee_rate {
                Some(rate) => json!(["", Value::Object(amounts), 1, payout_comment(id), [], null, null, null, rate]),
                None => json!(["", Value::Object(amounts), 1, payout_comment(id)]),
            };
            let res = w.call("sendmany", params).await;
            if unlocked {
                let _ = w.call("walletlock", json!([])).await;
            }
            match res {
                Ok(v) => {
                    let txid = v.as_str().unwrap_or_default().to_string();
                    let t2 = txid.clone();
                    self.ledger.call(move |c| ledger::finish_payout(c, id, &t2)).await?;
                    self.event("info", format!("payout {id}: {} accounts, tx {}", items.len(), txid));
                }
                Err(e) if e.to_string().contains("unreachable") => {
                    self.event("warn", format!("payout {id} outcome unknown ({e}), it will be reconciled with the wallet"));
                }
                Err(e) => {
                    self.ledger.call(move |c| ledger::fail_payout(c, id)).await?;
                    self.event("err", format!("payout {id} failed and was reverted: {e}"));
                    any_failed = true;
                    break;
                }
            }
        }
        Ok(any_failed)
    }

    async fn unlock_wallet(&self, w: &NodeRpc) -> bool {
        match std::env::var(&self.cfg.payout.wallet_passphrase_env) {
            Ok(p) if !p.is_empty() => w.call("walletpassphrase", json!([p, 120])).await.is_ok(),
            _ => false,
        }
    }
}

fn payout_comment(id: i64) -> String {
    format!("photon-payout-{id}")
}
