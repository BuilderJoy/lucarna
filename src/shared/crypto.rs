//! Cryptographic primitives for End-to-End Encryption (E2EE) over untrusted repeaters:
//! - HMAC-SHA256 and PBKDF2-HMAC-SHA256 (RFC 8018)
//! - ChaCha20 Stream Cipher (RFC 8439 §2.4)
//! - Poly1305 Authenticator (RFC 8439 §2.5)
//! - ChaCha20-Poly1305 AEAD (RFC 8439 §2.8)
//! - Ephemeral X25519 Key Exchange + PIN Authentication
//! - Framed AEAD Stream with Replay Protection & 3-Attempt Brute-Force Lockout

use std::io::{self, Read, Write};

use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::shared::wire::invalid;

pub const MAX_PIN_FAILURES: u32 = 3;
/// Digits in the E2EE PIN.
pub const PIN_DIGITS: usize = 6;
pub const E2EE_MAGIC: &[u8; 8] = b"LUCE2E02";

/// Constant-time slice comparison.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Computes HMAC-SHA256(key, message).
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        let hash = Sha256::digest(key);
        key_block[..32].copy_from_slice(&hash);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0u8; 64];
    let mut opad = [0u8; 64];
    for i in 0..64 {
        ipad[i] = key_block[i] ^ 0x36;
        opad[i] = key_block[i] ^ 0x5c;
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// Derives key material from a password/PIN and salt using PBKDF2-HMAC-SHA256 (RFC 8018).
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    assert!(iterations > 0);
    for (block_idx, chunk) in (1u32..).zip(out.chunks_mut(32)) {
        let mut msg = Vec::with_capacity(salt.len() + 4);
        msg.extend_from_slice(salt);
        msg.extend_from_slice(&block_idx.to_be_bytes());

        let mut u = hmac_sha256(password, &msg);
        let mut f = u;

        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for i in 0..32 {
                f[i] ^= u[i];
            }
        }

        chunk.copy_from_slice(&f[..chunk.len()]);
    }
}

// ---------------------------------------------------------------------------
// RFC 8439: ChaCha20 Stream Cipher
// ---------------------------------------------------------------------------

#[inline]
fn quarter_round(w: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    w[a] = w[a].wrapping_add(w[b]);
    w[d] = (w[d] ^ w[a]).rotate_left(16);
    w[c] = w[c].wrapping_add(w[d]);
    w[b] = (w[b] ^ w[c]).rotate_left(12);
    w[a] = w[a].wrapping_add(w[b]);
    w[d] = (w[d] ^ w[a]).rotate_left(8);
    w[c] = w[c].wrapping_add(w[d]);
    w[b] = (w[b] ^ w[c]).rotate_left(7);
}

