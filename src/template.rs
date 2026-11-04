use crate::tx::{build_coinbase, merkle_branch, merkle_root, put_varint, varint_len, TxOutput};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct Gbt {
    pub version: i32,
    pub previousblockhash: String,
    pub height: u32,
    pub coinbasevalue: i64,
    pub bits: String,
    pub target: String,
    pub curtime: u32,
    pub mintime: u32,
    pub transactions: Vec<GbtTx>,
    pub coinbase_required_outputs: Vec<GbtOut>,
    pub coinbase_script_sig_prefix: String,
}

#[derive(Deserialize)]
pub struct GbtTx {
    pub data: String,
    pub txid: String,
    #[serde(default)]
    pub fee: i64,
}

#[derive(Deserialize)]
pub struct GbtOut {
    pub value: i64,
    #[serde(rename = "scriptPubKey")]
    pub script_pub_key: String,
}

/// Transactions shared by all candidates built on the same mempool snapshot.
pub struct TxSet {
    pub datas: Vec<Vec<u8>>,
    pub txids: Vec<[u8; 32]>,
    pub branch: Vec<[u8; 32]>,
}

/// One immutable block candidate: exact coinbase, exact transaction set, fixed attestation.
pub struct Candidate {
    pub prev: [u8; 32],
    pub version: u32,
    pub bits: u32,
    pub target: [u8; 32],
    pub curtime: u32,
    pub mintime: u32,
    pub coinbase_value: i64,
    pub fees: i64,
    pub coinbase_wit: Vec<u8>,
    pub merkle_root: [u8; 32],
    pub txs: Arc<TxSet>,
    pub(crate) notify_b: String,
    pub(crate) notify_c: String,
}

fn rev32(hexstr: &str) -> Result<[u8; 32]> {
    let mut b: [u8; 32] = hex::decode(hexstr)?.try_into().map_err(|_| anyhow!("expected 32 bytes: {hexstr}"))?;
    b.reverse();
    Ok(b)
}

/// Stratum prevhash: internal byte order with every 4-byte word reversed.
fn stratum_prevhash(prev: &[u8; 32]) -> String {
    let mut out = [0u8; 32];
    for (i, w) in prev.chunks(4).enumerate() {
        for (j, b) in w.iter().rev().enumerate() {
            out[i * 4 + j] = *b;
        }
    }
    hex::encode(out)
}

impl TxSet {
    pub fn from_gbt(txs: &[GbtTx]) -> Result<Self> {
        let txids = txs.iter().map(|t| rev32(&t.txid)).collect::<Result<Vec<_>>>()?;
        Ok(Self { datas: txs.iter().map(|t| hex::decode(&t.data)).collect::<Result<Vec<_>, _>>()?, branch: merkle_branch(&txids), txids })
    }

    pub fn matches(&self, txs: &[GbtTx]) -> bool {
        self.txids.len() == txs.len() && self.txids.iter().zip(txs).all(|(a, b)| rev32(&b.txid).map(|x| &x == a).unwrap_or(false))
    }
}

/// Response of the Photon node patch `getattestationbatch`: one template, many coinbase variants.
#[derive(Deserialize)]
pub struct BatchTemplate {
    pub tip: String,
    pub height: u32,
    pub version: i32,
    pub bits: String,
    pub target: String,
    pub curtime: u32,
    pub mintime: u32,
    pub coinbasevalue: i64,
    pub coinbase_script_sig_prefix: String,
    pub witness_commitment_script: String,
    pub transactions: Vec<GbtTx>,
    pub candidates: Vec<BatchCand>,
}

#[derive(Deserialize)]
pub struct BatchCand {
    pub attestation_script: String,
    pub coinbase_hex: String,
}

