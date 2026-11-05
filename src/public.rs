use crate::api::{account_view, block_url, mode_name};
use crate::config::Mode;
use crate::diff::difficulty_from_target;
use crate::ledger;
use crate::pool::{unix_now, Pool};
use anyhow::Result;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; media-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Token bucket per client address.
pub struct Limiter {
    buckets: Mutex<HashMap<IpAddr, (f64, Instant)>>,
    capacity: f64,
    per_sec: f64,
}

impl Limiter {
    pub fn new(per_min: u32) -> Self {
        let capacity = per_min.max(1) as f64;
        Self { buckets: Mutex::new(HashMap::new()), capacity, per_sec: capacity / 60.0 }
    }

    pub fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut m = self.buckets.lock().unwrap();
        if m.len() > 50_000 {
            m.retain(|_, (_, t)| now.duration_since(*t) < Duration::from_secs(600));
        }
        let e = m.entry(ip).or_insert((self.capacity, now));
        e.0 = (e.0 + now.duration_since(e.1).as_secs_f64() * self.per_sec).min(self.capacity);
        e.1 = now;
        if e.0 >= 1.0 {
            e.0 -= 1.0;
            true
        } else {
            false
        }
    }
}

pub fn client_ip(peer: IpAddr, headers: &HeaderMap, trust_proxy: bool) -> IpAddr {
    if trust_proxy {
        if let Some(ip) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).and_then(|v| v.split(',').next()).and_then(|v| v.trim().parse().ok()) {
            return ip;
        }
    }
    peer
}

#[derive(Clone)]
struct St {
    pool: Arc<Pool>,
    limiter: Arc<Limiter>,
}

#[derive(Deserialize)]
struct Limit {
    limit: Option<usize>,
}

fn data_routes() -> Router<St> {
    Router::new()
        .route("/public/v1/pool", get(pool_view))
        .route("/public/v1/blocks", get(blocks))
        .route("/public/v1/payouts", get(payouts))
        .route("/public/v1/devices", get(|State(s): State<St>| async move { Json(crate::api::devices_view(&s.pool)) }))
        .route("/public/v1/history/devices", get(|State(s): State<St>, Query(q): Query<crate::api::RangeQ>| async move { Json(crate::api::device_history_view(&s.pool, q.range.as_deref().unwrap_or("24h")).await) }))
        .route("/public/v1/history", get(|State(s): State<St>, Query(q): Query<crate::api::RangeQ>| async move { Json(crate::api::history_view(&s.pool, q.range.as_deref().unwrap_or("24h")).await) }))
        .route("/public/v1/account/:address", get(account))
        .route("/public/v1/engine", get(|State(s): State<St>| async move { Json(crate::api::summary_value(&s.pool, true)) }))
        .route("/public/v1/events", get(public_events))
}

/// The data API of the public page without the page itself and without the rate limit, for the operator listener (which then serves the same pages).
pub fn data_router(pool: Arc<Pool>) -> Router {
    let st = St { limiter: Arc::new(Limiter::new(u32::MAX)), pool };
    data_routes().with_state(st)
}

#[derive(Deserialize)]
struct SinceQ {
    since: Option<u64>,
}

/// Events that are fit for the public page, newer than `since` (sequence number), at most 100.
async fn public_events(State(st): State<St>, Query(q): Query<SinceQ>) -> Json<Value> {
    let since = q.since.unwrap_or(0);
    let q = st.pool.public_events.lock().unwrap();
    let events: Vec<Value> = q.iter().filter(|e| e.0 > since).rev().take(100).collect::<Vec<_>>().into_iter().rev().map(|(seq, t, lvl, msg)| json!({"seq": seq, "t": t, "lvl": lvl, "msg": msg})).collect();
    let last = q.back().map(|e| e.0).unwrap_or(0);
    Json(json!({"events": events, "last": last}))
}