fn chacha20_block(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> [u8; 64] {
    let state = [
        0x61707865,
        0x3320646e,
        0x79622d32,
        0x6b206574,
        u32::from_le_bytes(key[0..4].try_into().unwrap()),
        u32::from_le_bytes(key[4..8].try_into().unwrap()),
        u32::from_le_bytes(key[8..12].try_into().unwrap()),
        u32::from_le_bytes(key[12..16].try_into().unwrap()),
        u32::from_le_bytes(key[16..20].try_into().unwrap()),
        u32::from_le_bytes(key[20..24].try_into().unwrap()),
        u32::from_le_bytes(key[24..28].try_into().unwrap()),
        u32::from_le_bytes(key[28..32].try_into().unwrap()),
        counter,
        u32::from_le_bytes(nonce[0..4].try_into().unwrap()),
        u32::from_le_bytes(nonce[4..8].try_into().unwrap()),
        u32::from_le_bytes(nonce[8..12].try_into().unwrap()),
    ];

    let mut working = state;
    for _ in 0..10 {
        quarter_round(&mut working, 0, 4, 8, 12);
        quarter_round(&mut working, 1, 5, 9, 13);
        quarter_round(&mut working, 2, 6, 10, 14);
        quarter_round(&mut working, 3, 7, 11, 15);
        quarter_round(&mut working, 0, 5, 10, 15);
        quarter_round(&mut working, 1, 6, 11, 12);
        quarter_round(&mut working, 2, 7, 8, 13);
        quarter_round(&mut working, 3, 4, 9, 14);
    }

    let mut out = [0u8; 64];
    for i in 0..16 {
        let sum = working[i].wrapping_add(state[i]);
        out[i * 4..(i + 1) * 4].copy_from_slice(&sum.to_le_bytes());
    }
    out
}

pub fn chacha20_xor(key: &[u8; 32], nonce: &[u8; 12], mut counter: u32, data: &mut [u8]) {
    for chunk in data.chunks_mut(64) {
        let block = chacha20_block(key, nonce, counter);
        for (d, b) in chunk.iter_mut().zip(block.iter()) {
            *d ^= *b;
        }
        counter = counter.wrapping_add(1);
    }
}

// ---------------------------------------------------------------------------
// RFC 8439: Poly1305 One-Time Authenticator
// ---------------------------------------------------------------------------

pub fn poly1305_mac(key: &[u8; 32], message: &[u8]) -> [u8; 16] {
    // Clamping r
    let mut r_bytes = [0u8; 16];
    r_bytes.copy_from_slice(&key[0..16]);
    r_bytes[3] &= 15;
    r_bytes[7] &= 15;
    r_bytes[11] &= 15;
    r_bytes[15] &= 15;
    r_bytes[4] &= 252;
    r_bytes[8] &= 252;
    r_bytes[12] &= 252;

    // Split r into 5 limbs of 26 bits
    let r0 = (u32::from_le_bytes(r_bytes[0..4].try_into().unwrap()) & 0x03ffffff) as u64;
    let r1 = ((u32::from_le_bytes(r_bytes[3..7].try_into().unwrap()) >> 2) & 0x03ffffff) as u64;
    let r2 = ((u32::from_le_bytes(r_bytes[6..10].try_into().unwrap()) >> 4) & 0x03ffffff) as u64;
    let r3 = ((u32::from_le_bytes(r_bytes[9..13].try_into().unwrap()) >> 6) & 0x03ffffff) as u64;
    let r4 = (u32::from_le_bytes(r_bytes[12..16].try_into().unwrap()) >> 8) as u64;

    let s1 = r1 * 5;
    let s2 = r2 * 5;
    let s3 = r3 * 5;
    let s4 = r4 * 5;

    let mut h0 = 0u64;
    let mut h1 = 0u64;
    let mut h2 = 0u64;
    let mut h3 = 0u64;
    let mut h4 = 0u64;

    for chunk in message.chunks(16) {
        let mut w = [0u8; 17];
        w[..chunk.len()].copy_from_slice(chunk);
        w[chunk.len()] = 0x01; // pad bit

        let w0 = (u32::from_le_bytes(w[0..4].try_into().unwrap()) & 0x03ffffff) as u64;
        let w1 = ((u32::from_le_bytes(w[3..7].try_into().unwrap()) >> 2) & 0x03ffffff) as u64;
        let w2 = ((u32::from_le_bytes(w[6..10].try_into().unwrap()) >> 4) & 0x03ffffff) as u64;
        let w3 = ((u32::from_le_bytes(w[9..13].try_into().unwrap()) >> 6) & 0x03ffffff) as u64;
        let w4 = ((u32::from_le_bytes(w[12..16].try_into().unwrap()) >> 8) | ((w[16] as u32) << 24))
            as u64;

        h0 += w0;
        h1 += w1;
        h2 += w2;
        h3 += w3;
        h4 += w4;

        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;

        let mut c = d0 >> 26;
        h0 = d0 & 0x03ffffff;
        let d1 = d1 + c;
        c = d1 >> 26;
        h1 = d1 & 0x03ffffff;
        let d2 = d2 + c;
        c = d2 >> 26;
        h2 = d2 & 0x03ffffff;
        let d3 = d3 + c;
        c = d3 >> 26;
        h3 = d3 & 0x03ffffff;
        let d4 = d4 + c;
        c = d4 >> 26;
        h4 = d4 & 0x03ffffff;
        h0 += c * 5;
        c = h0 >> 26;
        h0 &= 0x03ffffff;
        h1 += c;
    }

    // Full carry
    let mut c = h0 >> 26;
    h0 &= 0x03ffffff;
    h1 += c;
    c = h1 >> 26;
    h1 &= 0x03ffffff;
    h2 += c;
    c = h2 >> 26;
    h2 &= 0x03ffffff;
    h3 += c;
    c = h3 >> 26;
    h3 &= 0x03ffffff;
    h4 += c;
    c = h4 >> 26;
    h4 &= 0x03ffffff;
    h0 += c * 5;
    c = h0 >> 26;
    h0 &= 0x03ffffff;
    h1 += c;

    // Compute h + 5 to see if h >= 2^130 - 5
    let mut g0 = h0 + 5;
    c = g0 >> 26;
    g0 &= 0x03ffffff;
    let mut g1 = h1 + c;
    c = g1 >> 26;
    g1 &= 0x03ffffff;
    let mut g2 = h2 + c;
    c = g2 >> 26;
    g2 &= 0x03ffffff;
    let mut g3 = h3 + c;
    c = g3 >> 26;
    g3 &= 0x03ffffff;
    let mut g4 = h4 + c;

    let mask = (g4 >> 26).wrapping_neg();
    g4 &= 0x03ffffff;
    let not_mask = !mask;
    h0 = (h0 & not_mask) | (g0 & mask);
    h1 = (h1 & not_mask) | (g1 & mask);
    h2 = (h2 & not_mask) | (g2 & mask);
    h3 = (h3 & not_mask) | (g3 & mask);
    h4 = (h4 & not_mask) | (g4 & mask);

    // Reassemble into two 64-bit halves
    let lo = h0 | (h1 << 26) | ((h2 & 0xfff) << 52);
    let hi = (h2 >> 12) | (h3 << 14) | ((h4 & 0xffffff) << 40);

    let s0 = u64::from_le_bytes(key[16..24].try_into().unwrap());
    let s1 = u64::from_le_bytes(key[24..32].try_into().unwrap());

    let (out0, carry0) = lo.overflowing_add(s0);
    let out1 = hi.wrapping_add(s1).wrapping_add(carry0 as u64);

    let mut tag = [0u8; 16];
    tag[0..8].copy_from_slice(&out0.to_le_bytes());
    tag[8..16].copy_from_slice(&out1.to_le_bytes());
    tag
}

// ---------------------------------------------------------------------------
// RFC 8439: ChaCha20-Poly1305 AEAD
// ---------------------------------------------------------------------------

pub fn aead_seal(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let poly_key_block = chacha20_block(key, nonce, 0);
    let mut poly_key = [0u8; 32];
    poly_key.copy_from_slice(&poly_key_block[0..32]);

    let mut ciphertext = plaintext.to_vec();
    chacha20_xor(key, nonce, 1, &mut ciphertext);

    let mut mac_data = Vec::with_capacity(aad.len() + ciphertext.len() + 64);
    mac_data.extend_from_slice(aad);
    if !aad.len().is_multiple_of(16) {
        mac_data.resize(mac_data.len() + (16 - aad.len() % 16), 0);
    }
    mac_data.extend_from_slice(&ciphertext);
    if !ciphertext.len().is_multiple_of(16) {
        mac_data.resize(mac_data.len() + (16 - ciphertext.len() % 16), 0);
    }
    mac_data.extend_from_slice(&(aad.len() as u64).to_le_bytes());
    mac_data.extend_from_slice(&(ciphertext.len() as u64).to_le_bytes());

    let tag = poly1305_mac(&poly_key, &mac_data);
    ciphertext.extend_from_slice(&tag);
    ciphertext
}

pub fn aead_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext_and_tag: &[u8],
    aad: &[u8],
) -> io::Result<Vec<u8>> {
    if ciphertext_and_tag.len() < 16 {
        return Err(invalid("AEAD ciphertext shorter than 16-byte tag"));
    }
    let ct_len = ciphertext_and_tag.len() - 16;
    let (ciphertext, expected_tag) = ciphertext_and_tag.split_at(ct_len);

    let poly_key_block = chacha20_block(key, nonce, 0);
    let mut poly_key = [0u8; 32];
    poly_key.copy_from_slice(&poly_key_block[0..32]);

    let mut mac_data = Vec::with_capacity(aad.len() + ciphertext.len() + 64);
    mac_data.extend_from_slice(aad);
    if !aad.len().is_multiple_of(16) {
        mac_data.resize(mac_data.len() + (16 - aad.len() % 16), 0);
    }
    mac_data.extend_from_slice(ciphertext);
    if !ciphertext.len().is_multiple_of(16) {
        mac_data.resize(mac_data.len() + (16 - ciphertext.len() % 16), 0);
    }
    mac_data.extend_from_slice(&(aad.len() as u64).to_le_bytes());
    mac_data.extend_from_slice(&(ciphertext.len() as u64).to_le_bytes());

    let computed_tag = poly1305_mac(&poly_key, &mac_data);
    if !constant_time_eq(&computed_tag, expected_tag) {
        return Err(invalid(
            "AEAD authentication tag mismatch (tampering detected)",
        ));
    }

    let mut plaintext = ciphertext.to_vec();
    chacha20_xor(key, nonce, 1, &mut plaintext);
    Ok(plaintext)
}

