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
use tower_http::services::{ServeDir, ServeFile};

#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
}

pub fn router(pool: Arc<Pool>) -> Router {
    let ui = pool.cfg.api.ui_dir.clone();
    let music = crate::music::dir(&pool.cfg.api.music_dir, &ui);
    let shell = std::path::Path::new(&ui).join("index.html");
    Router::new()
        .route("/api/v1/summary", get(summary))
        .route("/api/v1/miners", get(miners))
        .route("/api/v1/blocks", get(blocks))
        .route("/api/v1/events", get(events))
        .route("/api/v1/ledger", get(ledger_view))
        .route("/api/v1/devices", get(|State(p): State<Arc<Pool>>| async move { Json(devices_view(&p)) }))
        .route("/api/v1/history/devices", get(|State(p): State<Arc<Pool>>, Query(q): Query<RangeQ>| async move { Json(device_history_view(&p, q.range.as_deref().unwrap_or("24h")).await) }))
        .route("/api/v1/history", get(|State(p): State<Arc<Pool>>, Query(q): Query<RangeQ>| async move { Json(history_view(&p, q.range.as_deref().unwrap_or("24h")).await) }))
        .route("/api/v1/account/:address", get(account))
        .route("/music/playlist.json", get(|State(p): State<Arc<Pool>>| crate::music::playlist_response(crate::music::dir(&p.cfg.api.music_dir, &p.cfg.api.ui_dir))))
        .nest_service("/music", crate::music::files(music))
        .with_state(pool.clone())
        .merge(crate::public::data_router(pool))
        // the one-page app: every view path answers with the shell, the files are served from the ui directory (ops.js only here)
        .route_service("/engine", ServeFile::new(shell.clone()))
        .route_service("/ops", ServeFile::new(shell.clone()))
        .route_service("/miner", ServeFile::new(shell.clone()))
        .route_service("/miner.html", ServeFile::new(shell.clone()))
        .route_service("/miner/*address", ServeFile::new(shell))
        .fallback_service(ServeDir::new(ui))
}

#[derive(Deserialize)]
pub struct RangeQ {
    pub range: Option<String>,
}

