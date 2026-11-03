use crate::ledger;
use crate::pool::{get_f64, unix_now, Pool};
use axum::extract::{Query, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::services::ServeDir;

#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
}

pub fn router(pool: Arc<Pool>) -> Router {
    let ui = pool.cfg.api.ui_dir.clone();
    Router::new()
        .route("/api/v1/summary", get(summary))
        .route("/api/v1/miners", get(miners))
        .route("/api/v1/blocks", get(blocks))
        .route("/api/v1/events", get(events))
        .route("/api/v1/ledger", get(ledger_view))
        .route("/api/v1/account/:address", get(account))
        .fallback_service(ServeDir::new(ui))
        .with_state(pool)
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() as f64 * q) as usize).min(sorted.len() - 1)]
}

async fn summary(State(p): State<Arc<Pool>>) -> Json<Value> {
    let epoch = p.epoch.borrow().clone();
    let mut lat: Vec<f64> = p.stats.tip_ready_ms.lock().unwrap().iter().copied().collect();
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let s = &p.stats;
    let live = p.live.lock().unwrap();
    let (ok, stale, dup, inv) = (s.ok.load(Ordering::Relaxed), s.stale.load(Ordering::Relaxed), s.dup.load(Ordering::Relaxed), s.inv.load(Ordering::Relaxed));
    let total = ok + stale + dup + inv;
    let frac = |n: u64| if total == 0 { 0.0 } else { n as f64 / total as f64 };
    let (height, tip, tip_time, cands, used, total_units, overlap) = match &epoch {
        Some(e) => (e.height, e.tip_hex.clone(), e.started_unix, e.candidates.len(), e.units_used(), e.units_total(), e.overlap.load(Ordering::Relaxed)),
        None => (0, String::new(), unix_now(), 0, 0, 1, 0),
    };
    let modes: Vec<Value> = [crate::config::Mode::Solo, crate::config::Mode::Pplns, crate::config::Mode::Pps]
        .iter()
        .filter(|m| p.cfg.payout.offers(**m))
        .map(|m| json!({"mode": m.as_str(), "fee": p.cfg.payout.fee(*m), "available": p.mode_available(*m).is_ok()}))
        .collect();
    Json(json!({
        "pool": {"name": "photon", "version": env!("CARGO_PKG_VERSION"), "uptime": p.started.elapsed().as_secs_f64(), "mode": "core", "explorerUrl": p.cfg.api.explorer_url},
        "node": {
            "height": height, "tip": tip, "tipTime": tip_time, "connected": s.connected.load(Ordering::Relaxed), "transport": "http-rpc",
            "rttMs": p.node.last_rtt_us.load(Ordering::Relaxed) as f64 / 1000.0,
            "p50": quantile(&lat, 0.5), "p95": quantile(&lat, 0.95), "samples": lat.len(),
            "batchMs": s.last_fetch_ms.load(Ordering::Relaxed), "backend": *p.backend_name.lock().unwrap(),
        },
        "hashrate": {"now": live.hashrate, "m1": live.m1, "h1": live.h1, "hist": if live.hist.is_empty() { vec![0.0] } else { live.hist.iter().copied().collect() }},
        "miners": {"online": live.online, "total": live.total, "v1": live.total, "v2": 0, "edge": 0},
        "shares": {"perSec": live.shares_per_sec, "ok": frac(ok), "stale": frac(stale), "dup": frac(dup), "inv": frac(inv), "total": total},
        "alloc": {"candidates": cands, "unitsPerSec": live.units_per_sec, "epochUse": used as f64 / total_units as f64, "overlap": overlap},
        "found": s.found.lock().unwrap().len(),
        "modes": modes,
    }))
}