// ---------------------------------------------------------------------------
// Human Session Code & PIN Primitives
// ---------------------------------------------------------------------------

/// Normalizes human session codes (e.g. `"582-914"` or `"582 914"` -> `"582914"`).
pub fn normalize_code(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_digit() || c.is_ascii_alphabetic())
        .collect::<String>()
        .to_uppercase()
}

/// Formats a 6- or 9-digit code with hyphens for high readability (e.g. `"582-914"`).
pub fn format_code(raw: &str) -> String {
    let clean = normalize_code(raw);
    match clean.len() {
        6 => format!("{}-{}", &clean[0..3], &clean[3..6]),
        9 => format!("{}-{}-{}", &clean[0..3], &clean[3..6], &clean[6..9]),
        _ => clean,
    }
}

/// Generates a random 6-digit numeric session code.
pub fn generate_session_code() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    let val = (u32::from_le_bytes(b) % 900_000) + 100_000;
    val.to_string()
}

/// Generates a random 6-digit numeric PIN (never starts with 0).
pub fn generate_pin() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    let val = (u32::from_le_bytes(b) % 900_000) + 100_000;
    val.to_string()
}

/// Random password for the classic (unencrypted) mode, so no host ever runs with a well-known one.
pub fn generate_password() -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut b = [0u8; 10];
    let _ = getrandom::fill(&mut b);
    b.iter()
        .map(|x| ALPHABET[*x as usize % ALPHABET.len()] as char)
        .collect()
}

// ---------------------------------------------------------------------------
// E2EE Directional Channels and Brute-Force Protected Handshake
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct E2eeKeys {
    pub send_key: [u8; 32],
    pub recv_key: [u8; 32],
    pub send_nonce_base: [u8; 12],
    pub recv_nonce_base: [u8; 12],
}

