//! Finds a sharing computer on the local network from its session code alone, so the person helping
//! types the code and the PIN and nothing else.
//!
//! The viewer broadcasts `LUCARNA-FIND1 <code>` over UDP; a host sharing under that code answers
//! `LUCARNA-HERE1 <code>` plus its TCP port, and the viewer dials the address the answer came from.
//! The code is not a secret and the answer proves nothing: whoever answers still has to pass the PIN
//! exchange (SPAKE2), so a fake answer only makes the connection fail.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// UDP port the hosts listen on (next to the default host TCP port, 5950).
pub const PORT: u16 = 5951;
const FIND: &[u8] = b"LUCARNA-FIND1 ";
const HERE: &[u8] = b"LUCARNA-HERE1 ";
const SEARCH: Duration = Duration::from_millis(1500);

/// True when `host` looks like a session code (6 to 9 digits) and not like a host name.
pub fn is_code(host: &str) -> bool {
    (6..=9).contains(&host.len()) && host.bytes().all(|b| b.is_ascii_digit())
}

/// Host side: answers searches for `code` with `tcp_port` until `stop` is set. Does nothing when the
/// port is taken (another Lucarna sharing on this computer); the direct address still works then.
pub fn respond(code: String, tcp_port: u16, stop: Arc<AtomicBool>) {
    if !is_code(&code) {
        return;
    }
    if let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT)) {
        std::thread::spawn(move || respond_on(&socket, &code, tcp_port, &stop));
    }
}

fn respond_on(socket: &UdpSocket, code: &str, tcp_port: u16, stop: &AtomicBool) {
    let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));
    let mut buf = [0u8; 64];
    while !stop.load(Ordering::SeqCst) {
        let Ok((n, from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        if buf[..n].strip_prefix(FIND) == Some(code.as_bytes()) {
            let mut reply = [HERE, code.as_bytes()].concat();
            reply.extend_from_slice(&tcp_port.to_be_bytes());
            let _ = socket.send_to(&reply, from);
        }
    }
}

/// Viewer side: the address of the computer sharing under `code` on this network.
pub fn find(code: &str) -> io::Result<SocketAddr> {
    // ponytail: limited broadcast leaves by the default interface only; send to each interface's
    // subnet broadcast if computers with several networks miss each other.
    find_at(
        code,
        &[
            (Ipv4Addr::BROADCAST, PORT).into(),
            (Ipv4Addr::LOCALHOST, PORT).into(),
        ],
        SEARCH,
    )
}

fn find_at(code: &str, targets: &[SocketAddr], search: Duration) -> io::Result<SocketAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(250)))?;
    let ask = [FIND, code.as_bytes()].concat();
    let mut expected = [HERE, code.as_bytes()].concat();
    let prefix = expected.len();
    expected.extend_from_slice(&[0, 0]);
    let started = Instant::now();
    let mut buf = [0u8; 64];
    while started.elapsed() < search {
        for target in targets {
            // One target failing (no route for broadcasts, say) must not hide the other.
            let _ = socket.send_to(&ask, target);
        }
        while let Ok((n, from)) = socket.recv_from(&mut buf) {
            if n == expected.len() && buf[..prefix] == expected[..prefix] {
                let port = u16::from_be_bytes([buf[prefix], buf[prefix + 1]]);
                return Ok(SocketAddr::new(from.ip(), port));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "No device with that code on this network",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_is_six_to_nine_digits() {
        assert!(is_code("122977"));
        assert!(!is_code("12297"));
        assert!(!is_code("studio"));
        assert!(!is_code("10.0.0.5"));
    }

    #[test]
    fn the_viewer_finds_the_host_that_shares_under_its_code() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let at = socket.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let host = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || respond_on(&socket, "122977", 5950, &stop))
        };
        let found = find_at("122977", &[at], Duration::from_secs(2)).unwrap();
        assert_eq!(found, SocketAddr::new(at.ip(), 5950));
        // Another code gets no answer.
        let missing = find_at("999999", &[at], Duration::from_millis(300));
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::NotFound);
        stop.store(true, Ordering::SeqCst);
        host.join().unwrap();
    }
}
