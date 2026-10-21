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

pub fn read_u64(s: &mut impl Read) -> io::Result<u64> {
    Ok(u64::from_be_bytes(read_bytes(s, 8)?.try_into().unwrap()))
}

/// Windows-ANSI text off the wire, read as Latin-1.
pub fn from_latin1(b: &[u8]) -> String {
    b.iter().map(|&c| char::from(c)).collect()
}

/// Characters outside Latin-1 become '?'.
pub fn to_latin1(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
        .collect()
}

/// u32 length-prefixed string, rejected above `max` bytes.
pub fn read_string(s: &mut impl Read, max: usize) -> io::Result<String> {
    let len = read_u32(s)? as usize;
    if len > max {
        return Err(invalid("string too long"));
    }
    Ok(String::from_utf8_lossy(&read_bytes(s, len)?).into_owned())
}

/// A fake connection for tests: reads come from `input`, writes are collected in `output`.
#[cfg(test)]
pub struct Duplex {
    pub input: io::Cursor<Vec<u8>>,
    pub output: Vec<u8>,
}

#[cfg(test)]
impl Duplex {
    pub fn new(input: Vec<u8>) -> Self {
        Self {
            input: io::Cursor::new(input),
            output: Vec::new(),
        }
    }
}

#[cfg(test)]
impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

#[cfg(test)]
impl io::Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