/// Computes directional nonces from a base 12-byte array and a 64-bit sequence counter.
pub fn make_nonce(base: &[u8; 12], seq: u64) -> [u8; 12] {
    let mut n = *base;
    let seq_bytes = seq.to_le_bytes();
    for i in 0..8 {
        n[4 + i] ^= seq_bytes[i];
    }
    n
}

/// Manages failed PIN attempts and locks down after `MAX_PIN_FAILURES` (3 attempts).
#[derive(Debug, Default, Clone)]
pub struct BruteForceLimiter {
    pub failures: u32,
}

impl BruteForceLimiter {
    pub fn new() -> Self {
        Self { failures: 0 }
    }

    pub fn record_failure(&mut self) -> bool {
        self.failures += 1;
        self.is_locked()
    }

    pub fn is_locked(&self) -> bool {
        self.failures >= MAX_PIN_FAILURES
    }
}

pub const MAX_E2EE_FRAME_SIZE: usize = 65_536; // 64 KiB frame limit on wire
pub const MAX_PLAINTEXT_CHUNK: usize = 16_384; // 16 KiB chunks for responsive latency

/// Transparent framed E2EE stream wrapping any bidirectional `Read + Write` stream.
pub struct E2eeStream<S: Read + Write> {
    stream: S,
    send_key: [u8; 32],
    send_nonce_base: [u8; 12],
    send_seq: u64,
    recv_key: [u8; 32],
    recv_nonce_base: [u8; 12],
    recv_seq: u64,
    recv_buf: Vec<u8>,
    recv_pos: usize,
}

impl<S: Read + Write> E2eeStream<S> {
    pub fn new(stream: S, keys: E2eeKeys) -> Self {
        Self {
            stream,
            send_key: keys.send_key,
            send_nonce_base: keys.send_nonce_base,
            send_seq: 0,
            recv_key: keys.recv_key,
            recv_nonce_base: keys.recv_nonce_base,
            recv_seq: 0,
            recv_buf: Vec::new(),
            recv_pos: 0,
        }
    }

    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    fn write_frame(&mut self, plaintext: &[u8]) -> io::Result<()> {
        let nonce = make_nonce(&self.send_nonce_base, self.send_seq);
        let aad = self.send_seq.to_be_bytes();
        self.send_seq = self
            .send_seq
            .checked_add(1)
            .ok_or_else(|| invalid("E2EE sequence counter overflow"))?;
        let sealed = aead_seal(&self.send_key, &nonce, plaintext, &aad);
        let len = (sealed.len() as u32).to_be_bytes();
        self.stream.write_all(&len)?;
        self.stream.write_all(&sealed)?;
        Ok(())
    }

    fn refill(&mut self) -> io::Result<bool> {
        let mut len_bytes = [0u8; 4];
        match self.stream.read_exact(&mut len_bytes) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(e) => return Err(e),
        }
        let frame_len = u32::from_be_bytes(len_bytes) as usize;
        if !(16..=MAX_E2EE_FRAME_SIZE).contains(&frame_len) {
            return Err(invalid(format!("invalid E2EE frame length: {frame_len}")));
        }
        let mut ciphertext = vec![0u8; frame_len];
        self.stream.read_exact(&mut ciphertext)?;
        let nonce = make_nonce(&self.recv_nonce_base, self.recv_seq);
        let aad = self.recv_seq.to_be_bytes();
        self.recv_seq = self
            .recv_seq
            .checked_add(1)
            .ok_or_else(|| invalid("E2EE sequence counter overflow"))?;
        let plaintext = aead_open(&self.recv_key, &nonce, &ciphertext, &aad)?;
        self.recv_buf = plaintext;
        self.recv_pos = 0;
        Ok(true)
    }
}

impl E2eeStream<std::net::TcpStream> {
    pub fn into_split(
        self,
    ) -> io::Result<(
        E2eeReader<std::net::TcpStream>,
        E2eeWriter<std::net::TcpStream>,
    )> {
        let stream_read = self.stream.try_clone()?;
        let stream_write = self.stream;
        let reader = E2eeReader {
            reader: stream_read,
            key: self.recv_key,
            nonce_base: self.recv_nonce_base,
            seq: self.recv_seq,
            buf: self.recv_buf,
            pos: self.recv_pos,
        };
        let writer = E2eeWriter {
            writer: stream_write,
            key: self.send_key,
            nonce_base: self.send_nonce_base,
            seq: self.send_seq,
        };
        Ok((reader, writer))
    }
}

impl<S: Read + Write> Read for E2eeStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.recv_pos >= self.recv_buf.len() && !self.refill()? {
            return Ok(0);
        }
        let available = self.recv_buf.len() - self.recv_pos;
        let to_copy = buf.len().min(available);
        buf[..to_copy].copy_from_slice(&self.recv_buf[self.recv_pos..self.recv_pos + to_copy]);
        self.recv_pos += to_copy;
        Ok(to_copy)
    }
}

