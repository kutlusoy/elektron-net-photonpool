//! Deterministic payout plan (rules version 2).
//!
//! Everybody with a credit is paid: there is no minimum amount and no minimum part. Parts are expressed in parts per billion (ppb) of the
//! block reward, so that at the current subsidy one part is half a lepton and nobody is lost to rounding except shares worth less than one lepton.
//!
//! Input: the credits of the window ending at block `h-2` (script, credit) and the rules.
//! 1. every credit becomes a weight; the weights are normalised so that payouts plus the pool fee are exactly the whole reward
//! 2. if there are at most `n_max` credits, all of them are paid in every block
//! 3. otherwise the plan has room for `n_max` outputs (a limit of the miners' firmware, see the review guideline): the `n_max - n_max/4` largest
//!    credits are paid in every block and the rest, ordered by script, is split into groups that take turns. The group of block `h` is `h mod groups`
//!    and gets its credit multiplied by the number of groups, so every credit is paid at its full value once per cycle and nobody is held back
//! 4. outputs are ordered by part descending, then script
use super::merkle::AccountingTree;
use super::msg::{PayoutPlan, PlanOutput, MAX_PLAN_OUTPUTS};
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

pub const RULES_VERSION: u16 = 2;
/// Parts per billion: the unit of `PlanOutput::ppb` and `PayoutPlan::fee_ppb`.
pub const PPB: u64 = 1_000_000_000;

#[derive(Debug, Clone)]
pub struct Rules {
    /// Most outputs of one plan.
    pub n_max: u16,
    pub fee_ppb: u32,
    pub fee_script: Vec<u8>,
}

impl Rules {
    pub fn validate(&self) -> Result<()> {
        if self.n_max < 4 || self.n_max as usize > MAX_PLAN_OUTPUTS {
            bail!("n_max must be 4..={MAX_PLAN_OUTPUTS}");
        }
        if self.fee_ppb as u64 > PPB {
            bail!("fee above 100 %");
        }
        if self.fee_script.is_empty() || self.fee_script.len() > 10_000 {
            bail!("fee script missing");
        }
        Ok(())
    }
}

/// Which credits are paid in the block of `height`, and with which weight (the credit, multiplied by the number of groups for the turn-taking tail).
fn weights(live: &[(Vec<u8>, u64)], n_max: usize, height: u32) -> Vec<(Vec<u8>, u128)> {
    if live.len() <= n_max {
        return live.iter().map(|(s, c)| (s.clone(), *c as u128)).collect();
    }
    let turn = (n_max / 4).max(1);
    let regular = n_max - turn;
    let mut by_credit: Vec<&(Vec<u8>, u64)> = live.iter().collect();
    by_credit.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut out: Vec<(Vec<u8>, u128)> = by_credit[..regular].iter().map(|(s, c)| (s.clone(), *c as u128)).collect();
    let mut tail: Vec<&(Vec<u8>, u64)> = by_credit[regular..].to_vec();
    tail.sort_by(|a, b| a.0.cmp(&b.0));
    let groups = (tail.len() + turn - 1) / turn;
    let this = height as usize % groups;
    for (j, (s, c)) in tail.into_iter().enumerate() {
        if j % groups == this {
            out.push((s.clone(), *c as u128 * groups as u128));
        }
    }
    out
}

pub fn derive(rules: &Rules, tip: [u8; 32], height: u32, credits: &[(Vec<u8>, u64)]) -> Result<PayoutPlan> {
    derive_full(rules, tip, height, credits).map(|(p, _)| p)
}

/// Like `derive`, also returning the accounting tree (for proofs).
pub fn derive_full(rules: &Rules, tip: [u8; 32], height: u32, credits: &[(Vec<u8>, u64)]) -> Result<(PayoutPlan, AccountingTree)> {
    rules.validate()?;
    let tree = AccountingTree::new(credits.iter().filter(|c| c.1 > 0).cloned().collect())?;
    let root = tree.root();
    let mut live: Vec<(Vec<u8>, u64)> = credits.iter().filter(|c| c.1 > 0).cloned().collect();
    live.sort_by(|a, b| a.0.cmp(&b.0));
    let paid = weights(&live, rules.n_max as usize, height);
    let total: u128 = paid.iter().map(|(_, w)| *w).sum();
    let budget = (PPB - rules.fee_ppb as u64) as u128;
    let mut outputs: Vec<PlanOutput> = if total == 0 {
        vec![]
    } else {
        paid.into_iter().map(|(s, w)| PlanOutput { script: s, ppb: (w * budget / total) as u32 }).filter(|o| o.ppb > 0).collect()
    };
    outputs.sort_by(|a, b| b.ppb.cmp(&a.ppb).then_with(|| a.script.cmp(&b.script)));

    let mut plan = PayoutPlan { tip, height, rules_version: RULES_VERSION, plan_hash: [0; 32], accounting_root: root, outputs, fee_script: rules.fee_script.clone(), fee_ppb: rules.fee_ppb };
    plan.plan_hash = hash(&plan);
    Ok((plan, tree))
}

