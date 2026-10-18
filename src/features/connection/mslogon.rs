//! UltraVNC MS-Logon (Windows user name and password). Notes: docs/ultravnc-auth.md.
//!
//! II agrees on a key with a 31-bit Diffie-Hellman, III with X25519 (hashed with SHA-256 and cut to 8 bytes).
//! Either way the credentials then travel as a 256-byte user and 64-byte password, DES-CBC encrypted.

use std::io::{self, Read, Write};

use des::cipher::{BlockCipherEncrypt, KeyInit};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use super::auth::Credentials;
use crate::shared::wire::{invalid, read_bytes, to_latin1};

const DH_LIMIT: u64 = 1 << 31;
const USER_LEN: usize = 256;
const PASSWORD_LEN: usize = 64;

/// MS-Logon II: the server sends generator, modulus and its public value; we answer with ours.
pub fn logon_ii(s: &mut (impl Read + Write), creds: &Credentials) -> io::Result<()> {
    let mut read_u64 = || Ok::<_, io::Error>(u64::from_be_bytes(read_bytes(s, 8)?.try_into().unwrap()));
    let (generator, modulus, server_public) = (read_u64()?, read_u64()?, read_u64()?);
    // The official viewer applies the same limits; they also keep every product below 2^62.
    if !(2..=DH_LIMIT).contains(&modulus) || generator > modulus || server_public >= DH_LIMIT {
        return Err(invalid("invalid MS-Logon parameters from server"));
    }
    let private = u64::from(u32::from_le_bytes(random()?) & 0x7fff_ffff);
    s.write_all(&pow_mod(generator, private, modulus).to_be_bytes())?;
    send_credentials(s, creds, pow_mod(server_public, private, modulus).to_be_bytes())
}

/// MS-Logon III: X25519 public keys are exchanged (server first), then credentials follow.
pub fn logon_iii(s: &mut (impl Read + Write), creds: &Credentials) -> io::Result<()> {
    let server_public: [u8; 32] = read_bytes(s, 32)?.try_into().unwrap();
    let secret = StaticSecret::from(random::<32>()?);
    s.write_all(PublicKey::from(&secret).as_bytes())?;
    let shared = secret.diffie_hellman(&PublicKey::from(server_public));
    if !shared.was_contributory() {
        return Err(invalid("invalid MS-Logon III key from server"));
    }
    let key = Sha256::digest(shared.as_bytes())[..8].try_into().unwrap();
    send_credentials(s, creds, key)
}

fn send_credentials(s: &mut impl Write, creds: &Credentials, key: [u8; 8]) -> io::Result<()> {
    // The official viewer allows 254 and 63 characters; the rest of each buffer stays NUL.
    let mut user = latin1(&creds.user, 254, USER_LEN, "user name")?;
    let mut password = latin1(&creds.password, 63, PASSWORD_LEN, "password")?;
    encrypt(&mut user, key);
    encrypt(&mut password, key);
    s.write_all(&[user, password].concat())
}

/// Windows-ANSI text as Latin-1, NUL-padded to `size`; refuses text longer than `limit` instead of cutting it.
fn latin1(text: &str, limit: usize, size: usize, what: &str) -> io::Result<Vec<u8>> {
    let mut bytes = to_latin1(text);
    if bytes.len() > limit {
        return Err(invalid(format!("{what} too long for MS-Logon (max {limit})")));
    }
    bytes.resize(size, 0);
    Ok(bytes)
}

/// UltraVNC's `vncEncryptBytes2`: DES-CBC with the key bytes also as IV, in VNC's bit-reversed key order.
fn encrypt(data: &mut [u8], key: [u8; 8]) {
    let cipher = des::Des::new(&key.map(u8::reverse_bits).into());
    let mut previous = key;
    for block in data.chunks_exact_mut(8) {
        block.iter_mut().zip(previous).for_each(|(b, p)| *b ^= p);
        cipher.encrypt_block((&mut *block).try_into().unwrap());
        previous = (&*block).try_into().unwrap();
    }
}

