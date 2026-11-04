//! Accounting commitment: a binary Merkle tree over `(script, credit)` leaves.
//!
//! * leaf  = SHA256(0x00 || u16 script_len || script || u64 credit)
//! * node  = SHA256(0x01 || left || right); an odd node at the end of a level is promoted unchanged (no duplication)
//! * root  = SHA256(0x02 || u32 leaf_count || top), the empty tree has top = 32 zero bytes
//!
//! Leaves are sorted by script bytes, scripts are unique. All integers little-endian.
use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];

pub fn leaf_hash(script: &[u8], credit: u64) -> Hash {
    let mut h = Sha256::new();
    h.update([0u8]);
    h.update((script.len() as u16).to_le_bytes());
    h.update(script);
    h.update(credit.to_le_bytes());
    h.finalize().into()
}

fn node_hash(l: &Hash, r: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update([1u8]);
    h.update(l);
    h.update(r);
    h.finalize().into()
}

fn seal(count: u32, top: &Hash) -> Hash {
    let mut h = Sha256::new();
    h.update([2u8]);
    h.update(count.to_le_bytes());
    h.update(top);
    h.finalize().into()
}

pub struct AccountingTree {
    leaves: Vec<(Vec<u8>, u64)>,
    levels: Vec<Vec<Hash>>,
}

impl AccountingTree {
    pub fn new(mut credits: Vec<(Vec<u8>, u64)>) -> Result<Self> {
        credits.sort_by(|a, b| a.0.cmp(&b.0));
        if credits.windows(2).any(|w| w[0].0 == w[1].0) {
            bail!("duplicate script in the accounting leaves");
        }
        if credits.iter().any(|(s, _)| s.len() > u16::MAX as usize) {
            bail!("script too long");
        }
        let mut levels = vec![credits.iter().map(|(s, c)| leaf_hash(s, *c)).collect::<Vec<_>>()];
        while levels.last().unwrap().len() > 1 {
            let prev = levels.last().unwrap();
            let next = prev.chunks(2).map(|p| if p.len() == 2 { node_hash(&p[0], &p[1]) } else { p[0] }).collect();
            levels.push(next);
        }
        Ok(Self { leaves: credits, levels })
    }

    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    pub fn root(&self) -> Hash {
        let top = self.levels.last().and_then(|l| l.first()).copied().unwrap_or([0u8; 32]);
        seal(self.leaves.len() as u32, &top)
    }

    pub fn index_of(&self, script: &[u8]) -> Option<usize> {
        self.leaves.binary_search_by(|(s, _)| s.as_slice().cmp(script)).ok()
    }

    pub fn credit(&self, index: usize) -> Option<u64> {
        self.leaves.get(index).map(|l| l.1)
    }

    pub fn proof(&self, index: usize) -> Option<Vec<Hash>> {
        if index >= self.leaves.len() {
            return None;
        }
        let (mut i, mut out) = (index, Vec::new());
        for level in &self.levels[..self.levels.len() - 1] {
            if !(i % 2 == 0 && i + 1 == level.len()) {
                out.push(level[i ^ 1]);
            }
            i /= 2;
        }
        Some(out)
    }
}

/// Verifies that `(script, credit)` is leaf `index` of a tree with `count` leaves and the given root.
pub fn verify(root: &Hash, count: u32, index: u32, script: &[u8], credit: u64, siblings: &[Hash]) -> bool {
    if index >= count || script.len() > u16::MAX as usize {
        return false;
    }
    let (mut acc, mut i, mut n, mut used) = (leaf_hash(script, credit), index as u64, count as u64, 0usize);
    while n > 1 {
        if !(i % 2 == 0 && i + 1 == n) {
            let Some(s) = siblings.get(used) else { return false };
            used += 1;
            acc = if i % 2 == 0 { node_hash(&acc, s) } else { node_hash(s, &acc) };
        }
        i /= 2;
        n = (n + 1) / 2;
    }
    used == siblings.len() && seal(count, &acc) == *root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credits(n: usize) -> Vec<(Vec<u8>, u64)> {
        (0..n).map(|i| (vec![0, 20, (i * 7 % 251) as u8, (i / 251) as u8], 1000 + i as u64 * 13)).collect()
    }

    #[test]
    fn every_leaf_proves_for_every_tree_size() {
        for n in 0..70 {
            let t = AccountingTree::new(credits(n)).unwrap();
            for i in 0..n {
                let (s, c) = (&t.leaves[i].0, t.leaves[i].1);
                let p = t.proof(i).unwrap();
                assert!(verify(&t.root(), n as u32, i as u32, s, c, &p), "n={n} i={i}");
                assert!(!verify(&t.root(), n as u32, i as u32, s, c + 1, &p), "credit tamper n={n} i={i}");
                if !p.is_empty() {
                    let mut bad = p.clone();
                    bad[0][0] ^= 1;
                    assert!(!verify(&t.root(), n as u32, i as u32, s, c, &bad));
                    assert!(!verify(&t.root(), n as u32, i as u32, s, c, &p[..p.len() - 1]));
                }
                let mut extra = p.clone();
                extra.push([0; 32]);
                assert!(!verify(&t.root(), n as u32, i as u32, s, c, &extra));
            }
            assert!(t.proof(n).is_none());
        }
    }

    #[test]
    fn root_is_order_independent_and_binds_count_and_content() {
        let a = credits(9);
        let mut b = a.clone();
        b.reverse();
        assert_eq!(AccountingTree::new(a.clone()).unwrap().root(), AccountingTree::new(b).unwrap().root());
        let mut c = a.clone();
        c[3].1 += 1;
        assert_ne!(AccountingTree::new(a.clone()).unwrap().root(), AccountingTree::new(c).unwrap().root());
        let mut d = a.clone();
        d.pop();
        assert_ne!(AccountingTree::new(a).unwrap().root(), AccountingTree::new(d).unwrap().root());
    }

    #[test]
    fn duplicate_scripts_are_refused_and_a_leaf_cannot_pose_as_a_node() {
        assert!(AccountingTree::new(vec![(vec![1], 1), (vec![1], 2)]).is_err());
        // two-leaf tree: its inner node must not verify as a single-leaf proof
        let t = AccountingTree::new(credits(2)).unwrap();
        let fake_script = [t.levels[0][0].as_slice(), t.levels[0][1].as_slice()].concat();
        assert!(!verify(&t.root(), 1, 0, &fake_script, 0, &[]));
    }

    #[test]
    fn empty_and_single_leaf_trees() {
        let e = AccountingTree::new(vec![]).unwrap();
        assert_eq!(e.root(), seal(0, &[0; 32]));
        let one = AccountingTree::new(credits(1)).unwrap();
        assert!(verify(&one.root(), 1, 0, &one.leaves[0].0, one.leaves[0].1, &[]));
        assert!(!verify(&one.root(), 0, 0, &one.leaves[0].0, one.leaves[0].1, &[]));
    }

    #[test]
    fn frozen_root_vector() {
        let t = AccountingTree::new(vec![(vec![0, 20, 1], 100), (vec![0, 20, 2], 200), (vec![0, 20, 3], 300)]).unwrap();
        assert_eq!(hex::encode(t.root()), FROZEN);
    }
    /// Computed independently in Python (hashlib) from the definition above.
    const FROZEN: &str = "6612ee8301340d35a390e4a84eda1d1023120649d9ae323cb2ae5cb189b9ac61";
}