pub fn router(pool: Arc<Pool>) -> Router {
    let st = St { limiter: Arc::new(Limiter::new(pool.cfg.public.rate_per_min)), pool: pool.clone() };
    Router::new()
        .merge(data_routes())
        .route("/", get(|State(s): State<St>| file(s, "index.html", "text/html; charset=utf-8")))
        .route("/style.css", get(|State(s): State<St>| file(s, "style.css", "text/css; charset=utf-8")))
        .route("/common.js", get(|State(s): State<St>| file(s, "common.js", "text/javascript; charset=utf-8")))
        .route("/music.js", get(|State(s): State<St>| file(s, "music.js", "text/javascript; charset=utf-8")))
        .route("/charts.js", get(|State(s): State<St>| file(s, "charts.js", "text/javascript; charset=utf-8")))
        .route("/depth.js", get(|State(s): State<St>| file(s, "depth.js", "text/javascript; charset=utf-8")))
        .route("/public.js", get(|State(s): State<St>| file(s, "public.js", "text/javascript; charset=utf-8")))
        .route("/engine", get(|State(s): State<St>| file(s, "index.html", "text/html; charset=utf-8")))
        .route("/ops", get(|State(s): State<St>| file(s, "index.html", "text/html; charset=utf-8")))
        .route("/engine.js", get(|State(s): State<St>| file(s, "engine.js", "text/javascript; charset=utf-8")))
        .route("/miner.html", get(|State(s): State<St>| file(s, "index.html", "text/html; charset=utf-8")))
        .route("/miner", get(|State(s): State<St>| file(s, "index.html", "text/html; charset=utf-8")))
        .route("/miner/*address", get(|State(s): State<St>| file(s, "index.html", "text/html; charset=utf-8")))
        .route("/stats.js", get(|State(s): State<St>| file(s, "stats.js", "text/javascript; charset=utf-8")))
        .route("/spa.js", get(|State(s): State<St>| file(s, "spa.js", "text/javascript; charset=utf-8")))
        .route("/miner.js", get(|State(s): State<St>| file(s, "miner.js", "text/javascript; charset=utf-8")))
        .route("/music/playlist.json", get(|State(s): State<St>| crate::music::playlist_response(crate::music::dir(&s.pool.cfg.api.music_dir, &s.pool.cfg.api.ui_dir))))
        .nest_service("/music", crate::music::files(crate::music::dir(&pool.cfg.api.music_dir, &pool.cfg.api.ui_dir)))
        .layer(middleware::from_fn_with_state(st.clone(), guard))
        .with_state(st)
}