/// Online devices by type (name, link, open source flag), with the number of working connections, their hash rate and the best share ever seen from that type.
pub fn devices_view(p: &Arc<Pool>) -> Value {
    use crate::devices::classify;
    let now = unix_now();
    let mut groups: std::collections::HashMap<String, (crate::devices::Device, u64, f64, f64)> = std::collections::HashMap::new();
    for s in p.sessions.lock().unwrap().values() {
        if now - get_f64(&s.last_seen) >= 120.0 {
            continue;
        }
        let dev = classify(&s.agent.lock().unwrap());
        let e = groups.entry(dev.key.clone()).or_insert((dev, 0, 0.0, 0.0));
        e.1 += 1;
        e.2 += get_f64(&s.hashrate);
        e.3 = e.3.max(get_f64(&s.best));
    }
    let bests = p.device_best.lock().unwrap().clone();
    let mut rows: Vec<Value> = groups
        .into_values()
        .map(|(d, n, hr, live_best)| {
            let best = bests.get(&d.key).copied().unwrap_or(0.0).max(live_best);
            json!({"key": d.key, "name": d.name, "url": d.url, "openSource": d.open_source, "working": n, "hashrate": hr, "best": best})
        })
        .collect();
    rows.sort_by(|a, b| b["hashrate"].as_f64().partial_cmp(&a["hashrate"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    json!({"devices": rows, "workers": rows.iter().map(|r| r["working"].as_u64().unwrap_or(0)).sum::<u64>()})
}

/// Pool history of the stored statistics: `range` is 1h, 24h, 7d, 30d or all (everything that is kept). Points are [time, hash rate, miners, ok, stale, dup, network difficulty].
pub async fn history_view(p: &Arc<Pool>, range: &str) -> Value {
    let keep = p.cfg.ledger.stats_keep_days.max(1) * 1440;
    let (minutes, slot): (i64, i64) = match range {
        "1h" => (60, 1),
        "7d" => (7 * 1440, 30),
        "30d" => (30 * 1440, 120),
        "all" => (keep, (keep / 400).max(1)),
        _ => (1440, 5),
    };
    let minutes = minutes.min(keep);
    let key = format!("{minutes}/{slot}");
    if let Some((t, v)) = p.hist_cache.lock().unwrap().get(&key) {
        if t.elapsed() < std::time::Duration::from_secs(20) {
            return v.clone();
        }
    }
    let since = unix_now() as i64 / 60 - minutes;
    let rows = p.ledger.call(move |c| crate::ledger::history(c, since, slot)).await.unwrap_or_default();
    let points: Vec<Value> = rows.into_iter().map(|(m, hr, miners, ok, stale, dup, nd)| json!([m * 60, hr, miners, ok, stale, dup, nd])).collect();
    let v = json!({"range": range, "minutes": minutes, "stepMinutes": slot, "keepDays": p.cfg.ledger.stats_keep_days, "points": points});
    p.hist_cache.lock().unwrap().insert(key, (std::time::Instant::now(), v.clone()));
    v
}

/// History per device type: for the eight types with the most hash rate in the range, `points` of `[time, hash rate, working connections]` in slots of `stepMinutes`.
pub async fn device_history_view(p: &Arc<Pool>, range: &str) -> Value {
    let keep = p.cfg.ledger.stats_keep_days.max(1) * 1440;
    let base = p.cfg.ledger.device_history_minutes.max(1);
    let (minutes, slot): (i64, i64) = match range {
        "1h" => (60, base),
        "7d" => (7 * 1440, 30.max(base)),
        "30d" => (30 * 1440, 120.max(base)),
        "all" => (keep, (keep / 400).max(base)),
        _ => (1440, 5.max(base)),
    };
    let minutes = minutes.min(keep);
    let key = format!("devices/{minutes}/{slot}");
    if let Some((t, v)) = p.hist_cache.lock().unwrap().get(&key) {
        if t.elapsed() < std::time::Duration::from_secs(20) {
            return v.clone();
        }
    }
    let since = unix_now() as i64 / 60 - minutes;
    let rows = p.ledger.call(move |c| crate::ledger::device_history(c, since, slot)).await.unwrap_or_default();
    let mut by: std::collections::HashMap<String, (String, f64, Vec<Value>)> = std::collections::HashMap::new();
    for (m, dev, name, hr, workers) in rows {
        let e = by.entry(dev).or_insert((name, 0.0, Vec::new()));
        e.1 += hr;
        e.2.push(json!([m * 60, hr, workers]));
    }
    let mut list: Vec<(String, (String, f64, Vec<Value>))> = by.into_iter().collect();
    list.sort_by(|a, b| b.1 .1.partial_cmp(&a.1 .1).unwrap_or(std::cmp::Ordering::Equal));
    list.truncate(8);
    let devices: Vec<Value> = list.into_iter().map(|(k, (name, _, pts))| json!({"key": k, "name": name, "points": pts})).collect();
    let v = json!({"range": range, "minutes": minutes, "stepMinutes": slot, "keepDays": p.cfg.ledger.stats_keep_days, "devices": devices});
    p.hist_cache.lock().unwrap().insert(key, (std::time::Instant::now(), v.clone()));
    v
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() as f64 * q) as usize).min(sorted.len() - 1)]
}

async fn summary(State(p): State<Arc<Pool>>) -> Json<Value> {
    Json(summary_value(&p, false))
}

