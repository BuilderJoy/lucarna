//! Big-endian RFB wire primitives.

use std::io::{self, Read};

pub fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

pub fn read_bytes(s: &mut impl Read, n: usize) -> io::Result<Vec<u8>> {
    let mut b = vec![0; n];
    s.read_exact(&mut b)?;
    Ok(b)
}

pub fn read_u8(s: &mut impl Read) -> io::Result<u8> {
    Ok(read_bytes(s, 1)?[0])
}

pub fn read_u16(s: &mut impl Read) -> io::Result<u16> {
    Ok(u16::from_be_bytes(read_bytes(s, 2)?.try_into().unwrap()))
}

pub fn read_u32(s: &mut impl Read) -> io::Result<u32> {
    Ok(u32::from_be_bytes(read_bytes(s, 4)?.try_into().unwrap()))
}

/// u32 length-prefixed string, rejected above `max` bytes.
pub fn read_string(s: &mut impl Read, max: usize) -> io::Result<String> {
    let len = read_u32(s)? as usize;
    if len > max {
        return Err(invalid("string too long"));
    }
    Ok(String::from_utf8_lossy(&read_bytes(s, len)?).into_owned())
}