impl<S: Read + Write> Write for E2eeStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let chunk_len = buf.len().min(MAX_PLAINTEXT_CHUNK);
        self.write_frame(&buf[..chunk_len])?;
        Ok(chunk_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

pub struct E2eeWriter<W: Write> {
    writer: W,
    key: [u8; 32],
    nonce_base: [u8; 12],
    seq: u64,
}

impl<W: Write> E2eeWriter<W> {
    pub fn get_ref(&self) -> &W {
        &self.writer
    }

    pub fn write_frame(&mut self, plaintext: &[u8]) -> io::Result<()> {
        let nonce = make_nonce(&self.nonce_base, self.seq);
        let aad = self.seq.to_be_bytes();
        self.seq = self
            .seq
            .checked_add(1)
            .ok_or_else(|| invalid("E2EE sequence counter overflow"))?;
        let sealed = aead_seal(&self.key, &nonce, plaintext, &aad);
        let len = (sealed.len() as u32).to_be_bytes();
        self.writer.write_all(&len)?;
        self.writer.write_all(&sealed)?;
        Ok(())
    }
}

impl<W: Write> Write for E2eeWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let chunk_len = buf.len().min(MAX_PLAINTEXT_CHUNK);
        self.write_frame(&buf[..chunk_len])?;
        Ok(chunk_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

pub struct E2eeReader<R: Read> {
    reader: R,
    key: [u8; 32],
    nonce_base: [u8; 12],
    seq: u64,
    buf: Vec<u8>,
    pos: usize,
}

impl<R: Read> E2eeReader<R> {
    fn refill(&mut self) -> io::Result<bool> {
        let mut len_bytes = [0u8; 4];
        match self.reader.read_exact(&mut len_bytes) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(e) => return Err(e),
        }
        let frame_len = u32::from_be_bytes(len_bytes) as usize;
        if !(16..=MAX_E2EE_FRAME_SIZE).contains(&frame_len) {
            return Err(invalid(format!("invalid E2EE frame length: {frame_len}")));
        }
        let mut ciphertext = vec![0u8; frame_len];
        self.reader.read_exact(&mut ciphertext)?;
        let nonce = make_nonce(&self.nonce_base, self.seq);
        let aad = self.seq.to_be_bytes();
        self.seq = self
            .seq
            .checked_add(1)
            .ok_or_else(|| invalid("E2EE sequence counter overflow"))?;
        let plaintext = aead_open(&self.key, &nonce, &ciphertext, &aad)?;
        self.buf = plaintext;
        self.pos = 0;
        Ok(true)
    }
}

impl<R: Read> Read for E2eeReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.buf.len() && !self.refill()? {
            return Ok(0);
        }
        let available = self.buf.len() - self.pos;
        let to_copy = buf.len().min(available);
        buf[..to_copy].copy_from_slice(&self.buf[self.pos..self.pos + to_copy]);
        self.pos += to_copy;
        Ok(to_copy)
    }
}

/// Bytes of one SPAKE2 message: a side byte and a compressed Ed25519 point.
const SPAKE_MSG_LEN: usize = 33;
/// A SPAKE2 message carries a side byte and an Ed25519 point. The crate only decompresses it, so we also refuse
/// points of small order (identity, 2-torsion...) and points outside the prime-order subgroup: a legitimate peer
/// never sends one, and one from an attacker could leak a few bits of the PIN hash per try.
fn peer_point_ok(msg: &[u8]) -> bool {
    use curve25519_dalek::edwards::CompressedEdwardsY;
    let Some(bytes) = msg.get(1..).and_then(|b| <[u8; 32]>::try_from(b).ok()) else {
        return false;
    };
    CompressedEdwardsY(bytes)
        .decompress()
        .is_some_and(|p| !p.is_small_order() && p.is_torsion_free())
}

const HOST_ID: &[u8] = b"lucarna-host";
const VIEWER_ID: &[u8] = b"lucarna-viewer";

/// Turns the SPAKE2 shared key into the proofs and the directional session keys.
fn keys_from_shared(shared: &[u8]) -> ([u8; 32], E2eeKeys, E2eeKeys) {
    let mut master = [0u8; 32];
    pbkdf2_sha256(shared, b"lucarna-e2ee-master-v2", 1, &mut master);
    let host_to_viewer = hmac_sha256(&master, b"host-to-viewer-key");
    let viewer_to_host = hmac_sha256(&master, b"viewer-to-host-key");
    let host_nonce = hmac_sha256(&master, b"host-nonce-base");
    let viewer_nonce = hmac_sha256(&master, b"viewer-nonce-base");
    let host_keys = E2eeKeys {
        send_key: host_to_viewer,
        recv_key: viewer_to_host,
        send_nonce_base: host_nonce[0..12].try_into().unwrap(),
        recv_nonce_base: viewer_nonce[0..12].try_into().unwrap(),
    };
    let viewer_keys = E2eeKeys {
        send_key: viewer_to_host,
        recv_key: host_to_viewer,
        send_nonce_base: viewer_nonce[0..12].try_into().unwrap(),
        recv_nonce_base: host_nonce[0..12].try_into().unwrap(),
    };
    (master, host_keys, viewer_keys)
}

