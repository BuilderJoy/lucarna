//! Session aggregate: one live connection to a VNC server. Owns the socket and
//! routes each server message to the feature that understands it.

use std::io::{self, BufReader, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{mpsc::Sender, Arc, Mutex};
use std::time::Duration;

use crate::features::connection::{handshake, Credentials, ServerAddress};
use crate::features::remote_screen::{self, Framebuffer};
use crate::features::clipboard;
use crate::features::file_transfer::{self, RemoteFiles};
use crate::shared::wire::{invalid, read_u8};

pub enum Event {
    Connected { name: String, outbound: Arc<Outbound>, screen: Arc<Mutex<Framebuffer>>, files: Arc<RemoteFiles> },
    ScreenUpdated,
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
pub fn run(address: &ServerAddress, creds: &Credentials, tx: Sender<Event>, notify: impl Fn() + Clone + Send + Sync + 'static) {
    let result: io::Result<()> = (|| {
        let mut s = connect(address)?;
        s.set_nodelay(true)?;
        // A server that accepts but never speaks would otherwise hang the handshake forever.
        s.set_read_timeout(Some(TIMEOUT))?;
        let init = handshake::perform(&mut s, creds)?;
        s.set_read_timeout(None)?;
        let screen = Arc::new(Mutex::new(Framebuffer::new(init.width.into(), init.height.into())?));
        remote_screen::negotiate(&mut s, &[file_transfer::ENC_PROTOCOL_VERSION])?;
        let outbound = Arc::new(Outbound(Mutex::new(s.try_clone()?)));
        remote_screen::request_update(&outbound, false, &screen)?;
        let (tx_files, notify_files) = (tx.clone(), notify.clone());
        let files = Arc::new(RemoteFiles::new(outbound.clone(), move |u| {
            let _ = tx_files.send(Event::Files(u));
            notify_files();
        }));
        let _ = tx.send(Event::Connected { name: init.name, outbound: outbound.clone(), screen: screen.clone(), files: files.clone() });
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
                2 => {} // bell
                3 => {
                    let _ = tx.send(Event::ServerClipboard(clipboard::read_server_text(&mut s)?));
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
    let _ = tx.send(Event::Closed(match result {
        Err(e) if e.kind() != io::ErrorKind::UnexpectedEof => e.to_string(),
        _ => "connection closed".into(),
    }));
    notify();
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
        run(&address, &Credentials { user: String::new(), password: "wrong-pw".into() }, tx, || {});
        assert!(matches!(rx.recv().unwrap(), Event::Closed(r) if r.contains("authentication failed")));

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || run(&address, &Credentials { user: String::new(), password }, tx, || {}));
        let Event::Connected { outbound, screen, files, .. } = rx.recv().unwrap() else { panic!("not connected") };
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
        let file_transfer::Update::Listing { entries, .. } = next_files() else { panic!("no listing") };
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
}
