//! Photon protocol: framing, messages, Noise transport, payout plan, accounting commitment and share verification.
//! Specification: doc-elektron/guideline-photon-protocol.md
pub mod cli;
pub mod coinbase;
pub mod merkle;
pub mod msg;
pub mod noise;
pub mod plan;
pub mod verify;
pub mod wire;

pub const PROTOCOL_VERSION: u16 = 1;
