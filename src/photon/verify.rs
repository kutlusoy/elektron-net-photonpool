//! Share verification on the pool side and the adaptive attestation spot check.
use super::coinbase::{check_announced, parse, ParsedCoinbase};
use super::msg::{reject, PayoutPlan, ShareItem, TemplateAnnounce};
use crate::template::{header_hash, meets_target};
use crate::tx::{merkle_root, sha256d};
use anyhow::{bail, Result};
use std::collections::HashSet;

/// BIP 320 version rolling bits.
pub const VERSION_MASK: u32 = 0x1fff_e000;
/// Largest accepted ntime advance over the announced curtime.
pub const MAX_NTIME_DELTA: u32 = 7200;

pub fn bits_to_target(bits: u32) -> [u8; 32] {
    let (exp, mant) = ((bits >> 24) as usize, bits & 0x007f_ffff);
    let mut t = [0u8; 32];
    if mant == 0 || bits & 0x0080_0000 != 0 {
        return t;
    }
    let m = mant.to_be_bytes();
    for (i, b) in m[1..].iter().enumerate() {
        let pos = 32usize as isize - exp as isize + i as isize;
        if (0..32).contains(&pos) {
            t[pos as usize] = *b;
        }
    }
    t
}

/// An announced template whose coinbases were checked against the plan.
pub struct Announced {
    pub ann: TemplateAnnounce,
    pub txids: Vec<[u8; 32]>,
    pub reward: u64,
    pub network_target: [u8; 32],
}

impl Announced {
    /// Validates the announce against the current tip and plan. `max_reward` bounds subsidy plus plausible fees.
    pub fn new(ann: TemplateAnnounce, tip: &[u8; 32], plan: &PayoutPlan, max_reward: u64) -> Result<Self> {
        if ann.tip != *tip || ann.height != plan.height {
            bail!("announce is not for the current tip and the plan of its height");
        }
        if ann.coinbases.is_empty() {
            bail!("announce without variants");
        }
        let mut reward = None;
        let mut txids = Vec::new();
        for raw in &ann.coinbases {
            let cb: ParsedCoinbase = parse(raw)?;
            let r = check_announced(plan, &cb, max_reward)?;
            if *reward.get_or_insert(r) != r {
                bail!("variants disagree on the reward");
            }
            txids.push(sha256d(raw));
        }
        let network_target = bits_to_target(ann.bits);
        Ok(Self { reward: reward.unwrap(), network_target, txids, ann })
    }

    pub fn header(&self, item: &ShareItem) -> Result<[u8; 80]> {
        let idx = item.variant.checked_sub(self.ann.first_variant).filter(|i| (*i as usize) < self.txids.len());
        let Some(idx) = idx else { bail!("unknown variant") };
        let root = merkle_root(&self.txids[idx as usize], &self.ann.branch);
        let version = (self.ann.version & !VERSION_MASK) | (item.version_bits & VERSION_MASK);
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&version.to_le_bytes());
        h[4..36].copy_from_slice(&self.ann.tip);
        h[36..68].copy_from_slice(&root);
        h[68..72].copy_from_slice(&self.ann.curtime.saturating_add(item.ntime_delta).to_le_bytes());
        h[72..76].copy_from_slice(&self.ann.bits.to_le_bytes());
        h[76..80].copy_from_slice(&item.nonce.to_le_bytes());
        Ok(h)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Reject(u8),
    /// Meets the channel's share target.
    Share { hash: [u8; 32], block: bool },
}

/// Per-channel duplicate filter, cleared on every new tip.
#[derive(Default)]
pub struct Seen(HashSet<(u32, ShareItem)>);

impl Seen {
    pub fn clear(&mut self) {
        self.0.clear();
    }
}

pub fn check_share(a: &Announced, template_id: u32, item: &ShareItem, share_target: &[u8; 32], seen: &mut Seen) -> Verdict {
    if item.ntime_delta > MAX_NTIME_DELTA {
        return Verdict::Reject(reject::BAD_TIME);
    }
    let Ok(hdr) = a.header(item) else { return Verdict::Reject(reject::UNKNOWN_VARIANT) };
    let hash = header_hash(&hdr);
    let block = meets_target(&hash, &a.network_target);
    if !block && !meets_target(&hash, share_target) {
        return Verdict::Reject(reject::LOW_DIFFICULTY);
    }
    if !seen.0.insert((template_id, *item)) {
        return Verdict::Reject(reject::DUPLICATE);
    }
    Verdict::Share { hash, block }
}

