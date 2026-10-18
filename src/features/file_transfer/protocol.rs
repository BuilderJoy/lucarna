//! UltraVNC file transfer wire messages (RFB message type 7). Byte layouts: docs/ultravnc-ft.md.

use std::io::{self, Read};

use crate::shared::wire::{invalid, read_bytes, read_u8, read_u16, read_u32};

/// Pseudo-encoding asking the server for FT protocol v4: UTF-8 paths, Unicode listings, 32 KiB blocks.
pub const ENC_PROTOCOL_VERSION: i32 = 0xFFFF_8002_u32 as i32;

const DIR_CONTENT_REQUEST: u8 = 1;
const DIR_PACKET: u8 = 2;
const FILE_TRANSFER_REQUEST: u8 = 3;
const FILE_HEADER: u8 = 4;
const FILE_PACKET: u8 = 5;
const END_OF_FILE: u8 = 6;
const ABORT: u8 = 7;
const FILE_TRANSFER_OFFER: u8 = 8;
const FILE_ACCEPT_HEADER: u8 = 9;
const COMMAND: u8 = 10;
const COMMAND_RETURN: u8 = 11;
const ACCESS: u8 = 14;
const PROTOCOL_VERSION: u8 = 17;

const R_DIR_CONTENT: u16 = 1;
const R_DRIVES_LIST: u16 = 2;
pub const UNICODE: u16 = 0x8000;
pub const A_DIRECTORY: u16 = 1;
pub const A_FILE: u16 = 2;
const A_DRIVES_LIST: u16 = 3;
pub const A_DIR_CREATE: u16 = 4;
pub const A_FILE_DELETE: u16 = 7;
pub const A_FILE_RENAME: u16 = 8;
pub const A_DIR_INACCESSIBLE: u16 = 18;

const C_DIR_CREATE: u16 = 1;
const C_FILE_DELETE: u16 = 4;
const C_FILE_RENAME: u16 = 5;

/// Default upload block size (8 KiB); libvncserver uses a fixed 8192-byte decompression buffer.
pub const DEFAULT_BLOCK: usize = 8 << 10;
/// Largest inflated file block we accept (servers announce 32 KiB; the official viewer allows up to 1 MiB).
pub const MAX_BLOCK: usize = 1 << 20;
const MAX_DATA: usize = MAX_BLOCK + 1024;
const ERROR: u32 = 0xFFFF_FFFF;

fn message(content_type: u8, param: u16, size: u32, data: &[u8]) -> Vec<u8> {
    let mut m = vec![7, content_type];
    // UltraVNC copies contentParam without byte swapping, so on the wire it is little-endian (unlike size and length).
    m.extend_from_slice(&param.to_le_bytes());
    m.extend_from_slice(&size.to_be_bytes());
    m.extend_from_slice(&(data.len() as u32).to_be_bytes());
    m.extend_from_slice(data);
    m
}

/// The official viewer's permission probe: Abort with a non-zero param, answered by Access.
pub fn permission_request() -> Vec<u8> {
    message(ABORT, 3, 0, &[])
}

pub fn drives_request() -> Vec<u8> {
    message(DIR_CONTENT_REQUEST, R_DRIVES_LIST, 0, &[])
}

/// `path` is a Windows directory ending in `\`.
pub fn dir_request(path: &str) -> Vec<u8> {
    message(DIR_CONTENT_REQUEST, R_DIR_CONTENT | UNICODE, 0, path.as_bytes())
}

/// Asks for a file, with per-block zlib compression.
pub fn download_request(path: &str) -> Vec<u8> {
    message(FILE_TRANSFER_REQUEST, 0, 1, path.as_bytes())
}

/// Accepts a FileHeader by echoing its size, or rejects it with `None`.
pub fn header_reply(size: Option<u32>) -> Vec<u8> {
    message(FILE_HEADER, 0, size.unwrap_or(ERROR), &[])
}

/// Offers to upload a `size`-byte file to `remote_spec` (`"C:\\dir\\name,MM/DD/YYYY HH:MM"`).
pub fn upload_offer(remote_spec: &str, size: u64) -> Vec<u8> {
    let mut m = message(FILE_TRANSFER_OFFER, 0, size as u32, remote_spec.as_bytes());
    // Like FileHeader, the high 32 bits of the size follow the data outside `length`.
    m.extend_from_slice(&((size >> 32) as u32).to_be_bytes());
    m
}

pub fn file_packet(compressed: bool, data: &[u8]) -> Vec<u8> {
    message(FILE_PACKET, 0, u32::from(compressed), data)
}

pub fn end_of_file() -> Vec<u8> {
    message(END_OF_FILE, 0, 0, &[])
}

pub fn abort() -> Vec<u8> {
    message(ABORT, 0, 0, &[])
}