impl Candidate {
    pub fn from_gbt(g: Gbt, payout_script: &[u8], reuse: Option<&Arc<TxSet>>) -> Result<Self> {
        if g.coinbase_required_outputs.len() != 2 {
            bail!("expected 2 required coinbase outputs, node sent {}", g.coinbase_required_outputs.len());
        }
        let sig = hex::decode(&g.coinbase_script_sig_prefix)?;
        let mut outputs = vec![TxOutput { value: g.coinbasevalue, script: payout_script.to_vec() }];
        for o in &g.coinbase_required_outputs {
            outputs.push(TxOutput { value: o.value, script: hex::decode(&o.script_pub_key)? });
        }
        let txs = match reuse {
            Some(r) if r.matches(&g.transactions) => r.clone(),
            _ => Arc::new(TxSet::from_gbt(&g.transactions)?),
        };
        let fees = g.transactions.iter().map(|t| t.fee).sum();
        Self::assemble(&g.previousblockhash, g.version, &g.bits, &g.target, g.curtime, g.mintime, g.coinbasevalue, fees, &sig, g.height, &outputs, txs).map(|(c, _)| c)
    }

    /// Candidate from a batch response. The coinbase is rebuilt locally and must equal the one the node returned.
    pub fn from_batch(bt: &BatchTemplate, txs: &Arc<TxSet>, cand: &BatchCand, payout_script: &[u8]) -> Result<Self> {
        let sig = hex::decode(&bt.coinbase_script_sig_prefix)?;
        let outputs = vec![
            TxOutput { value: bt.coinbasevalue, script: payout_script.to_vec() },
            TxOutput { value: 0, script: hex::decode(&cand.attestation_script)? },
            TxOutput { value: 0, script: hex::decode(&bt.witness_commitment_script)? },
        ];
        let fees = bt.transactions.iter().map(|t| t.fee).sum();
        let (c, nowit) = Self::assemble(&bt.tip, bt.version, &bt.bits, &bt.target, bt.curtime, bt.mintime, bt.coinbasevalue, fees, &sig, bt.height, &outputs, txs.clone())?;
        if hex::encode(nowit) != cand.coinbase_hex.to_ascii_lowercase() {
            bail!("coinbase built locally differs from the node's");
        }
        Ok(c)
    }

    /// Candidate for an arbitrary node-built coinbase (Photon gateway): the outputs are taken from the node's serialisation.
    pub fn from_coinbase(bt: &BatchTemplate, txs: &Arc<TxSet>, nowit: &[u8]) -> Result<Self> {
        let parsed = crate::photon::coinbase::parse(nowit)?;
        let outputs: Vec<TxOutput> = parsed.outputs.iter().map(|(v, s)| TxOutput { value: *v as i64, script: s.clone() }).collect();
        let fees = bt.transactions.iter().map(|t| t.fee).sum();
        let (c, rebuilt) = Self::assemble(&bt.tip, bt.version, &bt.bits, &bt.target, bt.curtime, bt.mintime, bt.coinbasevalue, fees, &parsed.script_sig, bt.height, &outputs, txs.clone())?;
        if rebuilt != nowit {
            bail!("coinbase rebuilt locally differs from the node's");
        }
        Ok(c)
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(prev_hex: &str, version: i32, bits_hex: &str, target_hex: &str, curtime: u32, mintime: u32, coinbase_value: i64, fees: i64, sig: &[u8], height: u32, outputs: &[TxOutput], txs: Arc<TxSet>) -> Result<(Self, Vec<u8>)> {
        let cb = build_coinbase(sig, height, outputs);
        let prev = rev32(prev_hex)?;
        let bits = u32::from_str_radix(bits_hex, 16).context("bad bits")?;
        let split = 4 + 1 + 36 + varint_len(sig.len()) + sig.len();
        let branch = txs.branch.iter().map(|h| format!("\"{}\"", hex::encode(h))).collect::<Vec<_>>().join(",");
        let notify_b = format!("\",\"{}\",\"{}\",\"{}\",[{}],\"", stratum_prevhash(&prev), hex::encode(&cb.nowit[..split]), hex::encode(&cb.nowit[split..]), branch);
        let notify_c = format!("\",\"{bits:08x}\",\"");
        let mut target = [0u8; 32];
        target.copy_from_slice(&hex::decode(target_hex)?);
        let nowit = cb.nowit.clone();
        Ok((
            Self {
                prev,
                version: version as u32,
                bits,
                target,
                curtime,
                mintime,
                coinbase_value,
                fees,
                coinbase_wit: cb.wit,
                merkle_root: merkle_root(&cb.txid, &txs.branch),
                txs,
                notify_b,
                notify_c,
            },
            nowit,
        ))
    }

    /// Appends a complete mining.notify line; the candidate-specific parts are pre-rendered.
    pub fn write_notify(&self, out: &mut String, job_id: u64, version: u32, ntime: u32, clean: bool) {
        let _ = write!(out, "{{\"id\":null,\"method\":\"mining.notify\",\"params\":[\"{job_id:x}{}{version:08x}{}{ntime:08x}\",{clean}]}}\n", self.notify_b, self.notify_c);
    }

    pub fn subsidy(&self) -> i64 {
        self.coinbase_value - self.fees
    }

    pub fn header(&self, version: u32, ntime: u32, nonce: u32) -> [u8; 80] {
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&version.to_le_bytes());
        h[4..36].copy_from_slice(&self.prev);
        h[36..68].copy_from_slice(&self.merkle_root);
        h[68..72].copy_from_slice(&ntime.to_le_bytes());
        h[72..76].copy_from_slice(&self.bits.to_le_bytes());
        h[76..80].copy_from_slice(&nonce.to_le_bytes());
        h
    }

