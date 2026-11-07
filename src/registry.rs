//! Block attribution through the Elektron Net registry (https://github.com/kutlusoy/elektron-net-registry).
//! The pool reads `mempools.txt`, tells every listed explorer when it found a block, and answers the explorer's call back
//! (`/api/pool/identity/confirm`) honestly: true only for blocks this pool itself found recently. No wallet, no on-chain data.
use crate::config::Config;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{client::legacy::{connect::HttpConnector, Client}, rt::TokioExecutor};
use std::{collections::HashMap, path::PathBuf, sync::Mutex, time::{Duration, Instant}};

pub const DEFAULT_URL: &str = "https://raw.githubusercontent.com/kutlusoy/elektron-net-registry/main";
const POLL: Duration = Duration::from_secs(15 * 60);
const RECENT: Duration = Duration::from_secs(30 * 60);
const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FILE: usize = 1 << 20;

pub struct Registry {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    base: String,
    pub name: String,
    pub url: String,
    pub active: bool,
    cache: PathBuf,
    mempools: Mutex<Vec<(String, String)>>,
    recent: Mutex<HashMap<String, Instant>>,
    fee: Mutex<Option<(Instant, f64)>>,
}

/// Every quoted substring of a line is a field; a line needs exactly two non-empty ones (name, url), other lines are skipped.
pub fn parse_mempools(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = Vec::new();
            let mut rest = line;
            while let Some(a) = rest.find('"') {
                let tail = &rest[a + 1..];
                let Some(b) = tail.find('"') else { break };
                fields.push(tail[..b].trim().to_string());
                rest = &tail[b + 1..];
            }
            (fields.len() == 2 && fields.iter().all(|f| !f.is_empty())).then(|| (fields[0].clone(), fields[1].clone()))
        })
        .collect()
}

impl Registry {
    pub fn new(cfg: &Config) -> Self {
        let connector = HttpsConnectorBuilder::new().with_webpki_roots().https_or_http().enable_http1().build();
        let base = cfg.registry.url.trim().trim_end_matches('/').to_string();
        let name = if cfg.registry.name.trim().is_empty() { cfg.public.name.trim().to_string() } else { cfg.registry.name.trim().to_string() };
        let url = if cfg.public.stratum_host.is_empty() { String::new() } else { format!("https://{}", cfg.public.stratum_host) };
        // the shared registry only knows mainnet; a custom registry url (tests, forks) is honoured on any network
        let active = cfg.registry.enabled && !name.is_empty() && (cfg.pool.network == "mainnet" || base != DEFAULT_URL);
        let cache = std::path::Path::new(&cfg.ledger.path).with_file_name("mempools-registry.txt");
        Self { client: Client::builder(TokioExecutor::new()).build(connector), base, name, url, active, cache, mempools: Mutex::new(Vec::new()), recent: Mutex::new(HashMap::new()), fee: Mutex::new(None) }
    }

    /// Recommended fee rate (sat/vB, the one-hour target) from the explorer's `/api/v1/fees/recommended`, cached for a minute.
    /// Clamped to 1 (the wallet minimum) .. 500 sat/vB so a broken or hostile answer cannot make a payout absurdly expensive.
    pub async fn explorer_fee(&self, explorer_url: &str) -> anyhow::Result<f64> {
        if let Some((t, v)) = *self.fee.lock().unwrap() {
            if t.elapsed() < Duration::from_secs(60) { return Ok(v); }
        }
        let req = Request::builder().method(Method::GET).uri(format!("{}/api/v1/fees/recommended", explorer_url.trim().trim_end_matches('/'))).header("user-agent", "photond").body(Full::new(Bytes::new()))?;
        let v: serde_json::Value = serde_json::from_slice(&self.request(req).await?)?;
        let rate = v["hourFee"].as_f64().filter(|r| r.is_finite() && *r > 0.0).ok_or_else(|| anyhow::anyhow!("no hourFee in the answer"))?.clamp(1.0, 500.0);
        *self.fee.lock().unwrap() = Some((Instant::now(), rate));
        Ok(rate)
    }

    pub fn mempool_count(&self) -> usize { self.mempools.lock().unwrap().len() }

    /// True only for a block this pool recorded itself within the last 30 minutes.
    pub fn confirmed(&self, hash: &str) -> bool {
        let mut r = self.recent.lock().unwrap();
        r.retain(|_, t| t.elapsed() < RECENT);
        self.active && r.contains_key(&hash.to_ascii_lowercase())
    }