pub fn create_dir(path: &str) -> Vec<u8> {
    message(COMMAND, C_DIR_CREATE, 0, path.as_bytes())
}

pub fn delete(path: &str) -> Vec<u8> {
    message(COMMAND, C_FILE_DELETE, 0, path.as_bytes())
}

pub fn rename(old: &str, new: &str) -> Vec<u8> {
    message(COMMAND, C_FILE_RENAME, 0, format!("{old}*{new}").as_bytes())
}

#[derive(Debug, PartialEq)]
pub enum Msg {
    ProtocolVersion {
        version: u16,
        block_size: u32,
    },
    Access(bool),
    Drives(Vec<u8>),
    Dir {
        param: u16,
        data: Vec<u8>,
    },
    /// `size` is None when the server could not open the file.
    FileHeader {
        size: Option<u64>,
        low: u32,
    },
    FileAccept(bool),
    FilePacket {
        compressed: bool,
        data: Vec<u8>,
    },
    EndOfFile,
    Abort,
    CommandReturn {
        param: u16,
        ok: bool,
    },
    /// Checksums and anything else we don't act on.
    Other,
}

/// Reads one FT message body (after the type byte).
pub fn read(s: &mut impl Read) -> io::Result<Msg> {
    let content_type = read_u8(s)?;
    let param = read_u16(s)?.swap_bytes(); // little-endian, see `message`
    let size = read_u32(s)?;
    let length = read_u32(s)? as usize;
    if length > MAX_DATA {
        return Err(invalid("file transfer message too large"));
    }
    let data = read_bytes(s, length)?;
    Ok(match content_type {
        PROTOCOL_VERSION => Msg::ProtocolVersion {
            version: param,
            block_size: size,
        },
        ACCESS => Msg::Access(size == 1),
        DIR_PACKET if param & !UNICODE == A_DRIVES_LIST => Msg::Drives(data),
        DIR_PACKET => Msg::Dir { param, data },
        FILE_HEADER => {
            // The high half of the size travels after the name, outside `length`.
            let high = read_u32(s)?;
            let size = (size != ERROR || high != ERROR).then_some(u64::from(high) << 32 | u64::from(size));
            Msg::FileHeader {
                size,
                low: size.map_or(ERROR, |s| s as u32),
            }
        }
        FILE_ACCEPT_HEADER => Msg::FileAccept(size != ERROR),
        FILE_PACKET => match size {
            0 | 1 => Msg::FilePacket {
                compressed: size == 1,
                data,
            },
            _ => return Err(invalid("unexpected delta file packet")),
        },
        END_OF_FILE => Msg::EndOfFile,
        ABORT => Msg::Abort,
        COMMAND_RETURN => Msg::CommandReturn {
            param: param & !UNICODE,
            ok: size != ERROR,
        },
        _ => Msg::Other,
    })
}

/// One `WIN32_FIND_DATA(W)` listing entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// FILETIME: 100 ns ticks since 1601-01-01 UTC.
    pub modified: u64,
    pub inaccessible: bool,
}

pub fn parse_entry(data: &[u8], unicode: bool) -> io::Result<Entry> {
    if data.len() < 44 {
        return Err(invalid("short directory entry"));
    }
    let u32le = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let name = &data[44..];
    let name = if unicode {
        let units: Vec<u16> = name.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
        String::from_utf16_lossy(&units)
    } else {
        ansi(name)
    };
    Ok(Entry {
        name,
        is_dir: u32le(0) & 0x10 != 0,
        size: u64::from(u32le(28)) << 32 | u64::from(u32le(32)),
        modified: u64::from(u32le(24)) << 32 | u64::from(u32le(20)),
        inaccessible: u32le(36) == 0xFFFF_FFFE,
    })
}

/// Server-codepage text, read as Latin-1 up to the first NUL.
pub fn ansi(b: &[u8]) -> String {
    b.iter().take_while(|&&c| c != 0).map(|&c| char::from(c)).collect()
}

/// `"C:l\0D:c\0"` → `[("C:\", 'l'), ("D:\", 'c')]`; the letter after the colon is the drive type.
pub fn parse_drives(data: &[u8]) -> Vec<(String, char)> {
    data.chunks(4)
        .filter(|c| c.len() >= 3 && c[0].is_ascii_alphabetic() && c[1] == b':')
        .map(|c| (format!("{}:\\", c[0] as char), c[2] as char))
        .collect()
}

