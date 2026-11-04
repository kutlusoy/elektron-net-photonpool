//! Photon messages (ext_type 0). Strict decoding: caps on every collection, no trailing bytes.
use super::wire::{Frame, Reader, Writer};
use anyhow::{bail, Result};

pub const EXT_CORE: u16 = 0;

pub const MAX_SCRIPT: usize = 10_000;
pub const MAX_PLAN_OUTPUTS: usize = 256;
pub const MAX_BRANCH: usize = 32;
pub const MAX_VARIANTS_PER_ANNOUNCE: usize = 64;
pub const MAX_COINBASE: usize = 24_000;
pub const MAX_SHARES_PER_MSG: usize = 2048;
pub const MAX_REJECTS: usize = 4096;
pub const MAX_SIBLINGS: usize = 40;
pub const MAX_TX_CHUNK: usize = 60_000;

pub mod reject {
    pub const STALE: u8 = 1;
    pub const DUPLICATE: u8 = 2;
    pub const LOW_DIFFICULTY: u8 = 3;
    pub const UNKNOWN_TEMPLATE: u8 = 4;
    pub const BAD_COINBASE: u8 = 5;
    pub const UNKNOWN_VARIANT: u8 = 6;
    pub const BAD_TIME: u8 = 7;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    pub version: u16,
    pub features: u32,
    pub max_frame: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenChannel {
    pub request: u32,
    pub payout_script: Vec<u8>,
    pub target: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenChannelAck {
    pub request: u32,
    pub channel: u32,
    pub target: [u8; 32],
    pub plan_height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTip {
    pub tip: [u8; 32],
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanOutput {
    pub script: Vec<u8>,
    pub ppm: u32,
}

/// Plan for block `height`. `tip` is the block the plan was derived on: normally block `height - 2` (known one block ahead), or `height - 1` for a plan that had to be derived late (startup, reorg).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayoutPlan {
    pub tip: [u8; 32],
    pub height: u32,
    pub rules_version: u16,
    pub plan_hash: [u8; 32],
    pub accounting_root: [u8; 32],
    pub outputs: Vec<PlanOutput>,
    pub fee_script: Vec<u8>,
    pub fee_ppm: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateAnnounce {
    pub tip: [u8; 32],
    pub template_id: u32,
    pub height: u32,
    pub version: u32,
    pub bits: u32,
    pub curtime: u32,
    pub branch: Vec<[u8; 32]>,
    pub first_variant: u32,
    /// Non-witness coinbase serialisations, one per variant.
    pub coinbases: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShareItem {
    pub variant: u32,
    pub nonce: u32,
    pub ntime_delta: u32,
    pub version_bits: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shares {
    pub channel: u32,
    pub template_id: u32,
    pub items: Vec<ShareItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareAck {
    pub channel: u32,
    pub template_id: u32,
    pub accepted: u32,
    /// (index into the `Shares` message, reject code)
    pub rejects: Vec<(u32, u8)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetTarget {
    pub channel: u32,
    pub target: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockFound {
    pub template_id: u32,
    pub variant: u32,
    pub height: u32,
    pub hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochCommit {
    pub height: u32,
    pub root: [u8; 32],
    pub leaves: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetProof {
    pub height: u32,
    pub script: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    pub height: u32,
    pub root: [u8; 32],
    pub leaves: u32,
    pub index: u32,
    pub credit: u64,
    pub siblings: Vec<[u8; 32]>,
}

/// Pool asks the gateway for the transactions of one announced variant (attestation spot check).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetTransactions {
    pub template_id: u32,
    pub variant: u32,
}

/// One chunk of the transaction stream: records of `u32 length || raw transaction`, in block order, coinbase excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transactions {
    pub template_id: u32,
    pub variant: u32,
    pub seq: u32,
    pub last: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reject {
    pub code: u16,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Setup(Setup),
    SetupAck(Setup),
    OpenChannel(OpenChannel),
    OpenChannelAck(OpenChannelAck),
    NewTip(NewTip),
    PayoutPlan(PayoutPlan),
    TemplateAnnounce(TemplateAnnounce),
    Shares(Shares),
    ShareAck(ShareAck),
    SetTarget(SetTarget),
    BlockFound(BlockFound),
    EpochCommit(EpochCommit),
    GetProof(GetProof),
    Proof(Proof),
    Reject(Reject),
    Disconnect(Reject),
    GetTransactions(GetTransactions),
    Transactions(Transactions),
}

impl Msg {
    pub fn type_id(&self) -> u8 {
        match self {
            Msg::Setup(_) => 0x00,
            Msg::SetupAck(_) => 0x01,
            Msg::OpenChannel(_) => 0x02,
            Msg::OpenChannelAck(_) => 0x03,
            Msg::NewTip(_) => 0x04,
            Msg::PayoutPlan(_) => 0x05,
            Msg::TemplateAnnounce(_) => 0x06,
            Msg::Shares(_) => 0x07,
            Msg::ShareAck(_) => 0x08,
            Msg::SetTarget(_) => 0x09,
            Msg::BlockFound(_) => 0x0a,
            Msg::EpochCommit(_) => 0x0b,
            Msg::GetProof(_) => 0x0c,
            Msg::Proof(_) => 0x0d,
            Msg::Reject(_) => 0x0e,
            Msg::Disconnect(_) => 0x0f,
            Msg::GetTransactions(_) => 0x10,
            Msg::Transactions(_) => 0x11,
        }
    }

    pub fn encode(&self) -> Frame {
        let mut w = Writer::default();
        match self {
            Msg::Setup(m) | Msg::SetupAck(m) => {
                w.u16(m.version).u32(m.features).u32(m.max_frame);
            }
            Msg::OpenChannel(m) => {
                w.u32(m.request).bytes16(&m.payout_script).h32(&m.target);
            }
            Msg::OpenChannelAck(m) => {
                w.u32(m.request).u32(m.channel).h32(&m.target).u32(m.plan_height);
            }
            Msg::NewTip(m) => {
                w.h32(&m.tip).u32(m.height);
            }
            Msg::PayoutPlan(m) => {
                w.h32(&m.tip).u32(m.height).u16(m.rules_version).h32(&m.plan_hash).h32(&m.accounting_root);
                w.u32(m.outputs.len() as u32);
                for o in &m.outputs {
                    w.bytes16(&o.script).u32(o.ppm);
                }
                w.bytes16(&m.fee_script).u32(m.fee_ppm);
            }
            Msg::TemplateAnnounce(m) => {
                w.h32(&m.tip).u32(m.template_id).u32(m.height).u32(m.version).u32(m.bits).u32(m.curtime);
                w.u32(m.branch.len() as u32);
                for b in &m.branch {
                    w.h32(b);
                }
                w.u32(m.first_variant).u32(m.coinbases.len() as u32);
                for c in &m.coinbases {
                    w.bytes32(c);
                }
            }
            Msg::Shares(m) => {
                w.u32(m.channel).u32(m.template_id).u32(m.items.len() as u32);
                for i in &m.items {
                    w.u32(i.variant).u32(i.nonce).u32(i.ntime_delta).u32(i.version_bits);
                }
            }
            Msg::ShareAck(m) => {
                w.u32(m.channel).u32(m.template_id).u32(m.accepted).u32(m.rejects.len() as u32);
                for (i, code) in &m.rejects {
                    w.u32(*i).u8(*code);
                }
            }
            Msg::SetTarget(m) => {
                w.u32(m.channel).h32(&m.target);
            }
            Msg::BlockFound(m) => {
                w.u32(m.template_id).u32(m.variant).u32(m.height).h32(&m.hash);
            }
            Msg::EpochCommit(m) => {
                w.u32(m.height).h32(&m.root).u32(m.leaves);
            }
            Msg::GetProof(m) => {
                w.u32(m.height).bytes16(&m.script);
            }
            Msg::Proof(m) => {
                w.u32(m.height).h32(&m.root).u32(m.leaves).u32(m.index).u64(m.credit).u32(m.siblings.len() as u32);
                for s in &m.siblings {
                    w.h32(s);
                }
            }
            Msg::Reject(m) | Msg::Disconnect(m) => {
                w.u16(m.code).bytes16(m.reason.as_bytes());
            }
            Msg::GetTransactions(m) => {
                w.u32(m.template_id).u32(m.variant);
            }
            Msg::Transactions(m) => {
                w.u32(m.template_id).u32(m.variant).u32(m.seq).u8(m.last as u8).bytes32(&m.data);
            }
        }
        Frame { ext: EXT_CORE, ty: self.type_id(), payload: w.buf }
    }

    pub fn decode(f: &Frame) -> Result<Msg> {
        if f.ext != EXT_CORE {
            bail!("unknown extension {}", f.ext);
        }
        let mut r = Reader::new(&f.payload);
        let m = match f.ty {
            0x00 | 0x01 => {
                let s = Setup { version: r.u16()?, features: r.u32()?, max_frame: r.u32()? };
                if f.ty == 0 { Msg::Setup(s) } else { Msg::SetupAck(s) }
            }
            0x02 => Msg::OpenChannel(OpenChannel { request: r.u32()?, payout_script: r.bytes16(MAX_SCRIPT)?, target: r.h32()? }),
            0x03 => Msg::OpenChannelAck(OpenChannelAck { request: r.u32()?, channel: r.u32()?, target: r.h32()?, plan_height: r.u32()? }),
            0x04 => Msg::NewTip(NewTip { tip: r.h32()?, height: r.u32()? }),
            0x05 => {
                let (tip, height, rules_version, plan_hash, accounting_root) = (r.h32()?, r.u32()?, r.u16()?, r.h32()?, r.h32()?);
                let n = r.count(MAX_PLAN_OUTPUTS, 6)?;
                let mut outputs = Vec::with_capacity(n);
                for _ in 0..n {
                    outputs.push(PlanOutput { script: r.bytes16(MAX_SCRIPT)?, ppm: r.u32()? });
                }
                Msg::PayoutPlan(PayoutPlan { tip, height, rules_version, plan_hash, accounting_root, outputs, fee_script: r.bytes16(MAX_SCRIPT)?, fee_ppm: r.u32()? })
            }
            0x06 => {
                let (tip, template_id, height, version, bits, curtime) = (r.h32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?);
                let n = r.count(MAX_BRANCH, 32)?;
                let branch = (0..n).map(|_| r.h32()).collect::<Result<Vec<_>>>()?;
                let first_variant = r.u32()?;
                let n = r.count(MAX_VARIANTS_PER_ANNOUNCE, 4)?;
                let coinbases = (0..n).map(|_| r.bytes32(MAX_COINBASE)).collect::<Result<Vec<_>>>()?;
                Msg::TemplateAnnounce(TemplateAnnounce { tip, template_id, height, version, bits, curtime, branch, first_variant, coinbases })
            }
            0x07 => {
                let (channel, template_id) = (r.u32()?, r.u32()?);
                let n = r.count(MAX_SHARES_PER_MSG, 16)?;
                let items = (0..n).map(|_| Ok(ShareItem { variant: r.u32()?, nonce: r.u32()?, ntime_delta: r.u32()?, version_bits: r.u32()? })).collect::<Result<Vec<_>>>()?;
                Msg::Shares(Shares { channel, template_id, items })
            }
            0x08 => {
                let (channel, template_id, accepted) = (r.u32()?, r.u32()?, r.u32()?);
                let n = r.count(MAX_REJECTS, 5)?;
                let rejects = (0..n).map(|_| Ok((r.u32()?, r.u8()?))).collect::<Result<Vec<_>>>()?;
                Msg::ShareAck(ShareAck { channel, template_id, accepted, rejects })
            }
            0x09 => Msg::SetTarget(SetTarget { channel: r.u32()?, target: r.h32()? }),
            0x0a => Msg::BlockFound(BlockFound { template_id: r.u32()?, variant: r.u32()?, height: r.u32()?, hash: r.h32()? }),
            0x0b => Msg::EpochCommit(EpochCommit { height: r.u32()?, root: r.h32()?, leaves: r.u32()? }),
            0x0c => Msg::GetProof(GetProof { height: r.u32()?, script: r.bytes16(MAX_SCRIPT)? }),
            0x0d => {
                let (height, root, leaves, index, credit) = (r.u32()?, r.h32()?, r.u32()?, r.u32()?, r.u64()?);
                let n = r.count(MAX_SIBLINGS, 32)?;
                let siblings = (0..n).map(|_| r.h32()).collect::<Result<Vec<_>>>()?;
                Msg::Proof(Proof { height, root, leaves, index, credit, siblings })
            }
            0x0e | 0x0f => {
                let m = Reject { code: r.u16()?, reason: String::from_utf8(r.bytes16(256)?)? };
                if f.ty == 0x0e { Msg::Reject(m) } else { Msg::Disconnect(m) }
            }
            0x10 => Msg::GetTransactions(GetTransactions { template_id: r.u32()?, variant: r.u32()? }),
            0x11 => {
                let (template_id, variant, seq) = (r.u32()?, r.u32()?, r.u32()?);
                let last = match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => bail!("bad flag"),
                };
                Msg::Transactions(Transactions { template_id, variant, seq, last, data: r.bytes32(MAX_TX_CHUNK)? })
            }
            t => bail!("unknown message type {t}"),
        };
        r.finish()?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::photon::wire::MAX_FRAME;

    fn h(b: u8) -> [u8; 32] {
        [b; 32]
    }

    pub fn samples() -> Vec<Msg> {
        vec![
            Msg::Setup(Setup { version: 1, features: 0b11, max_frame: 65536 }),
            Msg::SetupAck(Setup { version: 1, features: 0b1, max_frame: 65536 }),
            Msg::OpenChannel(OpenChannel { request: 7, payout_script: vec![0, 20, 1, 2, 3], target: h(0x0f) }),
            Msg::OpenChannelAck(OpenChannelAck { request: 7, channel: 3, target: h(0x0e), plan_height: 1001 }),
            Msg::NewTip(NewTip { tip: h(1), height: 1000 }),
            Msg::PayoutPlan(PayoutPlan { tip: h(1), height: 1001, rules_version: 1, plan_hash: h(2), accounting_root: h(3), outputs: vec![PlanOutput { script: vec![0, 20, 9], ppm: 600_000 }, PlanOutput { script: vec![0, 20, 8], ppm: 350_000 }], fee_script: vec![0, 20, 7], fee_ppm: 10_000 }),
            Msg::TemplateAnnounce(TemplateAnnounce { tip: h(1), template_id: 5, height: 1001, version: 0x2000_0000, bits: 0x1d00ffff, curtime: 1_700_000_000, branch: vec![h(4), h(5)], first_variant: 10, coinbases: vec![vec![1; 100], vec![2; 120]] }),
            Msg::Shares(Shares { channel: 3, template_id: 5, items: vec![ShareItem { variant: 10, nonce: 1, ntime_delta: 2, version_bits: 3 }, ShareItem { variant: 11, nonce: 4, ntime_delta: 5, version_bits: 6 }] }),
            Msg::ShareAck(ShareAck { channel: 3, template_id: 5, accepted: 1, rejects: vec![(1, reject::DUPLICATE)] }),
            Msg::SetTarget(SetTarget { channel: 3, target: h(0x01) }),
            Msg::BlockFound(BlockFound { template_id: 5, variant: 10, height: 1001, hash: h(9) }),
            Msg::EpochCommit(EpochCommit { height: 1001, root: h(3), leaves: 42 }),
            Msg::GetProof(GetProof { height: 1001, script: vec![0, 20, 9] }),
            Msg::Proof(Proof { height: 1001, root: h(3), leaves: 42, index: 5, credit: 123_456, siblings: vec![h(6), h(7)] }),
            Msg::Reject(Reject { code: 12, reason: "stale".into() }),
            Msg::Disconnect(Reject { code: 1, reason: "bye".into() }),
            Msg::GetTransactions(GetTransactions { template_id: 5, variant: 11 }),
            Msg::Transactions(Transactions { template_id: 5, variant: 11, seq: 2, last: true, data: vec![1, 2, 3, 4] }),
        ]
    }

    #[test]
    fn every_message_round_trips_through_a_frame() {
        for m in samples() {
            let f = m.encode();
            let bytes = f.encode(MAX_FRAME).unwrap();
            let mut d = crate::photon::wire::Decoder::new(MAX_FRAME);
            d.push(&bytes);
            assert_eq!(Msg::decode(&d.next().unwrap().unwrap()).unwrap(), m, "{m:?}");
        }
    }

    #[test]
    fn truncation_and_trailing_bytes_are_refused_for_every_message() {
        for m in samples() {
            let f = m.encode();
            for cut in 0..f.payload.len() {
                let t = Frame { payload: f.payload[..cut].to_vec(), ..f.clone() };
                assert!(Msg::decode(&t).is_err(), "{m:?} cut at {cut}");
            }
            let mut long = f.clone();
            long.payload.push(0);
            assert!(Msg::decode(&long).is_err(), "{m:?} with trailing byte");
        }
    }

    #[test]
    fn unknown_type_and_extension_are_refused() {
        assert!(Msg::decode(&Frame { ext: 0, ty: 0x7f, payload: vec![] }).is_err());
        assert!(Msg::decode(&Frame { ext: 1, ty: 0, payload: vec![0; 10] }).is_err());
    }

    #[test]
    fn collection_limits_are_enforced() {
        let mut w = Writer::default();
        w.u32(1).u32(2).u32(MAX_SHARES_PER_MSG as u32 + 1);
        w.buf.extend(std::iter::repeat(0).take(16 * (MAX_SHARES_PER_MSG + 1)));
        assert!(Msg::decode(&Frame { ext: 0, ty: 0x07, payload: w.buf }).is_err());
    }

    /// Frozen wire vectors: a change here is a protocol change and needs a new version.
    #[test]
    fn frozen_test_vectors() {
        let f = Msg::Setup(Setup { version: 1, features: 3, max_frame: 65536 }).encode().encode(MAX_FRAME).unwrap();
        assert_eq!(hex::encode(&f), "0000000a0000010003000000" .to_owned() + "00000100");
        let f = Msg::NewTip(NewTip { tip: [0xab; 32], height: 258 }).encode().encode(MAX_FRAME).unwrap();
        assert_eq!(hex::encode(&f), format!("000004240000{}02010000", "ab".repeat(32)));
        let f = Msg::ShareAck(ShareAck { channel: 1, template_id: 2, accepted: 3, rejects: vec![(4, 2)] }).encode().encode(MAX_FRAME).unwrap();
        assert_eq!(hex::encode(&f), "000008150000".to_owned() + "01000000" + "02000000" + "03000000" + "01000000" + "04000000" + "02");
    }
}
