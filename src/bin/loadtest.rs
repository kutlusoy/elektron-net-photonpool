use clap::Parser;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Synthetic Stratum V1 load generator: many idle miners, tip fan-out latency, optional invalid-share traffic.
#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:3333")]
    addr: String,
    #[arg(long, default_value_t = 1000)]
    conns: usize,
    #[arg(long)]
    account: String,
    #[arg(long, default_value_t = 2000)]
    ramp_per_sec: u64,
    /// Share submissions per connection and second (answered with low-difficulty errors).
    #[arg(long, default_value_t = 0.0)]
    share_rate: f64,
    #[arg(long, default_value_t = 20)]
    wait_secs: u64,
    /// User agent sent with mining.subscribe; a Bitaxe agent such as "bitaxe/BM1370/v2.9.0" gets a clean job every few hundred milliseconds.
    #[arg(long, default_value = "loadtest")]
    agent: String,
    /// After the tip fan-out keep counting jobs and bytes for this many seconds (steady-state load).
    #[arg(long, default_value_t = 0)]
    steady_secs: u64,
    /// Send an extranonce2 of 4 bytes with every share (a miner in extranonce mode, for example agent "bmminer/2.0.0").
    #[arg(long, default_value_t = false)]
    en2: bool,
}

#[derive(Default)]
struct Shared {
    connected: AtomicU64,
    failed: AtomicU64,
    notifies: AtomicU64,
    bytes: AtomicU64,
    responses: AtomicU64,
    go: AtomicBool,
    t_go_ns: AtomicU64,
    recv_ns: Mutex<Vec<u64>>,
}

fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

async fn conn(i: usize, a: Arc<Args>, sh: Arc<Shared>) {
    let Ok(stream) = TcpStream::connect(&a.addr).await else {
        sh.failed.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let _ = stream.set_nodelay(true);
    let (rd, mut wr) = stream.into_split();
    let hello = format!(
        "{{\"id\":1,\"method\":\"mining.configure\",\"params\":[[\"version-rolling\"],{{\"version-rolling.mask\":\"ffffffff\"}}]}}\n{{\"id\":2,\"method\":\"mining.subscribe\",\"params\":[\"{}\"]}}\n{{\"id\":3,\"method\":\"mining.authorize\",\"params\":[\"{}.lt{i}\",\"x\"]}}\n",
        a.agent, a.account
    );
    if wr.write_all(hello.as_bytes()).await.is_err() {
        sh.failed.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let mut rd = BufReader::new(rd);
    let mut line = String::new();
    let (mut up, mut got_clean, mut job_id, mut seq) = (false, false, String::new(), 0u32);
    let period = if a.share_rate > 0.0 { Duration::from_secs_f64(1.0 / a.share_rate) } else { Duration::from_secs(3600) };
    let mut tick = tokio::time::interval(period);
    loop {
        line.clear();
        tokio::select! {
            r = rd.read_line(&mut line) => {
                match r {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                sh.bytes.fetch_add(line.len() as u64, Ordering::Relaxed);
                if line.contains("\"mining.notify\"") {
                    sh.notifies.fetch_add(1, Ordering::Relaxed);
                    if let Some(s) = line.split("\"params\":[\"").nth(1) {
                        job_id = s.split('"').next().unwrap_or("").to_string();
                    }
                    if !up {
                        up = true;
                        sh.connected.fetch_add(1, Ordering::Relaxed);
                    }
                    if sh.go.load(Ordering::Relaxed) && !got_clean && line.trim_end().ends_with("true]}") {
                        got_clean = true;
                        sh.recv_ns.lock().unwrap().push(now_ns());
                    }
                } else if line.contains("\"result\"") {
                    sh.responses.fetch_add(1, Ordering::Relaxed);
                }
            }
            _ = tick.tick(), if up && a.share_rate > 0.0 => {
                seq += 1;
                let m = format!("{{\"id\":{},\"method\":\"mining.submit\",\"params\":[\"{}.lt{i}\",\"{job_id}\",\"{}\",\"{:08x}\",\"{:08x}\",\"00000000\"]}}\n", 100 + seq, a.account, if a.en2 { "0000002a" } else { "" }, 0x6000_0000u32, seq);
                if wr.write_all(m.as_bytes()).await.is_err() { break; }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let a = Arc::new(Args::parse());
    let sh = Arc::new(Shared::default());
    let t0 = Instant::now();
    for i in 0..a.conns {
        tokio::spawn(conn(i, a.clone(), sh.clone()));
        if i as u64 % a.ramp_per_sec == a.ramp_per_sec - 1 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while sh.connected.load(Ordering::Relaxed) + sh.failed.load(Ordering::Relaxed) < a.conns as u64 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("{}", json!({"event": "ready", "connected": sh.connected.load(Ordering::Relaxed), "failed": sh.failed.load(Ordering::Relaxed), "connect_secs": t0.elapsed().as_secs_f64()}));
    let mut go = String::new();
    let _ = tokio::task::spawn_blocking(move || {
        std::io::stdin().read_line(&mut go).ok();
    })
    .await;
    let (n0, b0) = (sh.notifies.load(Ordering::Relaxed), sh.bytes.load(Ordering::Relaxed));
    sh.t_go_ns.store(now_ns(), Ordering::Relaxed);
    sh.go.store(true, Ordering::Relaxed);
    let want = sh.connected.load(Ordering::Relaxed) as usize;
    let end = Instant::now() + Duration::from_secs(a.wait_secs);
    while sh.recv_ns.lock().unwrap().len() < want && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let t_fan = Instant::now();
    let (n_fan, b_fan) = (sh.notifies.load(Ordering::Relaxed), sh.bytes.load(Ordering::Relaxed));
    if a.steady_secs > 0 {
        tokio::time::sleep(Duration::from_secs(a.steady_secs)).await;
    }
    let steady = t_fan.elapsed().as_secs_f64();
    let (n1, b1) = (sh.notifies.load(Ordering::Relaxed), sh.bytes.load(Ordering::Relaxed));
    let _ = (n0, b0);
    let t_go = sh.t_go_ns.load(Ordering::Relaxed);
    let mut v: Vec<f64> = sh.recv_ns.lock().unwrap().iter().map(|t| (*t as f64 - t_go as f64) / 1e6).collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| if v.is_empty() { 0.0 } else { v[((v.len() as f64 * p) as usize).min(v.len() - 1)] };
    println!(
        "{}",
        json!({"event": "result", "connected": want, "received_clean": v.len(), "t_go_unix": t_go as f64 / 1e9,
               "ms": {"min": q(0.0), "p50": q(0.5), "p90": q(0.9), "p99": q(0.99), "max": q(1.0)},
               "notifies": sh.notifies.load(Ordering::Relaxed), "responses": sh.responses.load(Ordering::Relaxed),
               "steady": {"secs": steady, "jobs_per_sec": (n1 - n_fan) as f64 / steady.max(0.001), "mbit_per_sec": (b1 - b_fan) as f64 * 8.0 / 1e6 / steady.max(0.001)}})
    );
}