/// Civil `(year, month, day, hour, minute)` in UTC from Unix seconds (Howard Hinnant's algorithm).
pub fn utc_parts(secs: i64) -> (i64, i64, i64, i64, i64) {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, rem / 3600, rem % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(content_type: u8, param: u16, size: u32, data: &[u8], extra: &[u8]) -> Vec<u8> {
        let mut m = message(content_type, param, size, data)[1..].to_vec();
        m.extend_from_slice(extra);
        m
    }

    #[test]
    fn requests() {
        assert_eq!(permission_request(), [7, 7, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(dir_request("C:\\"), [7, 1, 1, 0x80, 0, 0, 0, 0, 0, 0, 0, 3, b'C', b':', b'\\']);
        assert_eq!(download_request("C:\\a")[4..8], [0, 0, 0, 1]);
        assert_eq!(
            upload_offer("C:\\a", (2 << 32) | 9),
            [7, 8, 0, 0, 0, 0, 0, 9, 0, 0, 0, 4, b'C', b':', b'\\', b'a', 0, 0, 0, 2]
        );
        assert_eq!(file_packet(true, b"xy"), [7, 5, 0, 0, 0, 0, 0, 1, 0, 0, 0, 2, b'x', b'y']);
        assert_eq!(end_of_file(), [7, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(create_dir("C:\\d"), [7, 10, 1, 0, 0, 0, 0, 0, 0, 0, 0, 4, b'C', b':', b'\\', b'd']);
        assert_eq!(delete("C:\\f"), [7, 10, 4, 0, 0, 0, 0, 0, 0, 0, 0, 4, b'C', b':', b'\\', b'f']);
        assert_eq!(
            rename("C:\\a", "C:\\b"),
            [
                7, 10, 5, 0, 0, 0, 0, 0, 0, 0, 0, 9, b'C', b':', b'\\', b'a', b'*', b'C', b':', b'\\', b'b'
            ]
        );
    }

    #[test]
    fn reads_messages() {
        let read = |m: Vec<u8>| read(&mut &m[..]).unwrap();
        assert_eq!(
            read(server(PROTOCOL_VERSION, 4, 32768, &[], &[])),
            Msg::ProtocolVersion {
                version: 4,
                block_size: 32768
            }
        );
        assert_eq!(read(server(ACCESS, 0, 1, &[], &[])), Msg::Access(true));
        assert_eq!(read(server(ACCESS, 0, ERROR, &[], &[])), Msg::Access(false));
        assert_eq!(read(server(DIR_PACKET, 3, 0, b"C:l\0", &[])), Msg::Drives(b"C:l\0".to_vec()));
        assert_eq!(
            read(server(FILE_HEADER, 0, 5, b"C:\\a,x", &[0, 0, 0, 1])),
            Msg::FileHeader {
                size: Some((1 << 32) + 5),
                low: 5
            }
        );
        assert_eq!(
            read(server(FILE_HEADER, 0, ERROR, b"C:\\a", &[0xFF; 4])),
            Msg::FileHeader { size: None, low: ERROR }
        );
        assert_eq!(read(server(FILE_ACCEPT_HEADER, 0, 0, b"C:\\!UVNCPFT-a", &[])), Msg::FileAccept(true));
        assert_eq!(read(server(FILE_ACCEPT_HEADER, 0, ERROR, b"C:\\a", &[])), Msg::FileAccept(false));
        assert_eq!(
            read(server(COMMAND_RETURN, A_DIR_CREATE, 0, b"C:\\d", &[])),
            Msg::CommandReturn {
                param: A_DIR_CREATE,
                ok: true
            }
        );
        assert_eq!(
            read(server(COMMAND_RETURN, A_FILE_DELETE, ERROR, b"C:\\f", &[])),
            Msg::CommandReturn {
                param: A_FILE_DELETE,
                ok: false
            }
        );
    }

    #[test]
    fn rejects_oversized_message() {
        let mut m = vec![FILE_PACKET, 0, 0, 0, 0, 0, 0];
        m.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(read(&mut &m[..]).is_err());
    }

    #[test]
    fn entries() {
        let mut e = vec![0; 44];
        e[0] = 0x10;
        e[32] = 7;
        e[36..40].copy_from_slice(&0xFFFF_FFFE_u32.to_le_bytes());
        let mut w = e.clone();
        w.extend("ñá".encode_utf16().flat_map(u16::to_le_bytes).chain([0, 0]));
        let w = parse_entry(&w, true).unwrap();
        assert_eq!((w.name.as_str(), w.is_dir, w.size, w.inaccessible), ("ñá", true, 7, true));
        e[0] = 0x20;
        e.extend([b'r', 0xE9]);
        assert_eq!(parse_entry(&e, false).unwrap().name, "ré");
        assert!(parse_entry(&[0; 10], true).is_err());
    }

    #[test]
    fn drives() {
        assert_eq!(parse_drives(b"C:l\0D:c\0\0"), [("C:\\".to_string(), 'l'), ("D:\\".to_string(), 'c')]);
    }
}
