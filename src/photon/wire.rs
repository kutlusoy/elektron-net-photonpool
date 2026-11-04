//! Frame codec and the primitive reader/writer used by all messages.
//! Frame: `ext_type u16 | msg_type u8 | length u24 | payload`, little-endian, payload capped.
use anyhow::{anyhow, bail, Result};

pub const HEADER_LEN: usize = 6;
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub ext: u16,
    pub ty: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn encode(&self, max: usize) -> Result<Vec<u8>> {
        if self.payload.len() > max.min(0xff_ffff) {
            bail!("frame payload of {} bytes exceeds the limit {}", self.payload.len(), max);
        }
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.ext.to_le_bytes());
        out.push(self.ty);
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes()[..3]);
        out.extend_from_slice(&self.payload);
        Ok(out)
    }
}

pub fn parse_header(h: &[u8]) -> (u16, u8, usize) {
    (u16::from_le_bytes([h[0], h[1]]), h[2], u32::from_le_bytes([h[3], h[4], h[5], 0]) as usize)
}

/// Incremental decoder; rejects an oversized frame as soon as its header is complete.
pub struct Decoder {
    buf: Vec<u8>,
    max: usize,
}

impl Decoder {
    pub fn new(max: usize) -> Self {
        Self { buf: Vec::new(), max }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn next(&mut self) -> Result<Option<Frame>> {
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let (ext, ty, len) = parse_header(&self.buf);
        if len > self.max {
            bail!("frame length {len} exceeds the limit {}", self.max);
        }
        if self.buf.len() < HEADER_LEN + len {
            return Ok(None);
        }
        let payload = self.buf[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.buf.drain(..HEADER_LEN + len);
        Ok(Some(Frame { ext, ty, payload }))
    }
}

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn h32(&mut self, v: &[u8; 32]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }
    /// Byte string with a u16 length prefix.
    pub fn bytes16(&mut self, v: &[u8]) -> &mut Self {
        self.u16(v.len() as u16);
        self.buf.extend_from_slice(v);
        self
    }
    /// Byte string with a u32 length prefix.
    pub fn bytes32(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32);
        self.buf.extend_from_slice(v);
        self
    }
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.buf.len()).ok_or_else(|| anyhow!("message truncated"))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn h32(&mut self) -> Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    pub fn bytes16(&mut self, max: usize) -> Result<Vec<u8>> {
        let n = self.u16()? as usize;
        if n > max {
            bail!("byte string of {n} exceeds the limit {max}");
        }
        Ok(self.take(n)?.to_vec())
    }
    pub fn bytes32(&mut self, max: usize) -> Result<Vec<u8>> {
        let n = self.u32()? as usize;
        if n > max {
            bail!("byte string of {n} exceeds the limit {max}");
        }
        Ok(self.take(n)?.to_vec())
    }
    /// A count that must fit both the limit and the bytes left (`each` is the minimum size of an element).
    pub fn count(&mut self, max: usize, each: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > max || n.saturating_mul(each) > self.remaining() {
            bail!("element count {n} exceeds the limit {max} or the message");
        }
        Ok(n)
    }
    pub fn finish(self) -> Result<()> {
        if self.pos != self.buf.len() {
            bail!("{} trailing bytes", self.buf.len() - self.pos);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_and_split_delivery() {
        let f = Frame { ext: 0x1234, ty: 7, payload: (0..200u8).collect() };
        let bytes = f.encode(MAX_FRAME).unwrap();
        assert_eq!(&bytes[..6], &[0x34, 0x12, 7, 200, 0, 0]);
        let mut d = Decoder::new(MAX_FRAME);
        for b in &bytes {
            assert!(d.next().unwrap().is_none() || false);
            d.push(&[*b]);
        }
        assert_eq!(d.next().unwrap().unwrap(), f);
        assert!(d.next().unwrap().is_none());
    }

    #[test]
    fn two_frames_in_one_chunk() {
        let a = Frame { ext: 0, ty: 1, payload: vec![] };
        let b = Frame { ext: 0, ty: 2, payload: vec![9; 10] };
        let mut d = Decoder::new(MAX_FRAME);
        d.push(&[a.encode(MAX_FRAME).unwrap(), b.encode(MAX_FRAME).unwrap()].concat());
        assert_eq!(d.next().unwrap().unwrap(), a);
        assert_eq!(d.next().unwrap().unwrap(), b);
    }

    #[test]
    fn oversized_frames_are_refused_at_the_header() {
        let mut d = Decoder::new(1000);
        d.push(&[0, 0, 1, 0xe9, 0x03, 0]); // length 1001, no payload yet
        assert!(d.next().is_err());
        assert!(Frame { ext: 0, ty: 0, payload: vec![0; 1001] }.encode(1000).is_err());
    }

    #[test]
    fn reader_rejects_truncation_trailing_bytes_and_huge_counts() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert!(r.u32().is_err());
        let mut r = Reader::new(&[1, 2]);
        r.u8().unwrap();
        assert!(r.finish().is_err());
        let mut w = Writer::default();
        w.u32(u32::MAX);
        let mut r = Reader::new(&w.buf);
        assert!(r.count(100, 1).is_err());
        let mut w = Writer::default();
        w.u32(5).u8(0);
        assert!(Reader::new(&w.buf).count(100, 4).is_err()); // five 4-byte elements do not fit in one byte
    }
}