pub async fn serve(pool: Arc<Pool>) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&pool.cfg.public.listen).await?;
    pool.event("info", format!("public pool page on {}", pool.cfg.public.listen));
    axum::serve(listener, router(pool).into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

async fn guard(State(st): State<St>, ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request, next: Next) -> Response {
    let ip = client_ip(peer.ip(), req.headers(), st.pool.cfg.public.trust_proxy);
    // only the data API is rate limited: a page view fetches about ten static files, and the music streams in ranges
    let limited = req.uri().path().starts_with("/public/v1/");
    let mut resp = if !limited || st.limiter.allow(ip) {
        next.run(req).await
    } else {
        let mut r = (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
        r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
        r
    };
    let h = resp.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("public, max-age=5"));
    resp
}

async fn file(st: St, name: &str, ctype: &'static str) -> Response {
    match tokio::fs::read(std::path::Path::new(&st.pool.cfg.api.ui_dir).join(name)).await {
        Ok(b) => ([(header::CONTENT_TYPE, ctype)], b).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn mask(a: &str) -> String {
    if a.len() > 14 {
        format!("{}..{}", &a[..6], &a[a.len() - 4..])
    } else {
        a.to_string()
    }
}

fn port_of(listen: &str) -> u64 {
    listen.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(0)
}

async fn pool_view(State(st): State<St>) -> Json<Value> {
    let p = &st.pool;
    let epoch = p.epoch.borrow().clone();
    let (height, tip, tip_time, net_diff) = match &epoch {
        Some(e) => (e.height, e.tip_hex.clone(), e.started_unix, e.candidates.first().map(|c| difficulty_from_target(&c.target)).unwrap_or(0.0)),
        None => (0, String::new(), unix_now(), 0.0),
    };
    let day_ago = unix_now() as i64 - 86_400;
    let (found24h, by_status) = p.ledger.call(move |c| Ok((ledger::blocks_since(c, day_ago)?, ledger::blocks_by_status(c)?))).await.unwrap_or_default();
    let (hashrate, workers, online) = {
        let l = p.live.lock().unwrap();
        (json!({"now": l.hashrate, "m1": l.m1, "h1": l.h1, "hist": l.hist.iter().copied().collect::<Vec<_>>()}), l.total, l.online)
    };
    let modes: Vec<Value> = [Mode::Solo, Mode::Pplns, Mode::Pps]
        .iter()
        .filter(|m| p.cfg.payout.offers(**m))
        .map(|m| json!({"mode": m.as_str(), "fee": p.cfg.payout.fee(*m), "available": p.mode_available(*m).is_ok()}))
        .collect();
    let ports: Vec<Value> = p.cfg.stratum_ports().iter().map(|x| json!({"port": port_of(&x.listen), "mode": x.mode.as_str(), "override": x.allow_override})).collect();
    Json(json!({
        "name": p.cfg.public.name, "version": env!("CARGO_PKG_VERSION"), "network": p.cfg.pool.network, "explorerUrl": p.cfg.api.explorer_url,
        "uptime": p.started.elapsed().as_secs_f64(),
        "stratum": {"host": p.cfg.public.stratum_host, "ports": ports},
        "modes": modes, "minPayoutLeptons": p.cfg.payout.min_payout_leptons, "pplnsWindowFactor": p.cfg.payout.pplns.window_factor,
        "hashrate": hashrate, "miners": {"online": online, "workers": workers},
        "found24h": found24h, "blocks": by_status.into_iter().collect::<HashMap<_, _>>(),
        "node": {"height": height, "tip": tip, "tipTime": tip_time}, "networkDifficulty": net_diff,
        "connected": p.stats.connected.load(Ordering::Relaxed),
    }))
}

async fn blocks(State(st): State<St>, Query(q): Query<Limit>) -> Json<Value> {
    let p = &st.pool;
    let n = q.limit.unwrap_or(12).min(50) as i64;
    let rows = p.ledger.call(move |c| ledger::recent_blocks(c, n)).await.unwrap_or_default();
    let base = &p.cfg.api.explorer_url;
    Json(Value::Array(
        rows.into_iter()
            .map(|b| {
                let mut v = json!({"height": b.height, "hash": b.hash, "time": b.found_at, "mode": mode_name(b.mode), "status": b.status, "finder": mask(&b.account)});
                if let Some(u) = block_url(base, &b.hash) {
                    v["url"] = json!(u);
                }
                v
            })
            .collect(),
    ))
}

async fn payouts(State(st): State<St>, Query(q): Query<Limit>) -> Json<Value> {
    let n = q.limit.unwrap_or(10).min(50) as i64;
    let rows = st.pool.ledger.call(move |c| ledger::recent_payouts(c, n)).await.unwrap_or_default();
    Json(Value::Array(rows.into_iter().filter(|r| r.txid.is_some()).map(|r| json!({"time": r.created_at, "txid": r.txid, "amount": r.amount, "status": r.status})).collect()))
}

async fn account(State(st): State<St>, Path(address): Path<String>) -> Response {
    match account_view(&st.pool, &address).await {
        Some(v) => Json(v).into_response(),
        None => (StatusCode::BAD_REQUEST, "invalid address").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_allows_the_burst_then_blocks() {
        let l = Limiter::new(5);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert_eq!((0..8).filter(|_| l.allow(ip)).count(), 5);
        assert!(l.allow("10.0.0.2".parse().unwrap()), "other clients have their own bucket");
    }

    #[test]
    fn forwarded_address_is_only_used_behind_a_trusted_proxy() {
        let peer: IpAddr = "10.0.0.9".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.7, 10.0.0.9"));
        assert_eq!(client_ip(peer, &h, false), peer);
        assert_eq!(client_ip(peer, &h, true), "203.0.113.7".parse::<IpAddr>().unwrap());
        h.insert("x-forwarded-for", HeaderValue::from_static("garbage"));
        assert_eq!(client_ip(peer, &h, true), peer);
    }

    #[test]
    fn addresses_are_masked() {
        assert_eq!(mask("bcrt1qabcdefghijklmnopqrstuvwxyz"), "bcrt1q..wxyz");
        assert_eq!(mask("short"), "short");
    }
}
