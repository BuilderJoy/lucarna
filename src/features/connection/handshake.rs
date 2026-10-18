//! RFB version, security and init exchange (RFC 6143 §7.1–7.3).

use std::io::{self, Read, Write};

use super::auth::{self, Credentials};
use super::mslogon;
use super::ServerAddress;
use crate::shared::wire::{invalid, read_bytes, read_string, read_u16, read_u32, read_u8};

#[derive(Debug)]
pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub name: String,
}

const NONE: u8 = 1;
const VNC_AUTH: u8 = 2;
const MS_LOGON_II: u8 = 0x71;
const MS_LOGON_III: u8 = 0x76;
/// RFB 3.3 servers announce MS-Logon II with this 32-bit value.
const LEGACY_MS_LOGON: u32 = 0xFFFF_FFFA;
/// Auth result meaning "another authentication layer follows" (UltraVNC connection prompt, plugins).
const AUTH_CONTINUE: u32 = 0xFFFF_FFFF;

/// Picks a security type from a 3.7+ server's list. MS-Logon only when a user name was given.
fn pick(types: &[u8], creds: &Credentials) -> io::Result<u8> {
    let order: &[u8] = if creds.user.is_empty() { &[VNC_AUTH, NONE] } else { &[MS_LOGON_III, MS_LOGON_II, VNC_AUTH, NONE] };
    if let Some(&t) = order.iter().find(|t| types.contains(t)) {
        return Ok(t);
    }
    if types.contains(&MS_LOGON_III) || types.contains(&MS_LOGON_II) {
        return Err(invalid("this server needs a Windows user name (MS-Logon)"));
    }
    Err(invalid(format!("unsupported security types {types:?}")))
}

/// UltraVNC repeater: it greets with version 0.0, then wants the real target as a 250-byte NUL-padded
/// "host:port" or "ID:number", and relays the server's own RFB version after that.
pub fn via_repeater(s: &mut (impl Read + Write), target: &ServerAddress) -> io::Result<()> {
    if read_bytes(s, 12)? != b"RFB 000.000\n" {
        return Err(invalid("not an UltraVNC repeater"));
    }
    let target = target.as_str().as_bytes();
    if target.len() >= 250 {
        return Err(invalid("repeater target too long"));
    }
    let mut msg = [0u8; 250];
    msg[..target.len()].copy_from_slice(target);
    s.write_all(&msg)
}