    pub fn block_hex(&self, version: u32, ntime: u32, nonce: u32) -> String {
        let mut b = Vec::with_capacity(80 + self.coinbase_wit.len() + self.txs.datas.iter().map(Vec::len).sum::<usize>() + 9);
        b.extend_from_slice(&self.header(version, ntime, nonce));
        put_varint(&mut b, 1 + self.txs.datas.len() as u64);
        b.extend_from_slice(&self.coinbase_wit);
        for t in &self.txs.datas {
            b.extend_from_slice(t);
        }
        hex::encode(b)
    }
}

pub fn header_hash(header: &[u8; 80]) -> [u8; 32] {
    Sha256::digest(Sha256::digest(header)).into()
}

/// True when the block hash (little-endian digest) is at or below the big-endian target.
pub fn meets_target(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    for i in 0..32 {
        let (a, b) = (hash[31 - i], target[i]);
        if a != b {
            return a < b;
        }
    }
    true
}

/// Nonce search with the first 64 header bytes hashed once (midstate).
pub fn grind(c: &Candidate, version: u32, ntime: u32, target: &[u8; 32]) -> Option<u32> {
    let hdr = c.header(version, ntime, 0);
    let mut mid = Sha256::new();
    mid.update(&hdr[..64]);
    for nonce in 0..=u32::MAX {
        let mut h = mid.clone();
        h.update(&hdr[64..76]);
        h.update(nonce.to_le_bytes());
        let d: [u8; 32] = Sha256::digest(h.finalize()).into();
        if meets_target(&d, target) {
            return Some(nonce);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_comparison_is_big_endian_on_reversed_hash() {
        let mut target = [0u8; 32];
        target[0] = 0x01;
        let mut low = [0u8; 32];
        low[31] = 0x00;
        assert!(meets_target(&low, &target));
        let mut high = [0u8; 32];
        high[31] = 0x02;
        assert!(!meets_target(&high, &target));
        let mut equal = [0u8; 32];
        equal[31] = 0x01;
        assert!(meets_target(&equal, &target));
    }

    #[test]
    fn prevhash_words_match_the_stratum_convention() {
        let mut internal: [u8; 32] = hex::decode("00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048").unwrap().try_into().unwrap();
        internal.reverse();
        assert_eq!(stratum_prevhash(&internal), "18eb604820161bbf90947ee375428afcd76f411486ab5951839a8e6800000000");
    }
}
