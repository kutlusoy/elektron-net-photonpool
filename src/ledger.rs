use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use std::sync::mpsc;
use tokio::sync::oneshot;

pub const LEPTON: i64 = 100_000_000;

#[allow(dead_code)]
pub const MODE_SOLO: i64 = 0;
pub const MODE_PPLNS: i64 = 1;
pub const MODE_PPS: i64 = 2;

pub struct ShareRow {
    pub minute: i64,
    pub account: String,
    pub mode: i64,
    pub diff: f64,
    pub count: u64,
}

pub struct BlockRec {
    pub hash: String,
    pub height: u32,
    pub account: String,
    pub worker: String,
    pub mode: i64,
    pub reward: i64,
    pub subsidy: i64,
    pub net_diff: f64,
    pub found_at: i64,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct AccountInfo {
    pub confirmed: i64,
    pub pending: i64,
    pub paid: i64,
}

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS share_buckets(minute INTEGER NOT NULL, account TEXT NOT NULL, mode INTEGER NOT NULL, diff REAL NOT NULL, cnt INTEGER NOT NULL, PRIMARY KEY(minute, account, mode)) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS idx_buckets_mode_minute ON share_buckets(mode, minute);
         CREATE TABLE IF NOT EXISTS blocks(hash TEXT PRIMARY KEY, height INTEGER NOT NULL, account TEXT NOT NULL, worker TEXT NOT NULL, mode INTEGER NOT NULL, reward INTEGER NOT NULL, subsidy INTEGER NOT NULL, net_diff REAL NOT NULL, found_at INTEGER NOT NULL, status TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS credits(block_hash TEXT NOT NULL, account TEXT NOT NULL, amount INTEGER NOT NULL, status TEXT NOT NULL, PRIMARY KEY(block_hash, account));
         CREATE TABLE IF NOT EXISTS balance(account TEXT PRIMARY KEY, confirmed INTEGER NOT NULL DEFAULT 0, paid INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE IF NOT EXISTS payouts(id INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, txid TEXT, status TEXT NOT NULL, total INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS payout_items(payout_id INTEGER NOT NULL, account TEXT NOT NULL, amount INTEGER NOT NULL);",
    )?;
    Ok(())
}

/// Adds aggregated shares; PPS rows are credited at `value_per_diff` leptons per unit of difficulty,
/// but never more than `cap_per_share` per share (no share may be worth more than a whole block).
pub fn add_shares(conn: &mut Connection, rows: &[ShareRow], pps: Option<(f64, i64)>) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut up = tx.prepare_cached(
            "INSERT INTO share_buckets(minute, account, mode, diff, cnt) VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(minute, account, mode) DO UPDATE SET diff = diff + excluded.diff, cnt = cnt + excluded.cnt",
        )?;
        let mut credit: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
        for r in rows {
            up.execute(params![r.minute, r.account, r.mode, r.diff, r.count as i64])?;
            if r.mode == MODE_PPS {
                if let Some((value, cap)) = pps {
                    *credit.entry(r.account.as_str()).or_default() += (r.diff * value).min(cap as f64 * r.count as f64);
                }
            }
        }
        let mut bal = tx.prepare_cached(
            "INSERT INTO balance(account, confirmed) VALUES(?1, ?2) ON CONFLICT(account) DO UPDATE SET confirmed = confirmed + excluded.confirmed",
        )?;
        for (acc, v) in credit {
            let amount = v.floor() as i64;
            if amount > 0 {
                bal.execute(params![acc, amount])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

/// Splits `distributable` over the newest PPLNS shares up to `window_diff` of difficulty.
pub fn pplns_split(conn: &Connection, window_diff: f64, distributable: i64) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare_cached("SELECT minute, account, diff FROM share_buckets WHERE mode = ?1 ORDER BY minute DESC")?;
    let mut rows = stmt.query(params![MODE_PPLNS])?;
    let mut taken: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let (mut acc_total, mut current_minute, mut group): (f64, Option<i64>, Vec<(String, f64)>) = (0.0, None, Vec::new());
    let flush = |group: &mut Vec<(String, f64)>, acc_total: &mut f64, taken: &mut std::collections::HashMap<String, f64>| -> bool {
        let t: f64 = group.iter().map(|(_, d)| *d).sum();
        if t <= 0.0 {
            group.clear();
            return false;
        }
        let room = window_diff - *acc_total;
        let frac = if t <= room { 1.0 } else { (room / t).max(0.0) };
        for (a, d) in group.drain(..) {
            *taken.entry(a).or_default() += d * frac;
        }
        *acc_total += t * frac;
        *acc_total >= window_diff
    };
    let mut done = false;
    while let Some(r) = rows.next()? {
        let (m, a, d): (i64, String, f64) = (r.get(0)?, r.get(1)?, r.get(2)?);
        if current_minute.is_some() && current_minute != Some(m) && flush(&mut group, &mut acc_total, &mut taken) {
            done = true;
            break;
        }
        current_minute = Some(m);
        group.push((a, d));
    }
    if !done {
        flush(&mut group, &mut acc_total, &mut taken);
    }
    let total: f64 = taken.values().sum();
    if total <= 0.0 {
        return Ok(Vec::new());
    }
    let mut out: Vec<(String, i64)> = taken.into_iter().map(|(a, d)| (a, (distributable as f64 * d / total).floor() as i64)).filter(|(_, v)| *v > 0).collect();
    out.sort();
    Ok(out)
}

/// Records a found block. For a PPLNS finder the reward minus fee is split over the current window as pending credits.
pub fn record_block(conn: &mut Connection, b: &BlockRec, window_factor: f64, fee_pct: f64) -> Result<Vec<(String, i64)>> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT OR IGNORE INTO blocks(hash, height, account, worker, mode, reward, subsidy, net_diff, found_at, status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'found')",
        params![b.hash, b.height, b.account, b.worker, b.mode, b.reward, b.subsidy, b.net_diff, b.found_at],
    )?;
    let mut credits = Vec::new();
    if b.mode == MODE_PPLNS {
        let distributable = (b.reward as f64 * (1.0 - fee_pct / 100.0)).floor() as i64;
        credits = pplns_split(&tx, window_factor * b.net_diff, distributable)?;
        for (acc, amount) in &credits {
            tx.execute("INSERT OR IGNORE INTO credits(block_hash, account, amount, status) VALUES(?1,?2,?3,'pending')", params![b.hash, acc, amount])?;
        }
    }
    tx.commit()?;
    Ok(credits)
}

/// Moves a block through found, maturing, mature or orphaned and settles its PPLNS credits.
pub fn set_block_status(conn: &mut Connection, hash: &str, status: &str) -> Result<()> {
    let tx = conn.transaction()?;
    let old: Option<String> = tx.query_row("SELECT status FROM blocks WHERE hash = ?1", params![hash], |r| r.get(0)).ok();
    let Some(old) = old else { return Ok(()) };
    if old == status || old == "mature" || old == "orphaned" {
        return Ok(());
    }
    tx.execute("UPDATE blocks SET status = ?2 WHERE hash = ?1", params![hash, status])?;
    if status == "mature" {
        let rows: Vec<(String, i64)> = {
            let mut st = tx.prepare("SELECT account, amount FROM credits WHERE block_hash = ?1 AND status = 'pending'")?;
            let v = st.query_map(params![hash], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
            v
        };
        for (acc, amount) in rows {
            tx.execute("INSERT INTO balance(account, confirmed) VALUES(?1, ?2) ON CONFLICT(account) DO UPDATE SET confirmed = confirmed + excluded.confirmed", params![acc, amount])?;
        }
        tx.execute("UPDATE credits SET status = 'confirmed' WHERE block_hash = ?1 AND status = 'pending'", params![hash])?;
    } else if status == "orphaned" {
        tx.execute("UPDATE credits SET status = 'cancelled' WHERE block_hash = ?1 AND status = 'pending'", params![hash])?;
    }
    tx.commit()?;
    Ok(())
}

pub fn open_blocks(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT hash FROM blocks WHERE status IN ('found','maturing')")?;
    let v = st.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    Ok(v)
}

pub fn payable(conn: &Connection, min_amount: i64) -> Result<Vec<(String, i64)>> {
    let mut st = conn.prepare("SELECT account, confirmed FROM balance WHERE confirmed >= ?1 AND confirmed > 0 ORDER BY account")?;
    let v = st.query_map(params![min_amount], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
    Ok(v)
}

pub fn liabilities(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COALESCE(SUM(confirmed), 0) FROM balance", [], |r| r.get(0))?)
}

/// Reserves the amounts for a payout (debits the confirmed balance) and returns the payout id.
pub fn begin_payout(conn: &mut Connection, items: &[(String, i64)], now: i64) -> Result<i64> {
    let tx = conn.transaction()?;
    let total: i64 = items.iter().map(|(_, a)| *a).sum();
    tx.execute("INSERT INTO payouts(created_at, status, total) VALUES(?1, 'pending', ?2)", params![now, total])?;
    let id = tx.last_insert_rowid();
    for (acc, amount) in items {
        let n = tx.execute("UPDATE balance SET confirmed = confirmed - ?2 WHERE account = ?1 AND confirmed >= ?2", params![acc, amount])?;
        if n != 1 {
            return Err(anyhow!("balance of {acc} is below the payout amount"));
        }
        tx.execute("INSERT INTO payout_items(payout_id, account, amount) VALUES(?1,?2,?3)", params![id, acc, amount])?;
    }
    tx.commit()?;
    Ok(id)
}

pub fn finish_payout(conn: &mut Connection, id: i64, txid: &str) -> Result<()> {
    let tx = conn.transaction()?;
    let n = tx.execute("UPDATE payouts SET status = 'broadcast', txid = ?2 WHERE id = ?1 AND status = 'pending'", params![id, txid])?;
    if n == 1 {
        tx.execute(
            "UPDATE balance SET paid = paid + (SELECT amount FROM payout_items WHERE payout_id = ?1 AND account = balance.account) WHERE account IN (SELECT account FROM payout_items WHERE payout_id = ?1)",
            params![id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn fail_payout(conn: &mut Connection, id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    let n = tx.execute("UPDATE payouts SET status = 'failed' WHERE id = ?1 AND status = 'pending'", params![id])?;
    if n == 1 {
        tx.execute(
            "UPDATE balance SET confirmed = confirmed + (SELECT amount FROM payout_items WHERE payout_id = ?1 AND account = balance.account) WHERE account IN (SELECT account FROM payout_items WHERE payout_id = ?1)",
            params![id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn pending_payouts(conn: &Connection) -> Result<Vec<i64>> {
    let mut st = conn.prepare("SELECT id FROM payouts WHERE status = 'pending'")?;
    let v = st.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    Ok(v)
}

pub fn account_info(conn: &Connection, account: &str) -> Result<AccountInfo> {
    let (confirmed, paid) = conn
        .query_row("SELECT confirmed, paid FROM balance WHERE account = ?1", params![account], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .unwrap_or((0, 0));
    let pending: i64 = conn.query_row("SELECT COALESCE(SUM(amount), 0) FROM credits WHERE account = ?1 AND status = 'pending'", params![account], |r| r.get(0))?;
    Ok(AccountInfo { confirmed, pending, paid })
}

pub struct BlockRow {
    pub hash: String,
    pub height: i64,
    pub account: String,
    pub worker: String,
    pub mode: i64,
    pub status: String,
    pub found_at: i64,
}

pub fn recent_blocks(conn: &Connection, limit: i64) -> Result<Vec<BlockRow>> {
    let mut st = conn.prepare("SELECT hash, height, account, worker, mode, status, found_at FROM blocks ORDER BY found_at DESC, height DESC LIMIT ?1")?;
    let v = st
        .query_map(params![limit], |r| Ok(BlockRow { hash: r.get(0)?, height: r.get(1)?, account: r.get(2)?, worker: r.get(3)?, mode: r.get(4)?, status: r.get(5)?, found_at: r.get(6)? }))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(v)
}

pub struct PayoutRow {
    pub id: i64,
    pub created_at: i64,
    pub txid: Option<String>,
    pub status: String,
    pub amount: i64,
}

pub fn recent_payouts(conn: &Connection, limit: i64) -> Result<Vec<PayoutRow>> {
    let mut st = conn.prepare("SELECT id, created_at, txid, status, total FROM payouts ORDER BY id DESC LIMIT ?1")?;
    let v = st.query_map(params![limit], |r| Ok(PayoutRow { id: r.get(0)?, created_at: r.get(1)?, txid: r.get(2)?, status: r.get(3)?, amount: r.get(4)? }))?.collect::<std::result::Result<_, _>>()?;
    Ok(v)
}

pub fn account_payouts(conn: &Connection, account: &str, limit: i64) -> Result<Vec<PayoutRow>> {
    let mut st = conn.prepare(
        "SELECT p.id, p.created_at, p.txid, p.status, i.amount FROM payouts p JOIN payout_items i ON i.payout_id = p.id WHERE i.account = ?1 AND p.status != 'failed' ORDER BY p.id DESC LIMIT ?2",
    )?;
    let v = st.query_map(params![account, limit], |r| Ok(PayoutRow { id: r.get(0)?, created_at: r.get(1)?, txid: r.get(2)?, status: r.get(3)?, amount: r.get(4)? }))?.collect::<std::result::Result<_, _>>()?;
    Ok(v)
}

/// (mode, summed difficulty, share count) of an account since the given minute.
pub fn account_shares(conn: &Connection, account: &str, since_minute: i64) -> Result<Vec<(i64, f64, i64)>> {
    let mut st = conn.prepare("SELECT mode, SUM(diff), SUM(cnt) FROM share_buckets WHERE account = ?1 AND minute >= ?2 GROUP BY mode")?;
    let v = st.query_map(params![account, since_minute], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<std::result::Result<_, _>>()?;
    Ok(v)
}

pub fn blocks_by_status(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut st = conn.prepare("SELECT status, COUNT(*) FROM blocks GROUP BY status")?;
    let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
    Ok(v)
}

pub fn blocks_since(conn: &Connection, ts: i64) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM blocks WHERE found_at >= ?1 AND status != 'orphaned'", params![ts], |r| r.get(0))?)
}

pub fn prune_shares(conn: &Connection, older_than_minute: i64) -> Result<usize> {
    Ok(conn.execute("DELETE FROM share_buckets WHERE minute < ?1", params![older_than_minute])?)
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// SQLite lives on its own thread; async code submits closures and awaits the result.
pub struct Ledger {
    tx: mpsc::Sender<Job>,
}

impl Ledger {
    pub fn open(path: &str) -> Result<Ledger> {
        let mut conn = Connection::open(path)?;
        init(&conn)?;
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new().name("ledger".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                job(&mut conn);
            }
        })?;
        Ok(Ledger { tx })
    }

    pub async fn call<T: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static) -> Result<T> {
        let (rtx, rrx) = oneshot::channel();
        self.tx.send(Box::new(move |c| {
            let _ = rtx.send(f(c));
        })).map_err(|_| anyhow!("ledger thread stopped"))?;
        rrx.await.map_err(|_| anyhow!("ledger thread dropped the request"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        c
    }

    fn shares(c: &mut Connection, rows: &[(i64, &str, i64, f64)]) {
        let v: Vec<ShareRow> = rows.iter().map(|(m, a, mode, d)| ShareRow { minute: *m, account: a.to_string(), mode: *mode, diff: *d, count: 1 }).collect();
        add_shares(c, &v, None).unwrap();
    }

    fn block(hash: &str, mode: i64, reward: i64) -> BlockRec {
        BlockRec { hash: hash.into(), height: 100, account: "finder".into(), worker: "w".into(), mode, reward, subsidy: reward, net_diff: 1000.0, found_at: 0 }
    }

    #[test]
    fn pplns_splits_proportionally_after_fee() {
        let mut c = db();
        shares(&mut c, &[(10, "a", MODE_PPLNS, 100.0), (10, "b", MODE_PPLNS, 300.0), (10, "x", MODE_PPS, 999.0), (10, "s", MODE_SOLO, 999.0)]);
        let credits = record_block(&mut c, &block("h1", MODE_PPLNS, 10_000), 10.0, 1.0).unwrap();
        assert_eq!(credits, vec![("a".to_string(), 2475), ("b".to_string(), 7425)]);
        assert!(credits.iter().map(|(_, v)| v).sum::<i64>() <= 9_900);
    }

    #[test]
    fn pplns_window_cuts_the_oldest_shares_proportionally() {
        let mut c = db();
        shares(&mut c, &[(1, "old", MODE_PPLNS, 400.0), (2, "mid", MODE_PPLNS, 100.0), (3, "new", MODE_PPLNS, 100.0)]);
        // window 150: all of "new" (100) and half of "mid" (50), nothing of "old"
        let credits = record_block(&mut c, &block("h1", MODE_PPLNS, 3_000), 0.15, 0.0).unwrap();
        assert_eq!(credits, vec![("mid".to_string(), 1000), ("new".to_string(), 2000)]);
    }

    #[test]
    fn pplns_without_shares_credits_nobody() {
        let mut c = db();
        assert!(record_block(&mut c, &block("h1", MODE_PPLNS, 1000), 2.0, 1.0).unwrap().is_empty());
    }

    #[test]
    fn solo_and_pps_blocks_create_no_credits() {
        let mut c = db();
        shares(&mut c, &[(1, "a", MODE_PPLNS, 100.0)]);
        assert!(record_block(&mut c, &block("s", MODE_SOLO, 1000), 2.0, 1.0).unwrap().is_empty());
        assert!(record_block(&mut c, &block("p", MODE_PPS, 1000), 2.0, 1.0).unwrap().is_empty());
    }

    #[test]
    fn pps_shares_accrue_to_the_confirmed_balance() {
        let mut c = db();
        let rows = vec![
            ShareRow { minute: 1, account: "a".into(), mode: MODE_PPS, diff: 10.0, count: 2 },
            ShareRow { minute: 1, account: "a".into(), mode: MODE_PPLNS, diff: 50.0, count: 1 },
            ShareRow { minute: 2, account: "a".into(), mode: MODE_PPS, diff: 5.0, count: 1 },
        ];
        add_shares(&mut c, &rows, Some((100.0, 1_000_000))).unwrap();
        assert_eq!(account_info(&c, "a").unwrap(), AccountInfo { confirmed: 1500, pending: 0, paid: 0 });
    }

    #[test]
    fn a_pps_share_is_never_worth_more_than_the_cap() {
        let mut c = db();
        let rows = vec![ShareRow { minute: 1, account: "a".into(), mode: MODE_PPS, diff: 1000.0, count: 4 }];
        add_shares(&mut c, &rows, Some((1.0e12, 500))).unwrap();
        assert_eq!(account_info(&c, "a").unwrap().confirmed, 2000);
    }

    #[test]
    fn maturity_confirms_credits_and_orphans_cancel_them() {
        let mut c = db();
        shares(&mut c, &[(1, "a", MODE_PPLNS, 1.0)]);
        record_block(&mut c, &block("good", MODE_PPLNS, 1000), 10.0, 0.0).unwrap();
        record_block(&mut c, &block("bad", MODE_PPLNS, 500), 10.0, 0.0).unwrap();
        assert_eq!(account_info(&c, "a").unwrap(), AccountInfo { confirmed: 0, pending: 1500, paid: 0 });
        set_block_status(&mut c, "good", "maturing").unwrap();
        set_block_status(&mut c, "good", "mature").unwrap();
        set_block_status(&mut c, "bad", "orphaned").unwrap();
        set_block_status(&mut c, "good", "mature").unwrap();
        assert_eq!(account_info(&c, "a").unwrap(), AccountInfo { confirmed: 1000, pending: 0, paid: 0 });
        assert!(open_blocks(&c).unwrap().is_empty());
    }

    #[test]
    fn payout_reserve_finish_and_failure() {
        let mut c = db();
        shares(&mut c, &[(1, "a", MODE_PPLNS, 3.0), (1, "b", MODE_PPLNS, 1.0)]);
        record_block(&mut c, &block("h", MODE_PPLNS, 4000), 10.0, 0.0).unwrap();
        set_block_status(&mut c, "h", "mature").unwrap();
        assert_eq!(payable(&c, 3001).unwrap(), Vec::<(String, i64)>::new());
        let items = payable(&c, 1000).unwrap();
        assert_eq!(items, vec![("a".to_string(), 3000), ("b".to_string(), 1000)]);
        let id = begin_payout(&mut c, &items, 0).unwrap();
        assert_eq!(liabilities(&c).unwrap(), 0);
        assert_eq!(pending_payouts(&c).unwrap(), vec![id]);
        fail_payout(&mut c, id).unwrap();
        assert_eq!(liabilities(&c).unwrap(), 4000);
        let id2 = begin_payout(&mut c, &items, 0).unwrap();
        finish_payout(&mut c, id2, "txid").unwrap();
        finish_payout(&mut c, id2, "txid").unwrap();
        assert_eq!(account_info(&c, "a").unwrap(), AccountInfo { confirmed: 0, pending: 0, paid: 3000 });
        assert!(pending_payouts(&c).unwrap().is_empty());
        assert!(begin_payout(&mut c, &items, 0).is_err(), "no double payout of the same balance");
    }

    #[tokio::test]
    async fn async_handle_runs_closures_on_the_ledger_thread() {
        let l = Ledger::open(":memory:").unwrap();
        l.call(|c| add_shares(c, &[ShareRow { minute: 1, account: "a".into(), mode: MODE_PPS, diff: 2.0, count: 1 }], Some((10.0, i64::MAX)))).await.unwrap();
        let info = l.call(|c| account_info(c, "a")).await.unwrap();
        assert_eq!(info.confirmed, 20);
    }
}