/// The pool summary for the operator (`public = false`) or for the public page (`public = true`: the same numbers, but nothing about the Photon listener).
pub fn summary_value(p: &Arc<Pool>, public: bool) -> Value {
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
        .map(|m| json!({"mode": m.as_str(), "fee": p.cfg.payout.fee(*m), "available": p.mode_available(*m).is_ok(), "online": live.by_mode[*m as usize].0, "hashrate": live.by_mode[*m as usize].1}))
        .collect();
    let mut v = json!({
        "pool": {"name": "photon", "version": env!("CARGO_PKG_VERSION"), "uptime": p.started.elapsed().as_secs_f64(), "mode": "core", "explorerUrl": p.cfg.api.explorer_url},
        "node": {
            "height": height, "tip": tip, "tipTime": tip_time, "connected": s.connected.load(Ordering::Relaxed), "transport": "http-rpc",
            "rttMs": p.node.last_rtt_us.load(Ordering::Relaxed) as f64 / 1000.0,
            "p50": quantile(&lat, 0.5), "p95": quantile(&lat, 0.95), "samples": lat.len(),
            "batchMs": s.last_fetch_ms.load(Ordering::Relaxed), "backend": *p.backend_name.lock().unwrap(), "candidatesAvailable": p.payouts.len(), "demandUnitsPerSec": get_f64(&p.demand_units),
        },
        "hashrate": {"now": live.hashrate, "m1": live.m1, "h1": live.h1, "hist": if live.hist.is_empty() { vec![0.0] } else { live.hist.iter().copied().collect() }},
        "miners": {"online": live.online, "total": live.total, "v1": live.total, "v2": 0, "edge": 0},
        "shares": {"perSec": live.shares_per_sec, "ok": frac(ok), "stale": frac(stale), "dup": frac(dup), "inv": frac(inv), "total": total},
        "alloc": {"candidates": cands, "unitsPerSec": live.units_per_sec, "epochUse": used as f64 / total_units as f64, "overlap": overlap},
        "net": {"connections": live.total, "inBps": live.in_bps, "outBps": live.out_bps, "jobsPerSec": live.jobs_per_sec, "connectsPerSec": live.connects_per_sec,
                "accepted": crate::stratum::NET.accepted.load(Ordering::Relaxed), "closed": crate::stratum::NET.closed.load(Ordering::Relaxed), "refused": crate::stratum::NET.refused.load(Ordering::Relaxed),
                "bytesIn": crate::stratum::NET.bytes_in.load(Ordering::Relaxed), "bytesOut": crate::stratum::NET.bytes_out.load(Ordering::Relaxed)},
        "found": s.found.lock().unwrap().len(),
        "modes": modes,
        "photon": p.photon.get().map(|ph| {
            let st = &ph.stats;
            let g = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
            json!({"enabled": true, "listen": ph.cfg.listen, "publicKey": ph.public_key_hex(), "connections": g(&st.connections), "channels": g(&st.channels), "sharesOk": g(&st.accepted), "sharesRejected": g(&st.rejected),
                   "announces": g(&st.announces), "checksOk": g(&st.checks_ok), "checksFailed": g(&st.checks_failed), "checksInconclusive": g(&st.checks_inconclusive), "blocks": g(&st.blocks),
                   "checkTotalUs": g(&st.check_total_us), "checkMaxUs": g(&st.check_max_us), "checkNodeUs": g(&st.check_node_us), "checkBytes": g(&st.check_bytes), "checkTimed": g(&st.check_timed), "checksSkipped": g(&st.checks_skipped), "trustEntries": ph.trust.lock().unwrap().len(), "sumP": ph.sum_p()})
        }).unwrap_or_else(|| json!({"enabled": false})),
    });
    if public {
        v["photon"] = json!({"enabled": p.photon.get().is_some()});
    }
    v
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
                "agent": s.agent.lock().unwrap().clone(), "device": crate::devices::classify(&s.agent.lock().unwrap()).name, "best": get_f64(&s.best),
                "unit": format!("c{:03}/t{:02}/s{:02}", unit >> 16, (unit >> 8) & 0xff, unit & 0xff), "jobMs": get_f64(&s.job_ms),
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
                "device": crate::devices::classify(&s.agent.lock().unwrap()).name, "best": get_f64(&s.best),
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
