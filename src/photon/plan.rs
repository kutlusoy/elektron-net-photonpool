//! Deterministic payout plan (rules version 1).
//!
//! Input: the credits of the window ending at block `h-2` (script, credit) and the rules.
//! 1. ppm of an entry = floor(credit * (1e6 - fee_ppm) / total)
//! 2. entries below `min_ppm` are dropped (their credit stays in the ledger and carries over)
//! 3. at most `n_max` entries remain, largest credit first, ties by script bytes
//! 4. ppm is recomputed over the kept entries so that payouts + fee use the whole reward
//! 5. outputs are ordered by ppm descending, then script bytes
use super::merkle::AccountingTree;
use super::msg::{PayoutPlan, PlanOutput, MAX_PLAN_OUTPUTS};
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

pub const RULES_VERSION: u16 = 1;
pub const PPM: u64 = 1_000_000;

#[derive(Debug, Clone)]
pub struct Rules {
    pub n_max: u16,
    pub min_ppm: u32,
    pub fee_ppm: u32,
    pub fee_script: Vec<u8>,
}

impl Rules {
    pub fn validate(&self) -> Result<()> {
        if self.n_max == 0 || self.n_max as usize > MAX_PLAN_OUTPUTS {
            bail!("n_max must be 1..={MAX_PLAN_OUTPUTS}");
        }
        if self.fee_ppm as u64 > PPM {
            bail!("fee_ppm above 100 %");
        }
        if self.fee_script.is_empty() || self.fee_script.len() > 10_000 {
            bail!("fee script missing");
        }
        Ok(())
    }
}

pub fn derive(rules: &Rules, tip: [u8; 32], height: u32, credits: &[(Vec<u8>, u64)]) -> Result<PayoutPlan> {
    rules.validate()?;
    let tree = AccountingTree::new(credits.iter().filter(|c| c.1 > 0).cloned().collect())?;
    let root = tree.root();
    let mut live: Vec<(Vec<u8>, u64)> = credits.iter().filter(|c| c.1 > 0).cloned().collect();
    live.sort_by(|a, b| a.0.cmp(&b.0));
    let total: u128 = live.iter().map(|c| c.1 as u128).sum();
    let share = PPM - rules.fee_ppm as u64;
    let ppm_of = |credit: u64, base: u128| -> u32 { (credit as u128 * share as u128 / base) as u32 };

    let mut kept: Vec<(Vec<u8>, u64)> = if total == 0 {
        vec![]
    } else {
        live.into_iter().filter(|(_, c)| ppm_of(*c, total) >= rules.min_ppm.max(1)).collect()
    };
    kept.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    kept.truncate(rules.n_max as usize);
    let base: u128 = kept.iter().map(|c| c.1 as u128).sum();
    let mut outputs: Vec<PlanOutput> = kept.iter().map(|(s, c)| PlanOutput { script: s.clone(), ppm: ppm_of(*c, base) }).filter(|o| o.ppm > 0).collect();
    outputs.sort_by(|a, b| b.ppm.cmp(&a.ppm).then_with(|| a.script.cmp(&b.script)));

    let mut plan = PayoutPlan { tip, height, rules_version: RULES_VERSION, plan_hash: [0; 32], accounting_root: root, outputs, fee_script: rules.fee_script.clone(), fee_ppm: rules.fee_ppm };
    plan.plan_hash = hash(&plan);
    Ok(plan)
}

pub fn hash(p: &PayoutPlan) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"PHOTON-PLAN-V1");
    h.update(p.rules_version.to_le_bytes());
    h.update(p.height.to_le_bytes());
    h.update(p.tip);
    h.update(p.accounting_root);
    h.update((p.outputs.len() as u16).to_le_bytes());
    for o in &p.outputs {
        h.update((o.script.len() as u16).to_le_bytes());
        h.update(&o.script);
        h.update(o.ppm.to_le_bytes());
    }
    h.update((p.fee_script.len() as u16).to_le_bytes());
    h.update(&p.fee_script);
    h.update(p.fee_ppm.to_le_bytes());
    h.finalize().into()
}