fn pow_mod(mut base: u64, mut exponent: u64, modulus: u64) -> u64 {
    let mut result = 1 % modulus;
    base %= modulus;
    while exponent > 0 {
        if exponent & 1 == 1 {
            result = result * base % modulus;
        }
        base = base * base % modulus;
        exponent >>= 1;
    }
    result
}

fn random<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::wire::Duplex;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn creds(user: &str, password: &str) -> Credentials {
        Credentials { user: user.into(), password: password.into() }
    }

    /// Vectors produced by compiling UltraVNC's own d3des.c and `vncEncryptBytes2`.
    #[test]
    fn encrypt_matches_ultravnc() {
        let plain: Vec<u8> = (0..32u8).map(|i| i * 7 + 3).collect();
        let mut data = plain.clone();
        encrypt(&mut data, hex("0123456789abcdef").try_into().unwrap());
        assert_eq!(data, hex("b8c85d577cef00e6bbf4adbce71c43c785ce6916e0c9835613bca78b222088ef"));
        let mut data = plain[..16].to_vec();
        encrypt(&mut data, hex("ff00807f1020fe01").try_into().unwrap());
        assert_eq!(data, hex("0e64f2805932e6f97c94f9e6f7f392f2"));
    }

    fn expected(user: &str, password: &str, key: [u8; 8]) -> Vec<u8> {
        let (mut u, mut p) = (latin1(user, 254, USER_LEN, "").unwrap(), latin1(password, 63, PASSWORD_LEN, "").unwrap());
        encrypt(&mut u, key);
        encrypt(&mut p, key);
        [u, p].concat()
    }

    #[test]
    fn ii_agrees_on_the_key_with_the_server() {
        let (generator, modulus, server_private) = (7u64, 2_147_483_629u64, 123_456_789u64);
        let mut input = Vec::new();
        for v in [generator, modulus, pow_mod(generator, server_private, modulus)] {
            input.extend(v.to_be_bytes());
        }
        let mut conn = Duplex::new(input);
        logon_ii(&mut conn, &creds("ANA\\admin", "pässword")).unwrap();
        assert_eq!(conn.output.len(), 8 + 256 + 64);
        let client_public = u64::from_be_bytes(conn.output[..8].try_into().unwrap());
        let key = pow_mod(client_public, server_private, modulus).to_be_bytes();
        assert_eq!(conn.output[8..], expected("ANA\\admin", "pässword", key));
    }

    #[test]
    fn iii_agrees_on_the_key_with_the_server() {
        let server = StaticSecret::from([9u8; 32]);
        let mut conn = Duplex::new(PublicKey::from(&server).as_bytes().to_vec());
        logon_iii(&mut conn, &creds("admin", "secret")).unwrap();
        assert_eq!(conn.output.len(), 32 + 256 + 64);
        let client_public = PublicKey::from(<[u8; 32]>::try_from(&conn.output[..32]).unwrap());
        let key = Sha256::digest(server.diffie_hellman(&client_public).as_bytes())[..8].try_into().unwrap();
        assert_eq!(conn.output[32..], expected("admin", "secret", key));
        // An all-zero server key gives a predictable secret and is refused.
        assert!(logon_iii(&mut Duplex::new(vec![0; 32]), &creds("a", "b")).is_err());
    }

    #[test]
    fn rejects_hostile_dh_parameters() {
        let params = |g: u64, m: u64, p: u64| [g, m, p].iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<u8>>();
        for bad in [params(2, 0, 5), params(2, 1, 0), params(10, 5, 1), params(2, 1 << 32, 1), params(2, 97, 1 << 31)] {
            assert!(logon_ii(&mut Duplex::new(bad), &creds("a", "b")).is_err());
        }
    }

    #[test]
    fn refuses_credentials_that_would_be_truncated() {
        let mut conn = Duplex::new(PublicKey::from(&StaticSecret::from([1u8; 32])).as_bytes().to_vec());
        assert!(logon_iii(&mut conn, &creds("a", &"x".repeat(64))).is_err());
        assert!(latin1(&"u".repeat(254), 254, USER_LEN, "user").is_ok());
        assert!(latin1(&"u".repeat(255), 254, USER_LEN, "user").is_err());
    }
}
