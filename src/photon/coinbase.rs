//! Plan-shaped coinbase: building the request for the node's batch RPC and checking an announced coinbase against the plan.
//!
//! vout: payouts in plan order, fee output (remainder, omitted when 0), attestation, witness commitment, `OP_RETURN "PHOT" || accounting_root`.
use super::msg::PayoutPlan;
use super::plan::PPM;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

pub const WITNESS_MAGIC: [u8; 6] = [0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
pub const MIN_SCRIPT_SIG: usize = 2;
pub const MAX_SCRIPT_SIG: usize = 100;

pub fn commitment_script(root: &[u8; 32]) -> Vec<u8> {
    let mut s = vec![0x6a, 0x24];
    s.extend_from_slice(b"PHOT");
    s.extend_from_slice(root);
    s
}

/// Payout values for `reward` and the fee remainder.
pub fn split(reward: u64, plan: &PayoutPlan) -> (Vec<u64>, u64) {
    let vals: Vec<u64> = plan.outputs.iter().map(|o| (reward as u128 * o.ppm as u128 / PPM as u128) as u64).collect();
    let fee = reward - vals.iter().sum::<u64>();
    (vals, fee)
}

/// Candidate for `getattestationbatch`: outputs with explicit values, the commitment as tail output, an optional extranonce suffix.
pub fn node_candidate(plan: &PayoutPlan, reward: u64, script_sig_suffix: &[u8]) -> Value {
    let (vals, fee) = split(reward, plan);
    let mut outs: Vec<Value> = plan.outputs.iter().zip(&vals).map(|(o, v)| json!({"scriptPubKey": hex::encode(&o.script), "value": v})).collect();
    if fee > 0 {
        outs.push(json!({"scriptPubKey": hex::encode(&plan.fee_script), "value": fee}));
    }
    let mut c = json!({"outputs": outs, "tail_outputs": [{"scriptPubKey": hex::encode(commitment_script(&plan.accounting_root))}]});
    if !script_sig_suffix.is_empty() {
        c["script_sig_suffix"] = json!(hex::encode(script_sig_suffix));
    }
    c
}

/// Asks the node to attest all candidates, split into calls that stay well below the RPC body limit (32 MB) and the
/// candidate limit (4096). Returns the first response with the candidates of all calls; calls must agree on the tip.
pub async fn attest_all(node: &crate::rpc::NodeRpc, cands: &[Value]) -> Result<Value> {
    let per = cands.first().map(|c| c.to_string().len()).unwrap_or(1).max(1);
    let chunk = (12_000_000 / per).clamp(1, 4000);
    let mut merged: Option<Value> = None;
    for part in cands.chunks(chunk) {
        let r = node.call("getattestationbatch", json!([{"candidates": part}])).await?;
        match merged.as_mut() {
            None => merged = Some(r),
            Some(m) => {
                if m["tip"] != r["tip"] || m["coinbasevalue"] != r["coinbasevalue"] {
                    bail!("the template changed between node calls");
                }
                m["candidates"].as_array_mut().ok_or_else(|| anyhow!("no candidates"))?.extend(r["candidates"].as_array().cloned().unwrap_or_default());
            }
        }
    }
    merged.ok_or_else(|| anyhow!("no candidates to attest"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCoinbase {
    pub script_sig: Vec<u8>,
    pub outputs: Vec<(u64, Vec<u8>)>,
    pub locktime: u32,
}

struct Cur<'a>(&'a [u8], usize);
impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let e = self.1.checked_add(n).filter(|e| *e <= self.0.len()).ok_or_else(|| anyhow!("coinbase truncated"))?;
        let s = &self.0[self.1..e];
        self.1 = e;
        Ok(s)
    }
    fn varint(&mut self) -> Result<u64> {
        Ok(match self.take(1)?[0] {
            0xfd => u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as u64,
            0xfe => u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as u64,
            0xff => u64::from_le_bytes(self.take(8)?.try_into().unwrap()),
            n => n as u64,
        })
    }
}

/// Parses a non-witness coinbase: one input with a null prevout, any number of outputs.
pub fn parse(raw: &[u8]) -> Result<ParsedCoinbase> {
    let mut c = Cur(raw, 0);
    if c.take(4)? != 2i32.to_le_bytes() {
        bail!("coinbase version is not 2");
    }
    if c.varint()? != 1 {
        bail!("coinbase needs exactly one input");
    }
    if c.take(32)? != [0u8; 32] || c.take(4)? != [0xff; 4] {
        bail!("input is not a coinbase input");
    }
    let n = c.varint()? as usize;
    if !(MIN_SCRIPT_SIG..=MAX_SCRIPT_SIG).contains(&n) {
        bail!("scriptSig length {n} out of range");
    }
    let script_sig = c.take(n)?.to_vec();
    c.take(4)?; // sequence
    let outs = c.varint()? as usize;
    if outs > 1000 {
        bail!("too many outputs");
    }
    let mut outputs = Vec::with_capacity(outs);
    for _ in 0..outs {
        let v = u64::from_le_bytes(c.take(8)?.try_into().unwrap());
        let l = c.varint()? as usize;
        if l > 10_000 {
            bail!("output script too long");
        }
        outputs.push((v, c.take(l)?.to_vec()));
    }
    let locktime = u32::from_le_bytes(c.take(4)?.try_into().unwrap());
    if c.1 != raw.len() {
        bail!("trailing bytes after the coinbase");
    }
    Ok(ParsedCoinbase { script_sig, outputs, locktime })
}

/// Witness serialisation of a non-witness coinbase (marker, flag and the 32-byte zero witness reserved value).
pub fn to_witness(nowit: &[u8]) -> Vec<u8> {
    let mut w = Vec::with_capacity(nowit.len() + 40);
    w.extend_from_slice(&nowit[..4]);
    w.extend_from_slice(&[0x00, 0x01]);
    w.extend_from_slice(&nowit[4..nowit.len() - 4]);
    w.extend_from_slice(&[0x01, 0x20]);
    w.extend_from_slice(&[0u8; 32]);
    w.extend_from_slice(&nowit[nowit.len() - 4..]);
    w
}

pub fn scriptnum(mut n: u64) -> Vec<u8> {
    let mut v = vec![];
    while n > 0 {
        v.push((n & 0xff) as u8);
        n >>= 8;
    }
    if v.last().map_or(false, |b| b & 0x80 != 0) {
        v.push(0);
    }
    v
}

fn is_attestation(script: &[u8], height: u32) -> bool {
    let h = scriptnum(height as u64);
    script.len() == 1 + 1 + h.len() + 1 + 32 && script[0] == 0x6a && script[1] as usize == h.len() && script[2..2 + h.len()] == h[..] && script[2 + h.len()] == 0x20
}

/// Checks an announced coinbase against the plan; returns the reward it distributes.
pub fn check_announced(plan: &PayoutPlan, cb: &ParsedCoinbase, max_reward: u64) -> Result<u64> {
    if cb.locktime != plan.height - 1 {
        bail!("locktime is not height - 1");
    }
    let h = scriptnum(plan.height as u64);
    if cb.script_sig.len() < 1 + h.len() || cb.script_sig[0] as usize != h.len() || cb.script_sig[1..1 + h.len()] != h[..] {
        bail!("scriptSig does not start with the BIP 34 height");
    }
    let reward = cb.outputs.iter().try_fold(0u64, |a, (v, _)| a.checked_add(*v)).ok_or_else(|| anyhow!("output values overflow"))?;
    if reward > max_reward {
        bail!("coinbase value {reward} above the bound {max_reward}");
    }
    let (vals, fee) = split(reward, plan);
    let mut expect: Vec<(u64, &[u8])> = plan.outputs.iter().zip(&vals).map(|(o, v)| (*v, o.script.as_slice())).collect();
    if fee > 0 {
        expect.push((fee, plan.fee_script.as_slice()));
    }
    let k = expect.len();
    if cb.outputs.len() != k + 3 {
        bail!("coinbase has {} outputs, the plan needs {}", cb.outputs.len(), k + 3);
    }
    for (i, (v, s)) in expect.iter().enumerate() {
        if cb.outputs[i].0 != *v || cb.outputs[i].1 != *s {
            bail!("output {i} deviates from the plan");
        }
    }
    let (att, wc, commit) = (&cb.outputs[k], &cb.outputs[k + 1], &cb.outputs[k + 2]);
    if att.0 != 0 || !is_attestation(&att.1, plan.height) {
        bail!("output {k} is not an attestation output");
    }
    if wc.0 != 0 || wc.1.len() != 38 || wc.1[..6] != WITNESS_MAGIC {
        bail!("output {} is not the witness commitment", k + 1);
    }
    if commit.0 != 0 || commit.1 != commitment_script(&plan.accounting_root) {
        bail!("accounting commitment missing or wrong");
    }
    Ok(reward)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::photon::msg::PlanOutput;
    use crate::photon::plan::{derive, Rules};
    use crate::tx::{build_coinbase, TxOutput};

    fn plan() -> PayoutPlan {
        let rules = Rules { n_max: 8, min_ppm: 100, fee_ppm: 10_000, fee_script: vec![0, 20, 0xfe] };
        derive(&rules, [1; 32], 1001, &[(vec![0, 20, 1], 600), (vec![0, 20, 2], 300), (vec![0, 20, 3], 100)]).unwrap()
    }

    fn make(plan: &PayoutPlan, reward: u64, tweak: impl Fn(&mut Vec<TxOutput>)) -> ParsedCoinbase {
        let (vals, fee) = split(reward, plan);
        let mut outs: Vec<TxOutput> = plan.outputs.iter().zip(&vals).map(|(o, v)| TxOutput { value: *v as i64, script: o.script.clone() }).collect();
        if fee > 0 {
            outs.push(TxOutput { value: fee as i64, script: plan.fee_script.clone() });
        }
        let mut att = vec![0x6a, 0x02, 0xe9, 0x03, 0x20];
        att.extend_from_slice(&[7u8; 32]);
        let mut wc = WITNESS_MAGIC.to_vec();
        wc.extend_from_slice(&[9u8; 32]);
        outs.push(TxOutput { value: 0, script: att });
        outs.push(TxOutput { value: 0, script: wc });
        outs.push(TxOutput { value: 0, script: commitment_script(&plan.accounting_root) });
        tweak(&mut outs);
        let mut sig = vec![0x02, 0xe9, 0x03];
        sig.extend_from_slice(&[0xaa, 0xbb]);
        parse(&build_coinbase(&sig, plan.height, &outs).nowit).unwrap()
    }

    #[test]
    fn a_plan_shaped_coinbase_passes_and_splits_exactly() {
        let p = plan();
        let reward = 5_000_000_000u64;
        let cb = make(&p, reward, |_| {});
        assert_eq!(check_announced(&p, &cb, reward).unwrap(), reward);
        let (vals, fee) = split(reward, &p);
        assert_eq!(vals.iter().sum::<u64>() + fee, reward);
        assert!(fee >= reward / 100, "fee share at least the agreed 1 %");
    }

    #[test]
    fn deviations_are_refused() {
        let p = plan();
        let reward = 5_000_000_000u64;
        let cases: Vec<(&str, Box<dyn Fn(&mut Vec<TxOutput>)>)> = vec![
            ("payout value changed", Box::new(|o| { o[0].value -= 1; o[1].value += 1; })),
            ("payout script swapped", Box::new(|o| o[0].script = vec![0, 20, 0x77])),
            ("output removed", Box::new(|o| { o.remove(1); })),
            ("extra output", Box::new(|o| o.push(TxOutput { value: 0, script: vec![0x51] }))),
            ("commitment root wrong", Box::new(|o| { let n = o.len(); o[n - 1].script[10] ^= 1; })),
            ("attestation shape wrong", Box::new(|o| { let n = o.len(); o[n - 3].script = vec![0x6a, 0x20].into_iter().chain([0u8; 32]).collect(); })),
            ("fee output stolen", Box::new(|o| { let n = o.len(); o[n - 4].script = vec![0, 20, 0x99]; })),
        ];
        for (name, f) in cases {
            let cb = make(&p, reward, f);
            assert!(check_announced(&p, &cb, reward).is_err(), "{name}");
        }
        let ok = make(&p, reward, |_| {});
        assert!(check_announced(&p, &ok, reward - 1).is_err(), "reward bound");
        let mut late = ok.clone();
        late.locktime += 1;
        assert!(check_announced(&p, &late, reward).is_err());
        let mut sig = ok;
        sig.script_sig[1] ^= 1;
        assert!(check_announced(&p, &sig, reward).is_err());
    }

    #[test]
    fn rounding_remainder_goes_to_the_fee_and_zero_fee_output_is_omitted() {
        let mut p = plan();
        p.outputs = vec![PlanOutput { script: vec![0, 20, 1], ppm: 1_000_000 }];
        p.fee_ppm = 0;
        let cb = make(&p, 1_234_567, |_| {});
        assert_eq!(cb.outputs.len(), 4, "one payout, attestation, witness commitment, commitment");
        check_announced(&p, &cb, 2_000_000).unwrap();
    }

    #[test]
    fn witness_serialisation_matches_the_tx_builder() {
        let outs = [TxOutput { value: 5, script: vec![0x51] }];
        let cb = build_coinbase(&[0x02, 0xe9, 0x03], 1001, &outs);
        assert_eq!(to_witness(&cb.nowit), cb.wit);
    }

    #[test]
    fn scriptnum_matches_script_encoding() {
        assert_eq!(scriptnum(1001), vec![0xe9, 0x03]);
        assert_eq!(scriptnum(128), vec![0x80, 0x00]);
        assert_eq!(scriptnum(255), vec![0xff, 0x00]);
        assert_eq!(scriptnum(32768), vec![0x00, 0x80, 0x00]);
    }

    #[test]
    fn node_candidate_has_the_plan_layout() {
        let p = plan();
        let c = node_candidate(&p, 5_000_000_000, &[1, 2, 3]);
        let outs = c["outputs"].as_array().unwrap();
        assert_eq!(outs.len(), p.outputs.len() + 1);
        assert_eq!(c["tail_outputs"][0]["scriptPubKey"].as_str().unwrap(), hex::encode(commitment_script(&p.accounting_root)));
        assert_eq!(c["script_sig_suffix"], "010203");
        let sum: u64 = outs.iter().map(|o| o["value"].as_u64().unwrap()).sum();
        assert_eq!(sum, 5_000_000_000);
    }

    #[test]
    fn garbage_is_rejected_by_the_parser() {
        let ok = make(&plan(), 5_000_000_000, |_| {});
        let raw = build_coinbase(&ok.script_sig, 1001, &[TxOutput { value: 1, script: vec![0x51] }]).nowit;
        for cut in 0..raw.len() {
            assert!(parse(&raw[..cut]).is_err(), "cut {cut}");
        }
        let mut long = raw.clone();
        long.push(0);
        assert!(parse(&long).is_err());
    }
}
