mod accounting;
mod addr;
mod api;
mod config;
mod diff;
mod ledger;
mod photon;
mod pool;
mod public;
mod rpc;
mod stratum;
mod template;
mod tx;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use pool::{Pool, ShareResult, Submission, POOL_VERSION_SHIFT};
use serde_json::json;
use std::path::PathBuf;
use template::{grind, header_hash};

#[derive(Parser)]
#[command(name = "photond", version, about = "Headless mining pool daemon for Elektron Net")]
struct Cli {
    #[arg(short, long, default_value = "photond.toml")]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon: follow the node, build candidates, serve the admin API and UI.
    Run,
    /// Build and fully validate all candidates once; with --mine also solve and submit one block (regtest).
    Check {
        #[arg(long)]
        mine: bool,
    },
    /// Photon: derive a payout plan and the node candidate from JSON on stdin (cross-language tests).
    PhotonPlan,
    /// Photon: check a coinbase against a plan, JSON on stdin (cross-language tests).
    PhotonCheck,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let cli = Cli::parse();
    if matches!(cli.cmd, Cmd::PhotonPlan | Cmd::PhotonCheck) {
        let input: serde_json::Value = serde_json::from_reader(std::io::stdin())?;
        let out = if matches!(cli.cmd, Cmd::PhotonPlan) { photon::cli::run_plan(&input) } else { photon::cli::run_check(&input) };
        match out {
            Ok(v) => println!("{v}"),
            Err(e) => {
                println!("{}", json!({"ok": false, "error": format!("{e:#}")}));
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    match cli.cmd {
        Cmd::Run => loop {
            match Pool::connect(config::Config::load(&cli.config)?).await {
                Ok(pool) => return run(pool).await,
                Err(e) => {
                    tracing::warn!("waiting for the node: {e:#}");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        },
        Cmd::PhotonPlan | Cmd::PhotonCheck => unreachable!(),
        Cmd::Check { mine } => check(Pool::connect(config::Config::load(&cli.config)?).await?, mine).await,
    }
}

async fn run(pool: std::sync::Arc<Pool>) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(&pool.cfg.api.listen).await?;
    pool.event("info", format!("photond {} listening on {}", env!("CARGO_PKG_VERSION"), pool.cfg.api.listen));
    tokio::spawn(pool.clone().run_tip_loop());
    tokio::spawn(pool.clone().run_accounting());
    if pool.cfg.public.enabled {
        let p = pool.clone();
        tokio::spawn(async move {
            if let Err(e) = public::serve(p.clone()).await {
                p.event("err", format!("public page stopped: {e}"));
            }
        });
    }
    if pool.cfg.stratum.enabled {
        tokio::spawn(stratum::serve(pool.clone()));
    }
    axum::serve(listener, api::router(pool)).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}

async fn check(pool: std::sync::Arc<Pool>, mine: bool) -> Result<()> {
    let epoch = pool.build_epoch(usize::MAX).await?;
    let mut report = json!({
        "height": epoch.height, "tip": epoch.tip_hex, "candidates": epoch.candidates.len(),
        "configured": pool.payouts.len(), "rejected": pool.stats.candidates_rejected.load(std::sync::atomic::Ordering::Relaxed),
        "fetch_ms": pool.stats.last_fetch_ms.load(std::sync::atomic::Ordering::Relaxed),
    });
    if epoch.candidates.len() != pool.payouts.len() {
        println!("{report}");
        bail!("some candidates were rejected by the node");
    }
    if mine {
        let (_, unit) = epoch.next_unit();
        let c = &epoch.candidates[unit.cand];
        let version = c.version | ((unit.slice as u32) << POOL_VERSION_SHIFT);
        let ntime = c.curtime.max(c.mintime) + unit.ntime_off;
        let Some(nonce) = grind(c, version, ntime, &c.target) else { bail!("no nonce found") };
        let verdict = epoch.check_share(&Submission { cand: unit.cand, slice: unit.slice, ntime, version, nonce }, &c.target);
        if verdict != ShareResult::Block {
            bail!("own share check disagrees: {verdict:?}");
        }
        let mut hash = header_hash(&c.header(version, ntime, nonce));
        hash.reverse();
        let result = pool.node.submit_block(&c.block_hex(version, ntime, nonce)).await?;
        let best = pool.node.best_block_hash().await?;
        report["mined"] = json!({"hash": hex::encode(hash), "submitblock": result, "accepted": result.is_none() && best == hex::encode(hash)});
        println!("{report}");
        if report["mined"]["accepted"] != true {
            bail!("node did not accept the mined block");
        }
        return Ok(());
    }
    println!("{report}");
    Ok(())
}
