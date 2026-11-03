use sha2::{Digest, Sha256};

pub fn sha256d(data: &[u8]) -> [u8; 32] {
    Sha256::digest(Sha256::digest(data)).into()
}

pub fn put_varint(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

#[derive(Clone)]
pub struct TxOutput {
    pub value: i64,
    pub script: Vec<u8>,
}

pub struct Coinbase {
    pub wit: Vec<u8>,
    pub txid: [u8; 32],
}

pub fn build_coinbase(script_sig: &[u8], height: u32, outputs: &[TxOutput]) -> Coinbase {
    let mut body = Vec::with_capacity(256);
    put_varint(&mut body, 1);
    body.extend_from_slice(&[0u8; 32]);
    body.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    put_varint(&mut body, script_sig.len() as u64);
    body.extend_from_slice(script_sig);
    body.extend_from_slice(&0xffff_fffeu32.to_le_bytes());
    put_varint(&mut body, outputs.len() as u64);
    for o in outputs {
        body.extend_from_slice(&o.value.to_le_bytes());
        put_varint(&mut body, o.script.len() as u64);
        body.extend_from_slice(&o.script);
    }
    let lock = (height - 1).to_le_bytes();
    let version = 2i32.to_le_bytes();

    let mut nowit = Vec::with_capacity(body.len() + 8);
    nowit.extend_from_slice(&version);
    nowit.extend_from_slice(&body);
    nowit.extend_from_slice(&lock);

    let mut wit = Vec::with_capacity(body.len() + 48);
    wit.extend_from_slice(&version);
    wit.extend_from_slice(&[0x00, 0x01]);
    wit.extend_from_slice(&body);
    wit.extend_from_slice(&[0x01, 0x20]);
    wit.extend_from_slice(&[0u8; 32]);
    wit.extend_from_slice(&lock);

    let txid = sha256d(&nowit);
    Coinbase { wit, txid }
}

fn hash_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(a);
    buf[32..].copy_from_slice(b);
    sha256d(&buf)
}

/// Merkle branch for the coinbase (index 0); `txids` are the other transactions in internal byte order.
pub fn merkle_branch(txids: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut level: Vec<[u8; 32]> = std::iter::once([0u8; 32]).chain(txids.iter().copied()).collect();
    let mut branch = Vec::new();
    while level.len() > 1 {
        branch.push(level[1]);
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        let mut next = vec![[0u8; 32]];
        for pair in level[2..].chunks(2) {
            next.push(hash_pair(&pair[0], &pair[1]));
        }
        level = next;
    }
    branch
}

pub fn merkle_root(coinbase_txid: &[u8; 32], branch: &[[u8; 32]]) -> [u8; 32] {
    branch.iter().fold(*coinbase_txid, |acc, s| hash_pair(&acc, s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_root(mut leaves: Vec<[u8; 32]>) -> [u8; 32] {
        while leaves.len() > 1 {
            if leaves.len() % 2 == 1 {
                leaves.push(*leaves.last().unwrap());
            }
            leaves = leaves.chunks(2).map(|p| hash_pair(&p[0], &p[1])).collect();
        }
        leaves[0]
    }

    #[test]
    fn branch_matches_full_merkle_tree() {
        for n in 0..40usize {
            let cb = sha256d(b"coinbase");
            let others: Vec<[u8; 32]> = (0..n).map(|i| sha256d(&(i as u64).to_le_bytes())).collect();
            let mut all = vec![cb];
            all.extend(others.iter().copied());
            assert_eq!(merkle_root(&cb, &merkle_branch(&others)), full_root(all), "n={n}");
        }
    }

    #[test]
    fn varint_boundaries() {
        for (n, len) in [(0u64, 1), (0xfc, 1), (0xfd, 3), (0xffff, 3), (0x1_0000, 5), (0xffff_ffff, 5), (0x1_0000_0000, 9)] {
            let mut v = Vec::new();
            put_varint(&mut v, n);
            assert_eq!(v.len(), len);
        }
    }
}
