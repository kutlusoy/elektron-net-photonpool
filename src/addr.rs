use anyhow::{bail, Result};

pub fn script_for_address(addr: &str, hrp: &str) -> Result<Vec<u8>> {
    let (got, version, program) = bech32::segwit::decode(addr)?;
    if got.as_str() != hrp {
        bail!("address {addr} has prefix {}, expected {hrp}", got.as_str());
    }
    let v = version.to_u8();
    let mut script = vec![if v == 0 { 0x00 } else { 0x50 + v }, program.len() as u8];
    script.extend_from_slice(&program);
    Ok(script)
}
