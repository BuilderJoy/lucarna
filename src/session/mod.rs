//! Session aggregate: one live connection to a VNC server. Owns the socket and
//! routes each server message to the feature that understands it.

use std::io::{self, BufReader, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, mpsc::Sender};
use std::time::Duration;

use crate::features::chat;
use crate::features::clipboard::{self, RemoteClipboard};
use crate::features::connection::{Credentials, ServerAddress, SshTarget, handshake};
use crate::features::ssh::Tunnel;
use crate::features::file_transfer::{self, RemoteFiles};
use crate::features::remote_screen::{self, Framebuffer};
use crate::shared::wire::{invalid, read_u8};

pub enum Event {
    Connected {
        name: String,
        outbound: Arc<Outbound>,
        screen: Arc<Mutex<Framebuffer>>,
        files: Arc<RemoteFiles>,
        clipboard: Arc<RemoteClipboard>,
    },
    ScreenUpdated,
    Bell,
    Chat(chat::Msg),
    Files(file_transfer::Update),
    ServerClipboard(String),
    Closed(String),
}

/// Client-to-server channel, shared by the UI and the reader thread.
pub struct Outbound(Mutex<TcpStream>);

impl Outbound {
    pub fn send(&self, msg: &[u8]) -> io::Result<()> {
        self.0.lock().unwrap().write_all(msg)
    }

    pub fn close(&self) {
        let _ = self.0.lock().unwrap().shutdown(Shutdown::Both);
    }
}

/// Connects and runs the session on the calling thread until it ends.
/// With a `repeater`, we dial it instead and tell it to reach `address` (host:port or `ID:number`).
/// With `ssh`, everything goes through an SSH tunnel that lives exactly as long as the session.
pub fn run(
    address: &ServerAddress,
    repeater: Option<&ServerAddress>,
    ssh: Option<&SshTarget>,
    creds: &Credentials,
    tx: Sender<Event>,
    notify: impl Fn() + Clone + Send + Sync + 'static,
) {
    let mut tunnel: Option<Tunnel> = None;
    let result: io::Result<()> = (|| {
        let mut s = match ssh {
            Some(_) if repeater.is_some() => return Err(invalid("SSH and a repeater can't be combined")),
            Some(target) => {
                let (t, s) = Tunnel::open(target, address)?;
                tunnel = Some(t);
                s
            }
            None => connect(repeater.unwrap_or(address))?,
        };
        s.set_nodelay(true)?;
        // A server that accepts but never speaks would otherwise hang the handshake forever.
        s.set_read_timeout(Some(TIMEOUT))?;
        if repeater.is_some() {
            handshake::via_repeater(&mut s, address)?;
        }
        let init = handshake::perform(&mut s, creds)?;
        s.set_read_timeout(None)?;
        let screen = Arc::new(Mutex::new(Framebuffer::new(init.width.into(), init.height.into())?));
        remote_screen::negotiate(&mut s, &[file_transfer::ENC_PROTOCOL_VERSION, clipboard::ENC_EXTENDED_CLIPBOARD])?;
        let outbound = Arc::new(Outbound(Mutex::new(s.try_clone()?)));
        remote_screen::request_update(&outbound, false, &screen)?;
        let (tx_files, notify_files) = (tx.clone(), notify.clone());
        let files = Arc::new(RemoteFiles::new(outbound.clone(), move |u| {
            let _ = tx_files.send(Event::Files(u));
            notify_files();
        }));
        let clipboard = Arc::new(RemoteClipboard::new(outbound.clone()));
        let _ = tx.send(Event::Connected {
            name: init.name,
            outbound: outbound.clone(),
            screen: screen.clone(),
            files: files.clone(),
            clipboard: clipboard.clone(),
        });
        // Server messages are parsed a few bytes at a time; unbuffered, each read is a syscall.
        let mut s = BufReader::with_capacity(1 << 16, s);
        let mut decoder = remote_screen::Decoder::default();
        let mut receiver = file_transfer::Receiver::default();

        loop {
            match read_u8(&mut s)? {
                0 => {
                    remote_screen::read_update(&mut s, &screen, &mut decoder)?;
                    let _ = tx.send(Event::ScreenUpdated);
                    notify();
                    remote_screen::request_update(&outbound, true, &screen)?;
                }
                1 => remote_screen::skip_colour_map(&mut s)?,
                2 => {
                    let _ = tx.send(Event::Bell);
                    notify();
                }
                3 => {
                    if let Some(text) = clipboard.read_server_msg(&mut s)? {
                        let _ = tx.send(Event::ServerClipboard(text));
                        notify();
                    }
                }
                11 => {
                    let _ = tx.send(Event::Chat(chat::read(&mut s)?));
                    notify();
                }
                7 => {
                    if let Some(update) = receiver.handle(file_transfer::read(&mut s)?, &files)? {
                        let _ = tx.send(Event::Files(update));
                        notify();
                    }
                }
                t => return Err(invalid(format!("unknown server message {t}"))),
            }
        }
    })();
    // When the tunnel broke (or could not reach the VNC server), ssh's message beats a bare "connection closed".
    let tunnel_error = result.as_ref().err().and(tunnel.as_mut()).and_then(Tunnel::error);
    let _ = tx.send(Event::Closed(match (result, tunnel_error) {
        (_, Some(why)) => why,
        (Err(e), None) if e.kind() != io::ErrorKind::UnexpectedEof => e.to_string(),
        _ => "connection closed".into(),
    }));
    notify();
    drop(tunnel); // closes the SSH tunnel
}

