use crate::pool::{unix_now, Pool};
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
    let (ok, stale, dup, inv) = (s.ok.load(Ordering::Relaxed), s.stale.load(Ordering::Relaxed), s.dup.load(Ordering::Relaxed), s.inv.load(Ordering::Relaxed));
    let total = ok + stale + dup + inv;
    let frac = |n: u64| if total == 0 { 0.0 } else { n as f64 / total as f64 };
    let (height, tip, tip_time, cands, used, total_units, overlap) = match &epoch {
        Some(e) => (e.height, e.tip_hex.clone(), e.started_unix, e.candidates.len(), e.units_used(), e.units_total(), e.overlap.load(Ordering::Relaxed)),
        None => (0, String::new(), unix_now(), 0, 0, 1, 0),
    };
    Json(json!({
        "pool": {"name": "photon", "version": env!("CARGO_PKG_VERSION"), "uptime": p.started.elapsed().as_secs_f64(), "mode": "core"},
        "node": {
            "height": height, "tip": tip, "tipTime": tip_time, "connected": s.connected.load(Ordering::Relaxed), "transport": "http-rpc",
            "rttMs": p.node.last_rtt_us.load(Ordering::Relaxed) as f64 / 1000.0,
            "p50": quantile(&lat, 0.5), "p95": quantile(&lat, 0.95), "samples": lat.len(),
            "batchMs": s.last_fetch_ms.load(Ordering::Relaxed),
        },
        "hashrate": {"now": 0, "m1": 0, "h1": 0, "hist": [0]},
        "miners": {"online": 0, "total": 0, "v1": 0, "v2": 0, "edge": 0},
        "shares": {"perSec": 0, "ok": frac(ok), "stale": frac(stale), "dup": frac(dup), "inv": frac(inv), "total": total},
        "alloc": {"candidates": cands, "unitsPerSec": 0, "epochUse": used as f64 / total_units as f64, "overlap": overlap},
        "found": s.found.lock().unwrap().len(),
    }))
}

async fn miners() -> Json<Value> {
    Json(json!([]))
}

async fn blocks(State(p): State<Arc<Pool>>, Query(q): Query<Limit>) -> Json<Value> {
    let found = p.stats.found.lock().unwrap();
    let n = q.limit.unwrap_or(12).min(200);
    Json(json!(found.iter().rev().take(n).collect::<Vec<_>>()))
}

async fn events(State(p): State<Arc<Pool>>) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let stream = BroadcastStream::new(p.events.subscribe()).filter_map(|r| async move { r.ok().map(|e| Ok(SseEvent::default().data(serde_json::to_string(&e).unwrap_or_default()))) });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