pub fn hash(p: &PayoutPlan) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"PHOTON-PLAN-V2");
    h.update(p.rules_version.to_le_bytes());
    h.update(p.height.to_le_bytes());
    h.update(p.tip);
    h.update(p.accounting_root);
    h.update((p.outputs.len() as u16).to_le_bytes());
    for o in &p.outputs {
        h.update((o.script.len() as u16).to_le_bytes());
        h.update(&o.script);
        h.update(o.ppb.to_le_bytes());
    }
    h.update((p.fee_script.len() as u16).to_le_bytes());
    h.update(&p.fee_script);
    h.update(p.fee_ppb.to_le_bytes());
    h.finalize().into()
}

/// Structural check of a plan received from the pool: hash, bounds, ordering, sum of the parts, fee terms.
pub fn check_plan(p: &PayoutPlan, rules: &Rules) -> Result<()> {
    if p.rules_version != RULES_VERSION {
        bail!("unknown rules version {}", p.rules_version);
    }
    if p.plan_hash != hash(p) {
        bail!("plan hash does not match the plan");
    }
    if p.outputs.len() > rules.n_max as usize {
        bail!("{} outputs, this gateway accepts at most {}", p.outputs.len(), rules.n_max);
    }
    if p.fee_ppb != rules.fee_ppb || p.fee_script != rules.fee_script {
        bail!("fee terms differ from the agreed rules");
    }
    let sum: u64 = p.outputs.iter().map(|o| o.ppb as u64).sum();
    if sum > PPB - p.fee_ppb as u64 {
        bail!("payout parts exceed the reward share");
    }
    if p.outputs.windows(2).any(|w| (w[0].ppb, std::cmp::Reverse(&w[0].script)) < (w[1].ppb, std::cmp::Reverse(&w[1].script))) {
        bail!("outputs are not in canonical order");
    }
    if p.outputs.iter().any(|o| o.ppb == 0) {
        bail!("an output without a part");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn rules() -> Rules {
        Rules { n_max: 8, fee_ppb: 10_000_000, fee_script: vec![0, 20, 0xfe] }
    }
    fn s(i: u8) -> Vec<u8> {
        vec![0, 20, i]
    }
    fn big(i: u16) -> Vec<u8> {
        vec![0, 20, (i % 251) as u8, (i / 251) as u8]
    }

    #[test]
    fn everybody_is_paid_and_the_fee_is_taken_from_the_whole_reward() {
        let p = derive(&rules(), [1; 32], 100, &[(s(1), 600), (s(2), 300), (s(3), 100)]).unwrap();
        let ppb: Vec<u32> = p.outputs.iter().map(|o| o.ppb).collect();
        assert_eq!(ppb, vec![594_000_000, 297_000_000, 99_000_000]);
        assert_eq!(p.outputs[0].script, s(1));
        check_plan(&p, &rules()).unwrap();
        assert!(p.outputs.iter().map(|o| o.ppb as u64).sum::<u64>() <= PPB - 10_000_000);
    }

    #[test]
    fn a_tiny_miner_is_paid_as_long_as_its_part_is_representable() {
        // 1 : 10^8 of the pool: 9.9 ppb of the reward, about 5 leptons at 5 coins
        let p = derive(&rules(), [1; 32], 100, &[(s(1), 100_000_000), (s(9), 1)]).unwrap();
        assert_eq!(p.outputs.len(), 2, "the small miner is in the plan");
        assert_eq!(p.outputs[1].script, s(9));
        assert_eq!(p.outputs[1].ppb, 9);
        // the committed root contains both
        assert_eq!(p.accounting_root, AccountingTree::new(vec![(s(1), 100_000_000), (s(9), 1)]).unwrap().root());
    }

    #[test]
    fn at_most_n_max_credits_are_all_paid_in_every_block() {
        let credits: Vec<_> = (1..=8u8).map(|i| (s(i), 1000 * i as u64)).collect();
        for height in 0..40 {
            let p = derive(&rules(), [1; 32], height, &credits).unwrap();
            assert_eq!(p.outputs.len(), 8, "height {height}");
        }
    }

    #[test]
    fn more_credits_than_outputs_take_turns_and_everybody_is_paid_once_per_cycle() {
        let r = Rules { n_max: 16, fee_ppb: 10_000_000, fee_script: vec![0, 20, 0xfe] };
        let credits: Vec<_> = (0..100u16).map(|i| (big(i), 1000 + i as u64)).collect();
        let mut paid: HashMap<Vec<u8>, u32> = HashMap::new();
        let mut regular_every_block: Option<Vec<Vec<u8>>> = None;
        let groups = 96 / 4 + 1; // tail of 100 - 12 = 88 entries, turn slots 4 -> 22 groups
        for height in 1000..1000 + 22u32 {
            let p = derive(&r, [1; 32], height, &credits).unwrap();
            assert!(p.outputs.len() <= 16, "{} outputs", p.outputs.len());
            check_plan(&p, &r).unwrap();
            for o in &p.outputs {
                *paid.entry(o.script.clone()).or_default() += 1;
            }
            let here: Vec<Vec<u8>> = p.outputs.iter().map(|o| o.script.clone()).collect();
            regular_every_block = Some(match regular_every_block {
                None => here,
                Some(prev) => prev.into_iter().filter(|x| here.contains(x)).collect(),
            });
        }
        let _ = groups;
        assert_eq!(regular_every_block.unwrap().len(), 12, "the 12 largest credits are in every block");
        assert_eq!(paid.len(), 100, "every credit is paid within one cycle");
        let tail_paid: Vec<u32> = paid.values().copied().filter(|n| *n < 22).collect();
        assert_eq!(tail_paid.len(), 88);
        assert!(tail_paid.iter().all(|n| *n == 1), "each member of the tail is paid exactly once per cycle");
    }

    #[test]
    fn over_a_cycle_the_turn_taking_tail_gets_what_it_has_earned() {
        let r = Rules { n_max: 16, fee_ppb: 0, fee_script: vec![0, 20, 0xfe] };
        // 12 regular miners with 10,000 each, 88 tail miners with 100 each: total 128,800
        let mut credits: Vec<_> = (0..12u16).map(|i| (big(i), 10_000)).collect();
        credits.extend((12..100u16).map(|i| (big(i), 100)));
        let mut sum: HashMap<Vec<u8>, u128> = HashMap::new();
        for height in 0..22u32 {
            for o in derive(&r, [1; 32], height, &credits).unwrap().outputs {
                *sum.entry(o.script).or_default() += o.ppb as u128;
            }
        }
        let expected = |c: u128| c * PPB as u128 / 128_800;
        // a tail miner is paid once per cycle, with 22 times its weight: over the 22 blocks it gets what it would get in 22 blocks of full payment
        let tail = sum[&big(50)] as f64;
        let want = 22.0 * expected(100) as f64;
        assert!((tail / want - 1.0).abs() < 0.12, "tail got {tail}, expected about {want}");
        let regular = sum[&big(3)] as f64 / 22.0;
        let want_r = expected(10_000) as f64;
        assert!((regular / want_r - 1.0).abs() < 0.12, "regular got {regular} per block, expected about {want_r}");
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
        a.outputs[0].ppb += 1;
        assert!(check_plan(&a, &rules()).is_err());
        let mut b = p.clone();
        b.outputs[0].script = s(77);
        assert!(check_plan(&b, &rules()).is_err());
        let mut c = p.clone();
        c.fee_ppb = 0;
        c.plan_hash = hash(&c);
        assert!(check_plan(&c, &rules()).is_err(), "fee terms must match the rules the gateway agreed to");
        let mut d = p.clone();
        d.outputs.reverse();
        d.plan_hash = hash(&d);
        assert!(check_plan(&d, &rules()).is_err());
        let mut e = p;
        e.outputs[0].ppb = 999_999_999;
        e.plan_hash = hash(&e);
        assert!(check_plan(&e, &rules()).is_err());
    }

    #[test]
    fn a_gateway_refuses_a_plan_larger_than_it_accepts() {
        let credits: Vec<_> = (0..6u8).map(|i| (s(i), 100)).collect();
        let p = derive(&rules(), [1; 32], 100, &credits).unwrap();
        let mut small = rules();
        small.n_max = 5;
        assert!(check_plan(&p, &small).is_err());
    }

    #[test]
    fn a_large_window_stays_within_limits() {
        let mut r = rules();
        r.n_max = 200;
        let credits: Vec<_> = (0..5000u32).map(|i| (vec![0, 20, (i % 251) as u8, (i / 251) as u8], 1 + (i as u64 * 7919) % 10_000_000)).collect();
        let p = derive(&r, [3; 32], 5, &credits).unwrap();
        assert!(p.outputs.len() <= 200);
        check_plan(&p, &r).unwrap();
        assert!(p.outputs.iter().map(|o| o.ppb as u64).sum::<u64>() <= PPB - r.fee_ppb as u64);
    }
}