    async fn request(&self, req: Request<Full<Bytes>>) -> anyhow::Result<Vec<u8>> {
        let resp = tokio::time::timeout(TIMEOUT * 2, self.client.request(req)).await??;
        if !resp.status().is_success() { anyhow::bail!("HTTP {}", resp.status()); }
        let mut body = resp.into_body();
        let mut out = Vec::new();
        while let Some(frame) = tokio::time::timeout(TIMEOUT * 2, body.frame()).await? {
            if let Ok(d) = frame?.into_data() { out.extend_from_slice(&d); }
            if out.len() > MAX_FILE { anyhow::bail!("answer too large"); }
        }
        Ok(out)
    }

    pub async fn refresh(&self) -> anyhow::Result<usize> {
        let req = Request::builder().method(Method::GET).uri(format!("{}/mempools.txt", self.base)).header("user-agent", "photond").body(Full::new(Bytes::new()))?;
        let text = String::from_utf8(self.request(req).await?)?;
        let list = parse_mempools(&text);
        // an error page or an empty file never replaces a good list
        if list.is_empty() { anyhow::bail!("no entries in the answer"); }
        let n = list.len();
        *self.mempools.lock().unwrap() = list;
        let _ = std::fs::write(&self.cache, text);
        Ok(n)
    }

    /// Read the last good list from disk, then keep it fresh every 15 minutes.
    pub async fn run(self: std::sync::Arc<Self>, pool: std::sync::Arc<crate::pool::Pool>) {
        if !self.active { return; }
        if let Ok(t) = std::fs::read_to_string(&self.cache) { *self.mempools.lock().unwrap() = parse_mempools(&t); }
        loop {
            match self.refresh().await {
                Ok(n) => pool.event("info", format!("registry: {n} explorers listed, this pool reports blocks as \"{}\"", self.name)),
                Err(e) => pool.event("warn", format!("registry: could not refresh {}/mempools.txt: {e}", self.base)),
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Remember the block and tell every listed explorer; fire and forget, nothing else depends on the answers.
    pub fn block_found(self: &std::sync::Arc<Self>, hash: &str, pool: std::sync::Arc<crate::pool::Pool>) {
        if !self.active { return; }
        let hash = hash.to_ascii_lowercase();
        self.recent.lock().unwrap().insert(hash.clone(), Instant::now());
        let targets = self.mempools.lock().unwrap().clone();
        let body = serde_json::json!({"name": self.name, "blockHash": hash}).to_string();
        for (mname, murl) in targets {
            let (me, body, hash, pool) = (self.clone(), body.clone(), hash.clone(), pool.clone());
            tokio::spawn(async move {
                let uri = format!("{}/api/v1/pool-registry/report", murl.trim_end_matches('/'));
                let req = match Request::builder().method(Method::POST).uri(&uri).header("content-type", "application/json").header("user-agent", "photond").body(Full::new(Bytes::from(body))) { Ok(r) => r, Err(_) => return };
                match me.request(req).await {
                    Ok(_) => {}
                    Err(e) => pool.event("info", format!("registry: block {} not reported to {mname}: {e}", &hash[..16.min(hash.len())])),
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_mempools, Registry};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn fake_explorer(body: &'static str) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    fn registry() -> Registry {
        let cfg: crate::config::Config = toml::from_str("[node]\nrpc_url = \"x\"\nauth = \"y\"\n[pool]\nnetwork = \"mainnet\"\n").unwrap();
        Registry::new(&cfg)
    }

    #[tokio::test]
    async fn the_fee_rate_comes_from_the_explorer_within_sane_bounds() {
        let r = registry();
        assert_eq!(r.explorer_fee(&fake_explorer(r#"{"fastestFee":9,"halfHourFee":5,"hourFee":3.5,"economyFee":2,"minimumFee":1}"#).await).await.unwrap(), 3.5);
        assert_eq!(registry().explorer_fee(&fake_explorer(r#"{"hourFee":0.2}"#).await).await.unwrap(), 1.0);
        assert_eq!(registry().explorer_fee(&fake_explorer(r#"{"hourFee":99999}"#).await).await.unwrap(), 500.0);
        assert!(registry().explorer_fee(&fake_explorer(r#"{"nothing":1}"#).await).await.is_err());
        assert!(registry().explorer_fee("http://127.0.0.1:1").await.is_err());
    }


    #[test]
    fn parses_the_registry_format_and_skips_bad_lines() {
        let t = "\"Elektron Net Mempool\", \"https://mempool.elektron-net.org\";\n\"two\" \"https://b\"\nbroken\n\"only one\";\n\"\", \"https://empty\";\n\"a\", \"b\", \"c\";\n";
        assert_eq!(parse_mempools(t), vec![("Elektron Net Mempool".into(), "https://mempool.elektron-net.org".into()), ("two".into(), "https://b".into())]);
        assert!(parse_mempools("<html>error</html>").is_empty());
    }
}