/// Structural check of a plan received from the pool: hash, bounds, ordering, ppm sum.
pub fn check_plan(p: &PayoutPlan, rules: &Rules) -> Result<()> {
    if p.rules_version != RULES_VERSION {
        bail!("unknown rules version {}", p.rules_version);
    }
    if p.plan_hash != hash(p) {
        bail!("plan hash does not match the plan");
    }
    if p.outputs.len() > rules.n_max as usize {
        bail!("too many outputs");
    }
    if p.fee_ppm != rules.fee_ppm || p.fee_script != rules.fee_script {
        bail!("fee terms differ from the agreed rules");
    }
    let sum: u64 = p.outputs.iter().map(|o| o.ppm as u64).sum();
    if sum > PPM - p.fee_ppm as u64 {
        bail!("payout parts exceed the reward share");
    }
    if p.outputs.windows(2).any(|w| (w[0].ppm, std::cmp::Reverse(&w[0].script)) < (w[1].ppm, std::cmp::Reverse(&w[1].script))) {
        bail!("outputs are not in canonical order");
    }
    if p.outputs.iter().any(|o| o.ppm < rules.min_ppm.max(1)) {
        bail!("output below the dust part");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules { n_max: 4, min_ppm: 1000, fee_ppm: 10_000, fee_script: vec![0, 20, 0xfe] }
    }
    fn s(i: u8) -> Vec<u8> {
        vec![0, 20, i]
    }

    #[test]
    fn parts_follow_credits_and_leave_the_fee_share() {
        let p = derive(&rules(), [1; 32], 100, &[(s(1), 600), (s(2), 300), (s(3), 100)]).unwrap();
        let ppm: Vec<u32> = p.outputs.iter().map(|o| o.ppm).collect();
        assert_eq!(ppm, vec![594_000, 297_000, 99_000]);
        assert_eq!(p.outputs[0].script, s(1));
        check_plan(&p, &rules()).unwrap();
        assert!(p.outputs.iter().map(|o| o.ppm as u64).sum::<u64>() <= PPM - 10_000);
    }

    #[test]
    fn dust_is_dropped_and_the_rest_renormalised() {
        // s(9) is 0.05 % of the credits: below min_ppm 1000 after the fee cut, so it is dropped
        let p = derive(&rules(), [1; 32], 100, &[(s(1), 999_500), (s(9), 500)]).unwrap();
        assert_eq!(p.outputs.len(), 1);
        assert_eq!(p.outputs[0].ppm, 990_000);
        // the dropped credit is still in the accounting root
        let with = AccountingTree::new(vec![(s(1), 999_500), (s(9), 500)]).unwrap().root();
        assert_eq!(p.accounting_root, with);
    }

    #[test]
    fn n_max_keeps_the_largest_with_a_deterministic_tie_break() {
        let credits: Vec<_> = (1..=8u8).map(|i| (s(i), 1000)).collect();
        let p = derive(&rules(), [1; 32], 100, &credits).unwrap();
        assert_eq!(p.outputs.len(), 4);
        let scripts: Vec<_> = p.outputs.iter().map(|o| o.script.clone()).collect();
        assert_eq!(scripts, vec![s(1), s(2), s(3), s(4)]);
        assert!(p.outputs.iter().all(|o| o.ppm == 247_500));
    }

    #[test]
    fn plan_is_independent_of_the_input_order() {
        let a: Vec<_> = (1..=30u8).map(|i| (s(i), 100 + i as u64 * 17)).collect();
        let mut b = a.clone();
        b.reverse();
        let mut r = rules();
        r.n_max = 10;
        assert_eq!(derive(&r, [2; 32], 7, &a).unwrap(), derive(&r, [2; 32], 7, &b).unwrap());
    }

    #[test]
    fn empty_credits_pay_everything_to_the_fee_script() {
        let p = derive(&rules(), [1; 32], 100, &[]).unwrap();
        assert!(p.outputs.is_empty());
        check_plan(&p, &rules()).unwrap();
    }

    #[test]
    fn tampering_is_detected() {
        let p = derive(&rules(), [1; 32], 100, &[(s(1), 600), (s(2), 300)]).unwrap();
        let mut a = p.clone();
        a.outputs[0].ppm += 1;
        assert!(check_plan(&a, &rules()).is_err());
        let mut b = p.clone();
        b.outputs[0].script = s(77);
        assert!(check_plan(&b, &rules()).is_err());
        let mut c = p.clone();
        c.fee_ppm = 0;
        c.plan_hash = hash(&c);
        assert!(check_plan(&c, &rules()).is_err(), "fee terms must match the rules the gateway agreed to");
        let mut d = p.clone();
        d.outputs.reverse();
        d.plan_hash = hash(&d);
        assert!(check_plan(&d, &rules()).is_err());
        let mut e = p;
        e.outputs[0].ppm = 999_999;
        e.plan_hash = hash(&e);
        assert!(check_plan(&e, &rules()).is_err());
    }

    #[test]
    fn a_large_window_stays_within_limits() {
        let mut r = rules();
        r.n_max = 128;
        r.min_ppm = 100;
        let credits: Vec<_> = (0..5000u32).map(|i| (vec![0, 20, (i % 251) as u8, (i / 251) as u8], 1 + (i as u64 * 7919) % 10_000_000)).collect();
        let p = derive(&r, [3; 32], 5, &credits).unwrap();
        assert!(p.outputs.len() <= 128);
        check_plan(&p, &r).unwrap();
        assert!(p.outputs.iter().map(|o| o.ppm as u64).sum::<u64>() <= PPM - r.fee_ppm as u64);
    }
}