/// Trust score of a gateway: how often its announced variants are validated by the node.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Trust {
    pub passes: u32,
    pub fails: u32,
}

impl Trust {
    pub const BAN_AFTER: u32 = 3;

    /// Probability that the next announced variant is validated by the node: starts at 0.5, decays with clean checks, jumps after failures.
    pub fn probability(&self) -> f64 {
        let base = (0.5 / (1.0 + self.passes as f64 / 4.0)).max(0.02);
        (base * 4f64.powi(self.fails.min(16) as i32)).min(1.0)
    }

    pub fn should_check(&self, roll: f64) -> bool {
        roll < self.probability()
    }

    pub fn record(&mut self, passed: bool) {
        if passed {
            self.passes = self.passes.saturating_add(1);
        } else {
            self.fails = self.fails.saturating_add(1);
        }
    }

    pub fn banned(&self) -> bool {
        self.fails >= Self::BAN_AFTER
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::photon::coinbase::{commitment_script, split, WITNESS_MAGIC};
    use crate::photon::plan::{derive, Rules};
    use crate::tx::{build_coinbase, merkle_branch, TxOutput};

    fn setup(variants: usize, bits: u32) -> (PayoutPlan, Announced) {
        let rules = Rules { n_max: 8, min_ppm: 100, min_output_leptons: 0, fee_ppm: 10_000, fee_script: vec![0, 20, 0xfe] };
        let tip = [5u8; 32];
        let plan = derive(&rules, tip, 1001, 5_000_000_000, &[(vec![0, 20, 1], 700), (vec![0, 20, 2], 300)]).unwrap();
        let reward = 5_000_000_000u64;
        let (vals, fee) = split(reward, &plan);
        let others: Vec<[u8; 32]> = (0..5u8).map(|i| sha256d(&[i])).collect();
        let mut coinbases = vec![];
        for v in 0..variants {
            let mut outs: Vec<TxOutput> = plan.outputs.iter().zip(&vals).map(|(o, x)| TxOutput { value: *x as i64, script: o.script.clone() }).collect();
            outs.push(TxOutput { value: fee as i64, script: plan.fee_script.clone() });
            let mut att = vec![0x6a, 0x02, 0xe9, 0x03, 0x20];
            att.extend_from_slice(&sha256d(&[v as u8]));
            let mut wc = WITNESS_MAGIC.to_vec();
            wc.extend_from_slice(&[1u8; 32]);
            outs.push(TxOutput { value: 0, script: att });
            outs.push(TxOutput { value: 0, script: wc });
            outs.push(TxOutput { value: 0, script: commitment_script(&plan.accounting_root) });
            coinbases.push(build_coinbase(&[0x02, 0xe9, 0x03, v as u8, 0], 1001, &outs).nowit);
        }
        let ann = TemplateAnnounce { tip, template_id: 9, height: 1001, version: 0x2000_0000, bits, curtime: 1_700_000_000, branch: merkle_branch(&others), first_variant: 100, coinbases };
        let a = Announced::new(ann, &tip, &plan, reward).unwrap();
        (plan, a)
    }

    fn easy_target() -> [u8; 32] {
        let mut t = [0xffu8; 32];
        t[0] = 0x0f;
        t
    }

    #[test]
    fn compact_bits_decode_like_the_node() {
        assert_eq!(hex::encode(bits_to_target(0x1d00ffff)), "00000000ffff0000000000000000000000000000000000000000000000000000");
        assert_eq!(hex::encode(bits_to_target(0x207fffff)), "7fffff0000000000000000000000000000000000000000000000000000000000");
        assert_eq!(bits_to_target(0x1d800000), [0; 32], "negative compact values give a zero target");
    }

    #[test]
    fn a_share_is_checked_against_the_announced_coinbase_and_is_not_countable_twice() {
        let (_, a) = setup(3, 0x1d00ffff);
        let mut seen = Seen::default();
        let t = easy_target();
        let mut accepted = 0;
        for nonce in 0..512u32 {
            let item = ShareItem { variant: 101, nonce, ntime_delta: 1, version_bits: 0 };
            match check_share(&a, 9, &item, &t, &mut seen) {
                Verdict::Share { hash, block } => {
                    accepted += 1;
                    assert!(!block);
                    assert_eq!(hash, header_hash(&a.header(&item).unwrap()));
                    assert_eq!(check_share(&a, 9, &item, &t, &mut seen), Verdict::Reject(reject::DUPLICATE));
                }
                Verdict::Reject(c) => assert_eq!(c, reject::LOW_DIFFICULTY),
            }
        }
        assert!(accepted > 5 && accepted < 200, "accepted {accepted}");
    }

    #[test]
    fn variants_time_and_unknowns_are_handled() {
        let (_, a) = setup(2, 0x207fffff);
        let mut seen = Seen::default();
        let t = [0xffu8; 32];
        assert_eq!(check_share(&a, 9, &ShareItem { variant: 99, nonce: 0, ntime_delta: 0, version_bits: 0 }, &t, &mut seen), Verdict::Reject(reject::UNKNOWN_VARIANT));
        assert_eq!(check_share(&a, 9, &ShareItem { variant: 102, nonce: 0, ntime_delta: 0, version_bits: 0 }, &t, &mut seen), Verdict::Reject(reject::UNKNOWN_VARIANT));
        assert_eq!(check_share(&a, 9, &ShareItem { variant: 100, nonce: 0, ntime_delta: MAX_NTIME_DELTA + 1, version_bits: 0 }, &t, &mut seen), Verdict::Reject(reject::BAD_TIME));
        let (mut blocks, mut shares) = (0, 0);
        for nonce in 1..200u32 {
            match check_share(&a, 9, &ShareItem { variant: 100, nonce, ntime_delta: 0, version_bits: 0 }, &t, &mut seen) {
                Verdict::Share { block, .. } => { shares += 1; blocks += block as u32; }
                v => panic!("{v:?}"),
            }
        }
        assert_eq!(shares, 199, "an all-ones share target accepts everything");
        assert!(blocks > 40 && blocks < 160, "about half of the hashes meet the easy network target, got {blocks}");
        // different variants give different headers
        let h0 = a.header(&ShareItem { variant: 100, nonce: 5, ntime_delta: 0, version_bits: 0 }).unwrap();
        let h1 = a.header(&ShareItem { variant: 101, nonce: 5, ntime_delta: 0, version_bits: 0 }).unwrap();
        assert_ne!(h0[36..68], h1[36..68]);
        // version rolling is limited to the mask
        let hv = a.header(&ShareItem { variant: 100, nonce: 5, ntime_delta: 0, version_bits: 0xffff_ffff }).unwrap();
        assert_eq!(u32::from_le_bytes(hv[0..4].try_into().unwrap()), 0x2000_0000 | VERSION_MASK);
    }

    #[test]
    fn announces_with_a_wrong_tip_or_a_deviating_coinbase_are_refused() {
        let (plan, a) = setup(2, 0x1d00ffff);
        let mut wrong_tip = a.ann.clone();
        wrong_tip.tip = [6; 32];
        assert!(Announced::new(wrong_tip, &[5; 32], &plan, 5_000_000_000).is_err());
        let mut bad = a.ann.clone();
        // byte 53 is inside the first payout value; the attestation digest itself can only be checked by the node (spot check)
        bad.coinbases[1][53] ^= 1;
        assert!(Announced::new(bad, &[5; 32], &plan, 5_000_000_000).is_err());
        let mut none = a.ann.clone();
        none.coinbases.clear();
        assert!(Announced::new(none, &[5; 32], &plan, 5_000_000_000).is_err());
    }

    #[test]
    fn trust_probability_decays_with_clean_checks_and_jumps_after_failures() {
        let mut t = Trust::default();
        assert_eq!(t.probability(), 0.5);
        for _ in 0..200 {
            t.record(true);
        }
        assert!((t.probability() - 0.02).abs() < 1e-9);
        t.record(false);
        assert!((t.probability() - 0.08).abs() < 1e-9);
        t.record(false);
        assert!(t.probability() > 0.3 && !t.banned());
        t.record(false);
        assert!(t.banned() && t.probability() == 1.0);
        assert!(t.should_check(0.99) && !Trust { passes: 200, fails: 0 }.should_check(0.5));
    }
}