/// Host side of the E2EE handshake, a SPAKE2 password-authenticated key exchange with the PIN as the password.
/// Whoever records the exchange learns nothing about the PIN: every guess has to be made against a live peer,
/// which the limiter counts. The wire format is:
/// 1. Host sends the magic `LUCE2E02` and its SPAKE2 message (33 bytes).
/// 2. Viewer sends its SPAKE2 message (33 bytes) and a proof (32 bytes) made with the shared key.
/// 3. Host checks the proof (constant time) and answers with its own proof (32 bytes).
pub fn host_e2ee_handshake(
    stream: &mut (impl Read + Write),
    pin: &str,
    limiter: &mut BruteForceLimiter,
) -> io::Result<E2eeKeys> {
    if limiter.is_locked() {
        return Err(invalid("session locked: too many failed PIN attempts"));
    }
    let (state, host_msg) = Spake2::<Ed25519Group>::start_a(
        &Password::new(pin.as_bytes()),
        &Identity::new(HOST_ID),
        &Identity::new(VIEWER_ID),
    );
    let mut out = Vec::with_capacity(8 + SPAKE_MSG_LEN);
    out.extend_from_slice(E2EE_MAGIC);
    out.extend_from_slice(&host_msg);
    stream.write_all(&out)?;
    stream.flush()?;

    let mut in_buf = [0u8; SPAKE_MSG_LEN + 32];
    stream.read_exact(&mut in_buf)?;
    if !peer_point_ok(&in_buf[..SPAKE_MSG_LEN]) {
        return Err(invalid("invalid key exchange message"));
    }
    let shared = state
        .finish(&in_buf[..SPAKE_MSG_LEN])
        .map_err(|_| invalid("invalid key exchange message"))?;
    let (master, host_keys, _) = keys_from_shared(&shared);

    let expected = hmac_sha256(&master, b"viewer-pin-proof");
    if !constant_time_eq(&in_buf[SPAKE_MSG_LEN..], &expected) {
        if limiter.record_failure() {
            let _ = stream.write_all(&[0xff; 32]);
            return Err(invalid(
                "authentication failed: 3 incorrect attempts reached, session revoked",
            ));
        }
        let _ = stream.write_all(&[0xee; 32]);
        return Err(invalid("authentication failed: invalid PIN"));
    }
    stream.write_all(&hmac_sha256(&master, b"host-pin-proof"))?;
    stream.flush()?;
    Ok(host_keys)
}

