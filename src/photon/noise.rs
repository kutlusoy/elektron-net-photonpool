//! Noise transport: `Noise_NX_25519_ChaChaPoly_SHA256`. The pool's static key is pinned by the gateway.
//!
//! Wire: every Noise message is prefixed with a u16 little-endian length. Photon frames are a byte stream
//! that is cut into Noise messages of at most 65,519 plaintext bytes.
use super::wire::{Decoder, Frame, MAX_FRAME};
use anyhow::{anyhow, bail, Result};
use snow::{Builder, HandshakeState, TransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PATTERN: &str = "Noise_NX_25519_ChaChaPoly_SHA256";
const PROLOGUE: &[u8] = b"photon/1";
const MAX_NOISE: usize = 65_535;
const CHUNK: usize = MAX_NOISE - 16;

pub struct Keypair {
    pub private: Vec<u8>,
    pub public: [u8; 32],
}

pub fn generate_keypair() -> Result<Keypair> {
    let kp = Builder::new(PATTERN.parse()?).generate_keypair()?;
    Ok(Keypair { public: kp.public.try_into().map_err(|_| anyhow!("unexpected key size"))?, private: kp.private })
}

pub struct NoiseConn<S> {
    io: S,
    st: TransportState,
    dec: Decoder,
    rbuf: Vec<u8>,
    pub remote_static: [u8; 32],
}

async fn read_msg<S: AsyncRead + Unpin>(io: &mut S) -> Result<Vec<u8>> {
    let mut l = [0u8; 2];
    io.read_exact(&mut l).await?;
    let mut buf = vec![0u8; u16::from_le_bytes(l) as usize];
    io.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_msg<S: AsyncWrite + Unpin>(io: &mut S, msg: &[u8]) -> Result<()> {
    io.write_all(&(msg.len() as u16).to_le_bytes()).await?;
    io.write_all(msg).await?;
    io.flush().await?;
    Ok(())
}

fn finish<S>(io: S, hs: HandshakeState, max_frame: usize) -> Result<NoiseConn<S>> {
    let remote: [u8; 32] = hs.get_remote_static().ok_or_else(|| anyhow!("peer sent no static key"))?.try_into().map_err(|_| anyhow!("bad key size"))?;
    Ok(NoiseConn { io, st: hs.into_transport_mode()?, dec: Decoder::new(max_frame), rbuf: Vec::new(), remote_static: remote })
}

/// Gateway side. With `pin`, the handshake fails unless the pool presents exactly that static key.
pub async fn connect<S: AsyncRead + AsyncWrite + Unpin>(mut io: S, pin: Option<[u8; 32]>) -> Result<NoiseConn<S>> {
    let mut hs = Builder::new(PATTERN.parse()?).prologue(PROLOGUE).build_initiator()?;
    let mut buf = vec![0u8; MAX_NOISE];
    let n = hs.write_message(&[], &mut buf)?;
    write_msg(&mut io, &buf[..n]).await?;
    let reply = read_msg(&mut io).await?;
    hs.read_message(&reply, &mut buf)?;
    if !hs.is_handshake_finished() {
        bail!("handshake incomplete");
    }
    let conn = finish(io, hs, MAX_FRAME)?;
    if let Some(p) = pin {
        if conn.remote_static != p {
            bail!("the pool's static key does not match the pinned key");
        }
    }
    Ok(conn)
}

/// Pool side.
pub async fn accept<S: AsyncRead + AsyncWrite + Unpin>(mut io: S, key: &Keypair) -> Result<NoiseConn<S>> {
    let mut hs = Builder::new(PATTERN.parse()?).prologue(PROLOGUE).local_private_key(&key.private).build_responder()?;
    let mut buf = vec![0u8; MAX_NOISE];
    let first = read_msg(&mut io).await?;
    hs.read_message(&first, &mut buf)?;
    let n = hs.write_message(&[], &mut buf)?;
    write_msg(&mut io, &buf[..n]).await?;
    if !hs.is_handshake_finished() {
        bail!("handshake incomplete");
    }
    // NX responder: the initiator has no static key, so none is reported
    let st = hs.into_transport_mode()?;
    Ok(NoiseConn { io, st, dec: Decoder::new(MAX_FRAME), rbuf: Vec::new(), remote_static: [0; 32] })
}

impl<S: AsyncRead + AsyncWrite + Unpin> NoiseConn<S> {
    pub async fn send(&mut self, frame: &Frame) -> Result<()> {
        let bytes = frame.encode(MAX_FRAME)?;
        let mut out = vec![0u8; MAX_NOISE];
        for chunk in bytes.chunks(CHUNK) {
            let n = self.st.write_message(chunk, &mut out)?;
            write_msg(&mut self.io, &out[..n]).await?;
        }
        Ok(())
    }

    /// Cancel-safe: the only await is a read into an internal buffer, so it may be used inside `select!`.
    pub async fn recv(&mut self) -> Result<Frame> {
        let mut plain = vec![0u8; MAX_NOISE];
        loop {
            if let Some(f) = self.dec.next()? {
                return Ok(f);
            }
            if self.rbuf.len() >= 2 {
                let len = u16::from_le_bytes([self.rbuf[0], self.rbuf[1]]) as usize;
                if self.rbuf.len() >= 2 + len {
                    let msg: Vec<u8> = self.rbuf.drain(..2 + len).skip(2).collect();
                    let n = self.st.read_message(&msg, &mut plain).map_err(|e| anyhow!("decryption failed: {e}"))?;
                    self.dec.push(&plain[..n]);
                    continue;
                }
            }
            if self.io.read_buf(&mut self.rbuf).await? == 0 {
                bail!("connection closed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::photon::msg::{Msg, NewTip, Setup};
    use tokio::io::duplex;

    async fn pair(pin_ok: bool) -> (Result<NoiseConn<tokio::io::DuplexStream>>, Result<NoiseConn<tokio::io::DuplexStream>>, Keypair) {
        let key = generate_keypair().unwrap();
        let (a, b) = duplex(1 << 20);
        let pin = if pin_ok { key.public } else { [9u8; 32] };
        let srv = tokio::spawn({
            let k = Keypair { private: key.private.clone(), public: key.public };
            async move { accept(b, &k).await }
        });
        let c = connect(a, Some(pin)).await;
        (c, srv.await.unwrap(), key)
    }

    #[tokio::test]
    async fn handshake_pins_the_pool_key_and_frames_flow_both_ways() {
        let (c, s, key) = pair(true).await;
        let (mut c, mut s) = (c.unwrap(), s.unwrap());
        assert_eq!(c.remote_static, key.public);
        c.send(&Msg::Setup(Setup { version: 1, features: 1, max_frame: 65536 }).encode()).await.unwrap();
        assert!(matches!(Msg::decode(&s.recv().await.unwrap()).unwrap(), Msg::Setup(_)));
        s.send(&Msg::NewTip(NewTip { tip: [3; 32], height: 5 }).encode()).await.unwrap();
        assert!(matches!(Msg::decode(&c.recv().await.unwrap()).unwrap(), Msg::NewTip(_)));
    }

    #[tokio::test]
    async fn a_wrong_pinned_key_fails_the_connect() {
        let (c, _s, _) = pair(false).await;
        assert!(c.is_err());
    }

    #[tokio::test]
    async fn frames_larger_than_one_noise_message_are_split_and_reassembled() {
        let (c, s, _) = pair(true).await;
        let (mut c, mut s) = (c.unwrap(), s.unwrap());
        let big = Frame { ext: 0, ty: 0x06, payload: (0..60_000u32).map(|i| (i % 251) as u8).collect() };
        let expect = big.clone();
        let send = tokio::spawn(async move { c.send(&big).await.unwrap(); c });
        assert_eq!(s.recv().await.unwrap(), expect);
        send.await.unwrap();
        // many frames back to back
        let (c, s, _) = pair(true).await;
        let (mut c, mut s) = (c.unwrap(), s.unwrap());
        let t = tokio::spawn(async move { for i in 0..200u32 { c.send(&Frame { ext: 0, ty: 1, payload: i.to_le_bytes().to_vec() }).await.unwrap(); } c });
        for i in 0..200u32 {
            assert_eq!(s.recv().await.unwrap().payload, i.to_le_bytes());
        }
        t.await.unwrap();
    }

    #[tokio::test]
    async fn ciphertext_tampering_and_oversized_frames_are_detected() {
        let key = generate_keypair().unwrap();
        let (a, b) = duplex(1 << 20);
        let (ra, mut wa) = tokio::io::split(a);
        let _ = ra;
        let k2 = Keypair { private: key.private.clone(), public: key.public };
        let srv = tokio::spawn(async move { accept(b, &k2).await });
        // a man in the middle flipping a byte of the transport data: emulate by a raw initiator that corrupts the first transport message
        let mut hs = Builder::new(PATTERN.parse().unwrap()).prologue(PROLOGUE).build_initiator().unwrap();
        let mut buf = vec![0u8; MAX_NOISE];
        let n = hs.write_message(&[], &mut buf).unwrap();
        let mut a = ra.unsplit(wa);
        write_msg(&mut a, &buf[..n]).await.unwrap();
        let reply = read_msg(&mut a).await.unwrap();
        hs.read_message(&reply, &mut buf).unwrap();
        let mut st = hs.into_transport_mode().unwrap();
        let mut srv = srv.await.unwrap().unwrap();
        let n = st.write_message(&Frame { ext: 0, ty: 1, payload: vec![1, 2, 3] }.encode(MAX_FRAME).unwrap(), &mut buf).unwrap();
        buf[3] ^= 0x01;
        write_msg(&mut a, &buf[..n]).await.unwrap();
        assert!(srv.recv().await.is_err(), "a modified ciphertext must not decrypt");
    }

    #[tokio::test]
    async fn the_declared_frame_length_is_capped_before_buffering() {
        let (c, s, _) = pair(true).await;
        let (mut c, mut s) = (c.unwrap(), s.unwrap());
        // header announcing 1 MiB, sent as a valid encrypted record
        let mut out = vec![0u8; MAX_NOISE];
        let n = c.st.write_message(&[0, 0, 1, 0x00, 0x00, 0x10], &mut out).unwrap();
        write_msg(&mut c.io, &out[..n]).await.unwrap();
        assert!(s.recv().await.is_err());
        assert!(Frame { ext: 0, ty: 0, payload: vec![0; MAX_FRAME + 1] }.encode(MAX_FRAME).is_err());
    }
}
