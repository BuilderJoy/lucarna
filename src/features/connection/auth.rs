use des::cipher::{BlockCipherEncrypt, KeyInit};

/// VNC auth: DES-encrypt the challenge with the password, each key byte bit-reversed.
pub fn vnc_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (k, b) in key.iter_mut().zip(password.bytes()) {
        *k = b.reverse_bits();
    }
    let cipher = des::Des::new(&key.into());
    let mut out = *challenge;
    for block in out.chunks_exact_mut(8) {
        cipher.encrypt_block(block.try_into().unwrap());
    }
    out
}