/// Viewer side of the handshake; see [`host_e2ee_handshake`].
pub fn viewer_e2ee_handshake(stream: &mut (impl Read + Write), pin: &str) -> io::Result<E2eeKeys> {
    let mut in_buf = [0u8; 8 + SPAKE_MSG_LEN];
    stream.read_exact(&mut in_buf)?;
    if &in_buf[0..8] != E2EE_MAGIC {
        return Err(invalid(
            "the other side runs another Lucarna version: update both sides",
        ));
    }
    let (state, viewer_msg) = Spake2::<Ed25519Group>::start_b(
        &Password::new(pin.as_bytes()),
        &Identity::new(HOST_ID),
        &Identity::new(VIEWER_ID),
    );
    if !peer_point_ok(&in_buf[8..]) {
        return Err(invalid("invalid key exchange message"));
    }
    let shared = state
        .finish(&in_buf[8..])
        .map_err(|_| invalid("invalid key exchange message"))?;
    let (master, _, viewer_keys) = keys_from_shared(&shared);

    let mut out = Vec::with_capacity(SPAKE_MSG_LEN + 32);
    out.extend_from_slice(&viewer_msg);
    out.extend_from_slice(&hmac_sha256(&master, b"viewer-pin-proof"));
    stream.write_all(&out)?;
    stream.flush()?;

    let mut host_proof = [0u8; 32];
    stream.read_exact(&mut host_proof)?;
    if !constant_time_eq(&host_proof, &hmac_sha256(&master, b"host-pin-proof")) {
        return Err(invalid("PIN rejected by host or session locked"));
    }
    Ok(viewer_keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc8439_chacha20_test_vector() {
        // Test vector from RFC 8439 §2.4.2
        let key = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ];
        let nonce = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4a, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.".to_vec();
        chacha20_xor(&key, &nonce, 1, &mut plaintext);

        let expected_prefix = [
            0x6e, 0x2e, 0x35, 0x9a, 0x25, 0x68, 0xf9, 0x80, 0x41, 0xba, 0x07, 0x28, 0xdd, 0x0d,
            0x69, 0x81,
        ];
        assert_eq!(&plaintext[0..16], &expected_prefix);
    }

    #[test]
    fn chacha20_poly1305_aead_roundtrip_and_tamper_detection() {
        let key = [0x42; 32];
        let nonce = [0x19; 12];
        let msg = b"Radio Studio Transmit Payload: cuna_apertura.wav";
        let aad = b"header-metadata";

        let sealed = aead_seal(&key, &nonce, msg, aad);
        assert_eq!(sealed.len(), msg.len() + 16);

        let opened = aead_open(&key, &nonce, &sealed, aad).unwrap();
        assert_eq!(opened, msg);

        // Tamper with ciphertext -> must be rejected
        let mut tampered = sealed.clone();
        tampered[5] ^= 0x01;
        assert!(aead_open(&key, &nonce, &tampered, aad).is_err());

        // Tamper with AAD -> must be rejected
        assert!(aead_open(&key, &nonce, &sealed, b"wrong-aad").is_err());
    }

    #[test]
    fn rfc8439_poly1305_test_vector() {
        // RFC 8439 §2.5.2 test vector
        let key = [
            0x85, 0xd6, 0xbe, 0x78, 0x57, 0x55, 0x6d, 0x33, 0x7f, 0x44, 0x52, 0xfe, 0x42, 0xd5,
            0x06, 0xa8, 0x01, 0x03, 0x80, 0x8a, 0xfb, 0x0d, 0xb2, 0xfd, 0x4a, 0xbf, 0xf6, 0xaf,
            0x41, 0x49, 0xf5, 0x1b,
        ];
        let msg = b"Cryptographic Forum Research Group";
        let tag = poly1305_mac(&key, msg);
        let expected = [
            0xa8, 0x06, 0x1d, 0xc1, 0x30, 0x51, 0x36, 0xc6, 0xc2, 0x2b, 0x8b, 0xaf, 0x0c, 0x01,
            0x27, 0xa9,
        ];
        assert_eq!(tag, expected);
    }

    #[test]
    fn e2ee_stream_bidirectional_roundtrip() {
        let (s1, s2) = crate::shared::wire::pipe_pair();
        let host_keys = E2eeKeys {
            send_key: [0x11; 32],
            recv_key: [0x22; 32],
            send_nonce_base: [0x33; 12],
            recv_nonce_base: [0x44; 12],
        };
        let viewer_keys = E2eeKeys {
            send_key: [0x22; 32],
            recv_key: [0x11; 32],
            send_nonce_base: [0x44; 12],
            recv_nonce_base: [0x33; 12],
        };

        let mut host_stream = E2eeStream::new(s1, host_keys);
        let mut viewer_stream = E2eeStream::new(s2, viewer_keys);

        let handle = std::thread::spawn(move || {
            let mut read_buf = [0u8; 19];
            viewer_stream.read_exact(&mut read_buf).unwrap();
            assert_eq!(&read_buf, b"Hello from Lucarna!");

            viewer_stream.write_all(b"Hello back to Host!").unwrap();
            viewer_stream.flush().unwrap();
        });

        host_stream.write_all(b"Hello from Lucarna!").unwrap();
        host_stream.flush().unwrap();

        let mut host_read_buf = [0u8; 19];
        host_stream.read_exact(&mut host_read_buf).unwrap();
        assert_eq!(&host_read_buf, b"Hello back to Host!");

        handle.join().unwrap();
    }

    #[test]
    fn pins_have_six_digits_and_passwords_are_random() {
        for _ in 0..50 {
            let pin = generate_pin();
            assert_eq!(pin.len(), PIN_DIGITS);
            assert!(pin.chars().all(|c| c.is_ascii_digit()) && !pin.starts_with('0'));
        }
        let (a, b) = (generate_password(), generate_password());
        assert_eq!(a.len(), 10);
        assert_ne!(a, b);
        assert_ne!(a, "studio");
    }

    fn run_handshake(
        host_pin: &'static str,
        viewer_pin: &'static str,
    ) -> (io::Result<E2eeKeys>, io::Result<E2eeKeys>) {
        let (mut s1, mut s2) = crate::shared::wire::pipe_pair();
        let host = std::thread::spawn(move || {
            let mut limiter = BruteForceLimiter::new();
            host_e2ee_handshake(&mut s1, host_pin, &mut limiter)
        });
        let viewer = viewer_e2ee_handshake(&mut s2, viewer_pin);
        (host.join().unwrap(), viewer)
    }

    #[test]
    fn points_of_small_order_are_refused_and_honest_ones_pass() {
        // Identity (y = 1) and the point of order 2 (y = -1), each behind the side byte.
        let mut identity = vec![0x41];
        identity.extend_from_slice(&{
            let mut b = [0u8; 32];
            b[0] = 1;
            b
        });
        let mut order2 = vec![0x41];
        order2.extend_from_slice(&{
            let mut b = [0xffu8; 32];
            b[0] = 0xec;
            b[31] = 0x7f;
            b
        });
        assert!(!peer_point_ok(&identity));
        assert!(!peer_point_ok(&order2));
        assert!(!peer_point_ok(&[0x41; 5]));
        // A real message from the crate passes (it is checked end to end by the round-trip tests too).
        let (_, msg) = Spake2::<Ed25519Group>::start_b(
            &Password::new(b"123456"),
            &Identity::new(HOST_ID),
            &Identity::new(VIEWER_ID),
        );
        assert!(peer_point_ok(&msg));
    }

    #[test]
    fn a_host_refuses_a_viewer_that_sends_a_small_order_point() {
        let (mut s1, mut s2) = crate::shared::wire::pipe_pair();
        let host = std::thread::spawn(move || {
            let mut l = BruteForceLimiter::new();
            let r = host_e2ee_handshake(&mut s1, "123456", &mut l);
            (r.is_err(), l.failures)
        });
        let mut hello = [0u8; 8 + SPAKE_MSG_LEN];
        s2.read_exact(&mut hello).unwrap();
        let mut reply = vec![0x42];
        reply.extend_from_slice(&{
            let mut b = [0u8; 32];
            b[0] = 1;
            b
        });
        reply.extend_from_slice(&[0u8; 32]);
        s2.write_all(&reply).unwrap();
        let (refused, failures) = host.join().unwrap();
        assert!(refused);
        // It is a malformed message, not a PIN guess: it does not use up an attempt.
        assert_eq!(failures, 0);
    }

    #[test]
    fn every_handshake_gets_new_keys_even_with_the_same_pin() {
        let (a, _) = run_handshake("123456", "123456");
        let (b, _) = run_handshake("123456", "123456");
        // Fresh secrets per visit: recording one visit and learning the PIN later opens nothing.
        assert_ne!(a.unwrap().send_key, b.unwrap().send_key);
    }

    #[test]
    fn a_wrong_pin_gives_neither_side_a_key() {
        let (host, viewer) = run_handshake("123456", "123457");
        assert!(host.is_err());
        assert!(viewer.is_err());
    }

    #[test]
    fn the_old_wire_format_is_refused_with_a_clear_message() {
        let (mut host_side, mut viewer_side) = crate::shared::wire::pipe_pair();
        host_side.write_all(b"LUCE2E01").unwrap();
        host_side.write_all(&[0u8; SPAKE_MSG_LEN]).unwrap();
        let err = viewer_e2ee_handshake(&mut viewer_side, "123456")
            .err()
            .unwrap();
        assert!(err.to_string().contains("update both sides"), "{err}");
    }

    #[test]
    fn a_guessing_peer_is_rejected_and_counted() {
        // An attacker who has the host's message but not the PIN gets one try per connection, and the host
        // counts it. (That no PIN can be checked offline against a recording is a property of SPAKE2 and is
        // covered by the crate's own tests; it cannot be shown by a black-box test here.)
        let (mut s1, mut s2) = crate::shared::wire::pipe_pair();
        let host = std::thread::spawn(move || {
            let mut l = BruteForceLimiter::new();
            let _ = host_e2ee_handshake(&mut s1, "000001", &mut l);
        });
        let mut hello = [0u8; 8 + SPAKE_MSG_LEN];
        s2.read_exact(&mut hello).unwrap();
        assert_eq!(&hello[..8], E2EE_MAGIC);
        // An attacker answers with a random point and a made-up proof: the host rejects it and counts it.
        let (_, msg) = Spake2::<Ed25519Group>::start_b(
            &Password::new(b"999999"),
            &Identity::new(HOST_ID),
            &Identity::new(VIEWER_ID),
        );
        let mut reply = msg.clone();
        reply.extend_from_slice(&[7u8; 32]);
        s2.write_all(&reply).unwrap();
        let mut answer = [0u8; 32];
        s2.read_exact(&mut answer).unwrap();
        assert_eq!(answer, [0xee; 32]);
        host.join().unwrap();
    }

    #[test]
    fn e2ee_handshake_succeeds_with_correct_pin() {
        let (mut s1, mut s2) = crate::shared::wire::pipe_pair();
        let pin = "4920";

        let host_handle = std::thread::spawn(move || {
            let mut limiter = BruteForceLimiter::new();
            host_e2ee_handshake(&mut s1, pin, &mut limiter).unwrap()
        });

        let viewer_keys = viewer_e2ee_handshake(&mut s2, pin).unwrap();
        let host_keys = host_handle.join().unwrap();

        assert_eq!(viewer_keys.send_key, host_keys.recv_key);
        assert_eq!(viewer_keys.recv_key, host_keys.send_key);
    }

    #[test]
    fn e2ee_handshake_rejects_wrong_pin_and_locks_after_3_failures() {
        let mut limiter = BruteForceLimiter::new();

        for i in 1..=3 {
            let (mut s1, mut s2) = crate::shared::wire::pipe_pair();
            let mut thread_limiter = limiter.clone();
            let host_handle = std::thread::spawn(move || {
                let res = host_e2ee_handshake(&mut s1, "4920", &mut thread_limiter);
                (res, thread_limiter)
            });

            // Viewer types incorrect PIN
            let viewer_res = viewer_e2ee_handshake(&mut s2, "0000");
            assert!(viewer_res.is_err());

            let (host_res, updated_limiter) = host_handle.join().unwrap();
            assert!(host_res.is_err());
            limiter = updated_limiter;

            if i < 3 {
                assert!(!limiter.is_locked());
            }
        }
        assert!(limiter.is_locked(), "must be locked after 3 failures");
    }
}