async fn miners(State(p): State<Arc<Pool>>, Query(q): Query<Limit>) -> Json<Value> {
    let now = unix_now();
    let mut rows: Vec<Value> = p
        .sessions
        .lock()
        .unwrap()
        .values()
        .map(|s| {
            let unit = s.unit.load(Ordering::Relaxed);
            json!({
                "name": s.name.lock().unwrap().clone(), "proto": "V1", "hashrate": get_f64(&s.hashrate), "diff": get_f64(&s.difficulty),
                "ok": s.ok.load(Ordering::Relaxed), "stale": s.stale.load(Ordering::Relaxed),
                "dup": s.dup.load(Ordering::Relaxed) + s.inv.load(Ordering::Relaxed),
                "seen": (now - get_f64(&s.last_seen)).max(0.0) as u64,
                "unit": format!("c{:03}/t{:02}/s{:02}", unit >> 16, (unit >> 8) & 0xff, unit & 0xff),
            })
        })
        .collect();
    rows.sort_by(|a, b| b["hashrate"].as_f64().partial_cmp(&a["hashrate"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(q.limit.unwrap_or(100).min(1000));
    Json(Value::Array(rows))
}

async fn blocks(State(p): State<Arc<Pool>>, Query(q): Query<Limit>) -> Json<Value> {
    Json(blocks_view(&p, q.limit.unwrap_or(12).min(200) as i64).await)
}

pub fn mode_name(mode: i64) -> &'static str {
    match mode {
        0 => "solo",
        1 => "pplns",
        _ => "pps",
    }
}

pub async fn blocks_view(p: &Arc<Pool>, n: i64) -> Value {
    let rows = p.ledger.call(move |c| ledger::recent_blocks(c, n)).await.unwrap_or_default();
    let base = &p.cfg.api.explorer_url;
    Value::Array(
        rows.into_iter()
            .map(|b| {
                let mut v = json!({"height": b.height, "hash": b.hash, "time": b.found_at, "worker": b.worker, "mode": mode_name(b.mode), "status": b.status});
                if let Some(u) = block_url(base, &b.hash) {
                    v["url"] = json!(u);
                }
                v
            })
            .collect(),
    )
}

/// Balances in leptons, shares as summed difficulty; `None` for an address that does not belong to the configured network.
pub async fn account_view(p: &Arc<Pool>, address: &str) -> Option<Value> {
    crate::addr::script_for_address(address, p.cfg.network().ok()?.hrp).ok()?;
    let now_min = crate::accounting::minute_now();
    let addr = address.to_string();
    let (info, day, hour, payouts, credits, found, history) = p
        .ledger
        .call(move |c| {
            Ok((
                ledger::account_info(c, &addr)?,
                ledger::account_shares(c, &addr, now_min - 1440)?,
                ledger::account_shares(c, &addr, now_min - 60)?,
                ledger::account_payouts(c, &addr, 20)?,
                ledger::account_credits(c, &addr, 30)?,
                ledger::account_blocks(c, &addr, 20)?,
                ledger::account_history(c, &addr, now_min - 1440, 10)?,
            ))
        })
        .await
        .ok()?;
    let tip_height = p.epoch.borrow().as_ref().map(|e| e.height as i64).unwrap_or(0);
    let prefix = format!("{address}.");
    let (mut hashrate, mut workers) = (0.0, Vec::new());
    for s in p.sessions.lock().unwrap().values() {
        let name = s.name.lock().unwrap().clone();
        if let Some(w) = name.strip_prefix(&prefix) {
            let hr = get_f64(&s.hashrate);
            hashrate += hr;
            workers.push(json!({
                "name": w, "mode": mode_name(s.mode.load(Ordering::Relaxed) as i64), "hashrate": hr, "difficulty": get_f64(&s.difficulty),
                "accepted": s.ok.load(Ordering::Relaxed), "stale": s.stale.load(Ordering::Relaxed),
                "invalid": s.inv.load(Ordering::Relaxed) + s.dup.load(Ordering::Relaxed), "seen": (unix_now() - get_f64(&s.last_seen)).max(0.0) as u64,
            }));
        }
    }
    workers.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let shares = |rows: Vec<(i64, f64, i64)>| {
        let mut m = serde_json::Map::new();
        for (mode, diff, cnt) in rows {
            m.insert(mode_name(mode).into(), json!({"difficulty": diff, "count": cnt}));
        }
        Value::Object(m)
    };
    let base = &p.cfg.api.explorer_url;
    let maturity = crate::accounting::MATURITY_CONFIRMATIONS;
    Some(json!({
        "address": address, "hashrate": hashrate, "workers": workers.len(), "workerList": workers,
        "shares1h": shares(hour), "shares24h": shares(day),
        "balance": {"pending": info.pending, "confirmed": info.confirmed, "paid": info.paid},
        "minPayoutLeptons": p.cfg.payout.min_payout_leptons,
        "payouts": payouts.into_iter().map(|r| json!({"time": r.created_at, "txid": r.txid, "amount": r.amount, "status": r.status})).collect::<Vec<_>>(),
        "credits": credits.into_iter().map(|c| {
            let remaining = if c.status == "pending" { (maturity - (tip_height - c.height + 1)).max(0) } else { 0 };
            let mut v = json!({"height": c.height, "hash": c.hash, "amount": c.amount, "status": c.status, "blockStatus": c.block_status, "maturesIn": remaining});
            if let Some(u) = block_url(base, &c.hash) { v["url"] = json!(u); }
            v
        }).collect::<Vec<_>>(),
        "blocksFound": found.into_iter().map(|b| {
            let mut v = json!({"height": b.height, "hash": b.hash, "time": b.found_at, "mode": mode_name(b.mode), "status": b.status});
            if let Some(u) = block_url(base, &b.hash) { v["url"] = json!(u); }
            v
        }).collect::<Vec<_>>(),
        "history": history.into_iter().map(|(m, d)| json!({"time": m * 60, "hashrate": d * 4_294_967_296.0 / 600.0})).collect::<Vec<_>>(),
    }))
}

async fn account(State(p): State<Arc<Pool>>, axum::extract::Path(address): axum::extract::Path<String>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match account_view(&p, &address).await {
        Some(v) => Json(v).into_response(),
        None => (axum::http::StatusCode::BAD_REQUEST, "invalid address").into_response(),
    }
}

async fn ledger_view(State(p): State<Arc<Pool>>) -> Json<Value> {
    let (liabilities, payable, payouts, by_status) = p
        .ledger
        .call(|c| Ok((ledger::liabilities(c)?, ledger::payable(c, 1)?.len(), ledger::recent_payouts(c, 10)?, ledger::blocks_by_status(c)?)))
        .await
        .unwrap_or_default();
    Json(json!({
        "dryRun": p.cfg.payout.dry_run, "wallet": p.wallet.is_some(), "ppsReady": p.pps_ready.load(Ordering::Relaxed),
        "liabilities": liabilities, "treasuryFree": p.treasury_free.load(Ordering::Relaxed), "payableAccounts": payable,
        "blocks": by_status.into_iter().collect::<std::collections::HashMap<_, _>>(),
        "payouts": payouts.into_iter().map(|r| json!({"id": r.id, "time": r.created_at, "txid": r.txid, "amount": r.amount, "status": r.status})).collect::<Vec<_>>(),
    }))
}

/// Link to a block page of a mempool-style explorer; `None` when no explorer is configured.
pub fn block_url(base: &str, hash: &str) -> Option<String> {
    if base.is_empty() || hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{base}/block/{hash}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_links() {
        let h = "ab".repeat(32);
        assert_eq!(block_url("https://mempool.example", &h), Some(format!("https://mempool.example/block/{h}")));
        assert_eq!(block_url("", &h), None);
        assert_eq!(block_url("https://mempool.example", "not-a-hash"), None);
    }
}

async fn events(State(p): State<Arc<Pool>>) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let stream = BroadcastStream::new(p.events.subscribe()).filter_map(|r| async move { r.ok().map(|e| Ok(SseEvent::default().data(serde_json::to_string(&e).unwrap_or_default()))) });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
