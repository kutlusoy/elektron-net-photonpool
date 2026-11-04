//! JSON helpers for the command line (`photond photon-plan`, `photond photon-check`), used by the cross-language tests.
use super::coinbase::{check_announced, node_candidate, parse};
use super::msg::{PayoutPlan, PlanOutput};
use super::plan::{check_plan, derive, hash, Rules};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

fn hexf(v: &Value, k: &str) -> Result<Vec<u8>> {
    Ok(hex::decode(v[k].as_str().ok_or_else(|| anyhow!("missing {k}"))?)?)
}
fn h32(v: &Value, k: &str) -> Result<[u8; 32]> {
    hexf(v, k)?.try_into().map_err(|_| anyhow!("{k} must be 32 bytes"))
}
fn num(v: &Value, k: &str) -> Result<u64> {
    v[k].as_u64().ok_or_else(|| anyhow!("missing {k}"))
}

fn rules_of(v: &Value) -> Result<Rules> {
    Ok(Rules { n_max: num(v, "n_max")? as u16, min_ppm: num(v, "min_ppm")? as u32, fee_ppm: num(v, "fee_ppm")? as u32, fee_script: hexf(v, "fee_script")? })
}

pub fn plan_json(p: &PayoutPlan) -> Value {
    json!({
        "tip": hex::encode(p.tip), "height": p.height, "rules_version": p.rules_version, "plan_hash": hex::encode(p.plan_hash),
        "accounting_root": hex::encode(p.accounting_root), "fee_script": hex::encode(&p.fee_script), "fee_ppm": p.fee_ppm,
        "outputs": p.outputs.iter().map(|o| json!({"script": hex::encode(&o.script), "ppm": o.ppm})).collect::<Vec<_>>(),
    })
}

fn plan_from(v: &Value) -> Result<PayoutPlan> {
    Ok(PayoutPlan {
        tip: h32(v, "tip")?,
        height: num(v, "height")? as u32,
        rules_version: num(v, "rules_version")? as u16,
        plan_hash: h32(v, "plan_hash")?,
        accounting_root: h32(v, "accounting_root")?,
        fee_script: hexf(v, "fee_script")?,
        fee_ppm: num(v, "fee_ppm")? as u32,
        outputs: v["outputs"].as_array().ok_or_else(|| anyhow!("missing outputs"))?.iter().map(|o| Ok(PlanOutput { script: hexf(o, "script")?, ppm: num(o, "ppm")? as u32 })).collect::<Result<_>>()?,
    })
}

pub fn run_plan(input: &Value) -> Result<Value> {
    let rules = rules_of(&input["rules"])?;
    let credits: Vec<(Vec<u8>, u64)> = input["credits"].as_array().ok_or_else(|| anyhow!("missing credits"))?.iter().map(|c| Ok((hexf(c, "script")?, num(c, "credit")?))).collect::<Result<_>>()?;
    let plan = derive(&rules, h32(input, "tip")?, num(input, "height")? as u32, &credits)?;
    check_plan(&plan, &rules)?;
    let suffix = input["suffix"].as_str().map(hex::decode).transpose()?.unwrap_or_default();
    Ok(json!({"plan": plan_json(&plan), "candidate": node_candidate(&plan, num(input, "reward")?, &suffix)}))
}

pub fn run_check(input: &Value) -> Result<Value> {
    let plan = plan_from(&input["plan"])?;
    if plan.plan_hash != hash(&plan) {
        return Err(anyhow!("plan hash does not match the plan"));
    }
    let cb = parse(&hexf(input, "coinbase_hex")?)?;
    let reward = check_announced(&plan, &cb, num(input, "max_reward")?)?;
    Ok(json!({"ok": true, "reward": reward}))
}
