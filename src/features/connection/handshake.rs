//! RFB version, security and init exchange (RFC 6143 §7.1–7.3).

use std::io::{self, Read, Write};

use super::auth;
use crate::shared::wire::{invalid, read_bytes, read_string, read_u16, read_u32, read_u8};

pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub name: String,
}

pub fn perform(s: &mut (impl Read + Write), password: &str) -> io::Result<ServerInit> {
    let ver = read_bytes(s, 12)?;
    if &ver[..4] != b"RFB " {
        return Err(invalid("not a VNC server"));
    }
    let minor: u32 = std::str::from_utf8(&ver[8..11]).ok().and_then(|v| v.parse().ok()).ok_or_else(|| invalid("bad version"))?;
    let legacy = minor < 7; // RFB 3.3
    s.write_all(if legacy { b"RFB 003.003\n" } else { b"RFB 003.008\n" })?;

    let sec = if legacy {
        read_u32(s)? as u8
    } else {
        let n = read_u8(s)?;
        if n == 0 {
            return Err(invalid(read_string(s, 4096)?));
        }
        let types = read_bytes(s, n as usize)?;
        let pick = [2u8, 1].into_iter().find(|t| types.contains(t)).ok_or_else(|| invalid(format!("unsupported security types {types:?}")))?;
        s.write_all(&[pick])?;
        pick
    };
    match sec {
        0 => return Err(invalid(read_string(s, 4096)?)),
        1 => {}
        2 => {
            let challenge: [u8; 16] = read_bytes(s, 16)?.try_into().unwrap();
            s.write_all(&auth::vnc_response(password, &challenge))?;
        }
        t => return Err(invalid(format!("unsupported security type {t}"))),
    }
    if !(legacy && sec == 1) && read_u32(s)? != 0 {
        let reason = if legacy { String::new() } else { read_string(s, 4096)? };
        return Err(invalid(format!("authentication failed {reason}").trim_end().to_string()));
    }

    s.write_all(&[1])?; // ClientInit: shared session
    let width = read_u16(s)?;
    let height = read_u16(s)?;
    read_bytes(s, 16)?; // server pixel format, overridden by remote_screen::negotiate
    let name = read_string(s, 4096)?;
    Ok(ServerInit { width, height, name })
}