pub fn perform(s: &mut (impl Read + Write), creds: &Credentials) -> io::Result<ServerInit> {
    let ver = read_bytes(s, 12)?;
    if &ver[..4] != b"RFB " {
        return Err(invalid("not a VNC server"));
    }
    let minor: u32 = std::str::from_utf8(&ver[8..11]).ok().and_then(|v| v.parse().ok()).ok_or_else(|| invalid("bad version"))?;
    let legacy = minor < 7; // RFB 3.3
    s.write_all(if legacy { b"RFB 003.003\n" } else { b"RFB 003.008\n" })?;

    let sec = if legacy {
        match read_u32(s)? {
            0 => return Err(invalid(read_string(s, 4096)?)),
            LEGACY_MS_LOGON if creds.user.is_empty() => return Err(invalid("this server needs a Windows user name (MS-Logon)")),
            LEGACY_MS_LOGON => MS_LOGON_II,
            t @ (1 | 2) => t as u8,
            t => return Err(invalid(format!("unsupported security type {t}"))),
        }
    } else {
        let n = read_u8(s)?;
        if n == 0 {
            return Err(invalid(read_string(s, 4096)?));
        }
        let types = read_bytes(s, n as usize)?;
        let choice = pick(&types, creds)?;
        s.write_all(&[choice])?;
        choice
    };
    match sec {
        NONE => {}
        VNC_AUTH => {
            let challenge: [u8; 16] = read_bytes(s, 16)?.try_into().unwrap();
            s.write_all(&auth::vnc_response(&creds.password, &challenge))?;
        }
        MS_LOGON_II => mslogon::logon_ii(s, creds)?,
        MS_LOGON_III => mslogon::logon_iii(s, creds)?,
        t => return Err(invalid(format!("unsupported security type {t}"))),
    }
    if !(legacy && sec == NONE) {
        match read_u32(s)? {
            0 => {}
            AUTH_CONTINUE => return Err(invalid("the server asks for an extra login step Lucarna doesn't support")),
            _ => {
                let reason = if legacy { String::new() } else { read_string(s, 4096)? };
                return Err(invalid(format!("authentication failed {reason}").trim_end().to_string()));
            }
        }
    }

    s.write_all(&[1])?; // ClientInit: shared session
    let width = read_u16(s)?;
    let height = read_u16(s)?;
    read_bytes(s, 16)?; // server pixel format, overridden by remote_screen::negotiate
    let name = read_string(s, 4096)?;
    Ok(ServerInit { width, height, name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::wire::Duplex;

    fn creds(user: &str) -> Credentials {
        Credentials { user: user.into(), password: "pw".into() }
    }

    fn init() -> Vec<u8> {
        let mut m = vec![0, 4, 0, 3]; // 4×3 screen
        m.extend([0; 16]);
        m.extend(2u32.to_be_bytes());
        m.extend(b"pc");
        m
    }

    #[test]
    fn repeater_receives_the_target_padded_to_250_bytes() {
        let mut conn = Duplex::new(b"RFB 000.000\n".to_vec());
        via_repeater(&mut conn, &ServerAddress::parse("ID:1234").unwrap()).unwrap();
        assert_eq!(conn.output.len(), 250);
        assert_eq!(&conn.output[..8], b"ID:1234\0");
        assert!(conn.output[7..].iter().all(|&b| b == 0));
        // A plain server (or anything else) behind the "repeater" is refused rather than misread.
        let err = via_repeater(&mut Duplex::new(b"RFB 003.008\n".to_vec()), &ServerAddress::parse("pc").unwrap());
        assert!(err.unwrap_err().to_string().contains("not an UltraVNC repeater"));
        let long = ServerAddress::parse(&"h".repeat(250)).unwrap();
        assert!(via_repeater(&mut Duplex::new(b"RFB 000.000\n".to_vec()), &long).is_err());
    }

    #[test]
    fn picks_by_whether_a_user_was_given() {
        assert_eq!(pick(&[17, 0x75, 0x76, 0x71], &creds("ana")).unwrap(), MS_LOGON_III);
        assert_eq!(pick(&[17, 0x71], &creds("ana")).unwrap(), MS_LOGON_II);
        assert_eq!(pick(&[17, 2], &creds("ana")).unwrap(), VNC_AUTH);
        assert_eq!(pick(&[17, 2], &creds("")).unwrap(), VNC_AUTH);
        assert!(pick(&[17, 0x76, 0x71], &creds("")).unwrap_err().to_string().contains("user name"));
        assert!(pick(&[17, 0x68], &creds("ana")).unwrap_err().to_string().contains("unsupported"));
    }

    #[test]
    fn vnc_auth_over_rfb_38() {
        let mut input = b"RFB 003.008
".to_vec();
        input.extend([2, 17, 2]); // two types: UltraVNC marker and VNC auth
        input.extend([5u8; 16]); // challenge
        input.extend(0u32.to_be_bytes());
        input.extend(init());
        let mut conn = Duplex::new(input);
        let got = perform(&mut conn, &creds("")).unwrap();
        assert_eq!((got.width, got.height, got.name.as_str()), (4, 3, "pc"));
        assert_eq!(&conn.output[..13], b"RFB 003.008
");
        assert_eq!(conn.output[13..29], auth::vnc_response("pw", &[5; 16]));
    }

    #[test]
    fn ms_logon_iii_over_rfb_38() {
        let server = x25519_dalek::StaticSecret::from([3u8; 32]);
        let mut input = b"RFB 003.008
".to_vec();
        input.extend([4, 17, 0x75, 0x76, 0x71]);
        input.extend(x25519_dalek::PublicKey::from(&server).as_bytes());
        input.extend(0u32.to_be_bytes());
        input.extend(init());
        let mut conn = Duplex::new(input);
        perform(&mut conn, &creds("ana")).unwrap();
        assert_eq!(conn.output[12], MS_LOGON_III, "chooses III over II");
        assert_eq!(conn.output.len(), 12 + 1 + 32 + 256 + 64 + 1, "version, choice, key, credentials, ClientInit");
    }

    #[test]
    fn legacy_rfb_33_ms_logon() {
        let (generator, modulus, public) = (7u64, 2_147_483_629u64, 1_000u64);
        let mut input = b"RFB 003.003
".to_vec();
        input.extend(LEGACY_MS_LOGON.to_be_bytes());
        input.extend([generator, modulus, public].iter().flat_map(|v| v.to_be_bytes()));
        input.extend(0u32.to_be_bytes());
        input.extend(init());
        assert!(perform(&mut Duplex::new(input.clone()), &creds("ana")).is_ok());
        assert!(perform(&mut Duplex::new(input), &creds("")).unwrap_err().to_string().contains("user name"));
    }

    #[test]
    fn failures_are_reported_and_continue_is_refused() {
        let reply = |result: u32, reason: &str| {
            let mut input = b"RFB 003.008
".to_vec();
            input.extend([1, 1]); // None auth, so no exchange before the result
            input.extend(result.to_be_bytes());
            input.extend((reason.len() as u32).to_be_bytes());
            input.extend(reason.as_bytes());
            perform(&mut Duplex::new(input), &creds("")).unwrap_err().to_string()
        };
        assert_eq!(reply(1, "authentication rejected"), "authentication failed authentication rejected");
        assert!(reply(AUTH_CONTINUE, "").contains("extra login step"));
    }
}