const TIMEOUT: Duration = Duration::from_secs(10);

/// Tries every resolved address (IPv6 and IPv4) with a timeout, instead of the OS default of minutes.
fn connect(address: &ServerAddress) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "host not found");
    address
        .as_str()
        .to_socket_addrs()?
        .find_map(|a| TcpStream::connect_timeout(&a, TIMEOUT).map_err(|e| last = e).ok())
        .ok_or(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// VNC_TEST_ADDR=host:port VNC_TEST_PASSWORD=pw cargo test -- --ignored
    #[test]
    #[ignore = "needs a live VNC server"]
    fn live_server() {
        let address = ServerAddress::parse(&std::env::var("VNC_TEST_ADDR").unwrap()).unwrap();
        let password = std::env::var("VNC_TEST_PASSWORD").unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        run(
            &address,
            None,
            None,
            &Credentials {
                user: String::new(),
                password: "wrong-pw".into(),
            },
            tx,
            || {},
        );
        assert!(matches!(rx.recv().unwrap(), Event::Closed(r) if r.contains("authentication failed")));

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            run(
                &address,
                None,
                None,
                &Credentials {
                    user: String::new(),
                    password,
                },
                tx,
                || {},
            )
        });
        let Event::Connected {
            outbound, screen, files, ..
        } = rx.recv().unwrap()
        else {
            panic!("not connected")
        };
        assert!(matches!(rx.recv().unwrap(), Event::ScreenUpdated));
        assert!(screen.lock().unwrap().pixels.iter().any(|&b| b != 0), "screen is blank");

        // The server sends its cursor shape once we advertise the Cursor pseudo-encoding.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while screen.lock().unwrap().cursor.is_none() {
            assert!(std::time::Instant::now() < deadline, "no cursor shape received");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let cursor = screen.lock().unwrap().cursor.take().unwrap();
        assert!(cursor.size[0] > 0 && cursor.rgba.chunks(4).any(|p| p[3] == 255), "cursor has visible pixels");

        // File transfer (x11vnc -ultrafilexfer maps C:\ to /).
        let next_files = || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                match rx.recv_timeout(left).expect("file transfer answer") {
                    Event::Files(u) => break u,
                    Event::Closed(r) => panic!("closed: {r}"),
                    _ => {}
                }
            }
        };
        files.open();
        assert!(matches!(next_files(), file_transfer::Update::Access(true)));
        assert!(matches!(next_files(), file_transfer::Update::Drives(d) if !d.is_empty()));
        files.list("C:\\srv\\ft\\");
        let file_transfer::Update::Listing { entries, .. } = next_files() else {
            panic!("no listing")
        };
        let random = entries.iter().find(|e| e.name == "random.bin").expect("random.bin listed");
        assert_eq!((random.size, random.is_dir), (100_000, false));
        let tmp = std::env::temp_dir();
        for (name, size) in [("hello.txt", 27), ("random.bin", 100_000)] {
            files.download(&format!("C:\\srv\\ft\\{name}"), &tmp);
            let dest = loop {
                match next_files() {
                    file_transfer::Update::Progress { .. } => {}
                    file_transfer::Update::Downloaded(dest) => break dest,
                    u => panic!("download of {name} failed: {u:?}"),
                }
            };
            let len = std::fs::metadata(&dest).unwrap().len();
            std::fs::remove_file(&dest).unwrap();
            assert_eq!(len, size, "{name}");
        }

        // Write side: create dir, upload multi-block file, rename, download back, and delete.
        files.delete("C:\\srv\\ft\\sub\\uploaded.bin");
        files.delete("C:\\srv\\ft\\sub\\renamed.bin");
        files.delete("C:\\srv\\ft\\sub");
        let _ = next_files();
        let _ = next_files();
        let _ = next_files();

        files.create_dir("C:\\srv\\ft\\sub");
        assert!(matches!(next_files(), file_transfer::Update::Command { ok: true, .. }));

        let local_src = tmp.join("uploaded.bin");
        let payload: Vec<u8> = (0..20_000u32).map(|i| (i.wrapping_mul(31) & 0xff) as u8).collect();
        std::fs::write(&local_src, &payload).unwrap();
        files.upload(&local_src, "C:\\srv\\ft\\sub\\");
        loop {
            match next_files() {
                file_transfer::Update::Progress { .. } => {}
                file_transfer::Update::Uploaded(name) => {
                    assert_eq!(name, "uploaded.bin");
                    break;
                }
                u => panic!("upload failed: {u:?}"),
            }
        }
        std::fs::remove_file(&local_src).unwrap();

        files.rename("C:\\srv\\ft\\sub\\uploaded.bin", "C:\\srv\\ft\\sub\\renamed.bin");
        assert!(matches!(next_files(), file_transfer::Update::Command { ok: true, .. }));

        files.download("C:\\srv\\ft\\sub\\renamed.bin", &tmp);
        let dest = loop {
            match next_files() {
                file_transfer::Update::Progress { .. } => {}
                file_transfer::Update::Downloaded(dest) => break dest,
                u => panic!("download of renamed.bin failed: {u:?}"),
            }
        };
        let downloaded = std::fs::read(&dest).unwrap();
        std::fs::remove_file(&dest).unwrap();
        assert_eq!(downloaded, payload);

        files.delete("C:\\srv\\ft\\sub\\renamed.bin");
        assert!(matches!(next_files(), file_transfer::Update::Command { ok: true, .. }));
        files.delete("C:\\srv\\ft\\sub");
        assert!(matches!(next_files(), file_transfer::Update::Command { ok: true, .. }));

        outbound.close();
    }

    /// Same server, reached through a minimal UltraVNC repeater built from UltraVNC's repeater source:
    /// greet with version 0.0, read the 250-byte target, then relay both ways.
    #[test]
    #[ignore = "needs a live VNC server"]
    fn live_repeater() {
        use std::io::Read;
        let target = ServerAddress::parse(&std::env::var("VNC_TEST_ADDR").unwrap()).unwrap();
        let password = std::env::var("VNC_TEST_PASSWORD").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let repeater = ServerAddress::parse(&format!("127.0.0.1:{}", listener.local_addr().unwrap().port())).unwrap();
        std::thread::spawn(move || {
            let (mut viewer, _) = listener.accept().unwrap();
            viewer.write_all(b"RFB 000.000\n").unwrap();
            let mut asked = [0u8; 250];
            viewer.read_exact(&mut asked).unwrap();
            let end = asked.iter().position(|&b| b == 0).unwrap();
            let mut server = TcpStream::connect(std::str::from_utf8(&asked[..end]).unwrap()).unwrap();
            let (mut from_viewer, mut to_server) = (viewer.try_clone().unwrap(), server.try_clone().unwrap());
            std::thread::spawn(move || drop(io::copy(&mut from_viewer, &mut to_server)));
            let _ = io::copy(&mut server, &mut viewer);
        });

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || run(&target, Some(&repeater), None, &Credentials { user: String::new(), password }, tx, || {}));
        let Event::Connected { outbound, .. } = rx.recv().unwrap() else { panic!("not connected through the repeater") };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left).expect("a screen update through the repeater") {
                Event::ScreenUpdated => break,
                Event::Closed(r) => panic!("closed: {r}"),
                _ => {}
            }
        }
        outbound.close();
    }


    /// Tight must reproduce exactly what Raw shows when JPEG is off, and stay close to it with JPEG on.
    #[test]
    #[ignore = "needs a live VNC server"]
    fn live_tight_matches_raw() {
        let address = ServerAddress::parse(&std::env::var("VNC_TEST_ADDR").unwrap()).unwrap();
        let password = std::env::var("VNC_TEST_PASSWORD").unwrap();
        let grab = |encodings: &[i32]| -> Vec<u8> {
            let creds = Credentials { user: String::new(), password: password.clone() };
            let mut s = connect(&address).unwrap();
            s.set_read_timeout(Some(TIMEOUT)).unwrap();
            let init = handshake::perform(&mut s, &creds).unwrap();
            let screen = Mutex::new(Framebuffer::new(init.width.into(), init.height.into()).unwrap());
            remote_screen::negotiate(&mut s, &[]).unwrap();
            // A second SetEncodings replaces the first: from here on only `encodings` may be used.
            let mut m = vec![2, 0];
            m.extend((encodings.len() as u16).to_be_bytes());
            encodings.iter().for_each(|e| m.extend(e.to_be_bytes()));
            s.write_all(&m).unwrap();
            let out = Outbound(Mutex::new(s.try_clone().unwrap()));
            remote_screen::request_update(&out, false, &screen).unwrap();
            let mut r = BufReader::new(s);
            assert_eq!(read_u8(&mut r).unwrap(), 0, "expected a FramebufferUpdate");
            remote_screen::read_update(&mut r, &screen, &mut remote_screen::Decoder::default()).unwrap();
            out.close();
            screen.into_inner().unwrap().pixels.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect()
        };
        let raw = grab(&[0]);
        assert!(grab(&[7]) == raw, "Tight without JPEG differs from Raw");
        let jpeg = grab(&[7, -24]);
        let drift = raw.iter().zip(&jpeg).map(|(a, b)| u64::from(a.abs_diff(*b))).sum::<u64>() as f64 / raw.len() as f64;
        assert!(drift < 3.0, "Tight with JPEG drifts too far from Raw: {drift:.2} per channel");
    }

    /// VNC through an SSH tunnel to a server whose VNC port is not reachable from here, so the picture
    /// can only arrive through the tunnel. Needs SSH_TEST_TARGET (e.g. lucarna@127.0.0.1:2222) with key auth.
    #[test]
    #[ignore = "needs a live SSH + VNC server"]
    fn live_ssh_tunnel() {
        let target = SshTarget::parse(&std::env::var("SSH_TEST_TARGET").unwrap()).unwrap().unwrap();
        let password = std::env::var("VNC_TEST_PASSWORD").unwrap();
        // The VNC host is the SSH host, so the tunnel ends at the server's loopback.
        let vnc = ServerAddress::parse(&target.host).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || run(&vnc, None, Some(&target), &Credentials { user: String::new(), password }, tx, || {}));
        let event = rx.recv_timeout(Duration::from_secs(30)).expect("an event");
        let Event::Connected { outbound, .. } = event else {
            let Event::Closed(why) = event else { panic!("unexpected event") };
            panic!("not connected through SSH: {why}")
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).expect("a screen update") {
                Event::ScreenUpdated => break,
                Event::Closed(r) => panic!("closed: {r}"),
                _ => {}
            }
        }
        outbound.close();
    }
}
