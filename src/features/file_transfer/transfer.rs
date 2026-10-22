//! Session-thread side of file transfer: assembles listings and streams downloads and uploads.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use super::protocol::{
    self, A_DIR_CREATE, A_DIR_INACCESSIBLE, A_DIRECTORY, A_FILE, A_FILE_DELETE, A_FILE_RENAME,
    DEFAULT_BLOCK, Entry, MAX_BLOCK, Msg, UNICODE,
};
use crate::session::Outbound;
use crate::shared::wire::invalid;

const MAX_ENTRIES: usize = 100_000;

/// What the UI sees.
#[derive(Debug)]
pub enum Update {
    Access(bool),
    Drives(Vec<(String, char)>),
    Listing { path: String, entries: Vec<Entry> },
    Inaccessible,
    Progress { name: String, done: u64, total: u64 },
    Downloaded(PathBuf),
    Uploaded(String),
    Command { ok: bool, message: String },
    Failed(String),
}

struct Upload {
    file: File,
    name: String,
    total: u64,
}

/// UI handle: sends requests and remembers which file was asked for or offered.
pub struct RemoteFiles {
    out: Arc<Outbound>,
    emit: Arc<dyn Fn(Update) + Send + Sync>,
    requested: Mutex<Option<(String, PathBuf)>>,
    offered: Mutex<Option<Upload>>,
    block_size: AtomicUsize,
    cancelled: Arc<AtomicBool>,
}

impl RemoteFiles {
    pub fn new(out: Arc<Outbound>, emit: impl Fn(Update) + Send + Sync + 'static) -> Self {
        Self {
            out,
            emit: Arc::new(emit),
            requested: Mutex::new(None),
            offered: Mutex::new(None),
            block_size: AtomicUsize::new(DEFAULT_BLOCK),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    // Send errors surface through the session reader as a Closed event, so they are ignored here.
    pub fn open(&self) {
        let _ = self
            .out
            .send(&[protocol::permission_request(), protocol::drives_request()].concat());
    }

    pub fn list(&self, path: &str) {
        let _ = self.out.send(&protocol::dir_request(path));
    }

    pub fn download(&self, path: &str, dir: &Path) {
        self.cancelled.store(false, Ordering::SeqCst);
        *self.requested.lock().unwrap() = Some((path.to_string(), dir.to_path_buf()));
        let _ = self.out.send(&protocol::download_request(path));
    }

    /// Offers `local` to the server under `remote_dir` (which must end in `\`).
    pub fn upload(&self, local: &Path, remote_dir: &str) {
        let name = local
            .file_name()
            .and_then(|n| n.to_str())
            .map(local_name)
            .unwrap_or_else(|| "upload".into());
        let (file, meta) = match File::open(local).and_then(|f| f.metadata().map(|m| (f, m))) {
            Ok((f, m)) if m.is_file() => (f, m),
            Ok(_) => return (self.emit)(Update::Failed(format!("{name} is not a regular file"))),
            Err(e) => return (self.emit)(Update::Failed(format!("{name}: {e}"))),
        };
        let total = meta.len();
        let spec = format!(
            "{remote_dir}{name}{}",
            offer_timestamp(meta.modified().ok())
        );
        self.cancelled.store(false, Ordering::SeqCst);
        *self.offered.lock().unwrap() = Some(Upload {
            file,
            name: name.clone(),
            total,
        });
        (self.emit)(Update::Progress {
            name,
            done: 0,
            total,
        });
        let _ = self.out.send(&protocol::upload_offer(&spec, total));
    }

    pub fn create_dir(&self, path: &str) {
        let _ = self.out.send(&protocol::create_dir(path));
    }

    pub fn delete(&self, path: &str) {
        let _ = self.out.send(&protocol::delete(path));
    }

    pub fn rename(&self, old: &str, new: &str) {
        let _ = self.out.send(&protocol::rename(old, new));
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(u) = self.offered.lock().unwrap().take() {
            (self.emit)(Update::Failed(format!("{} was aborted", u.name)));
        }
        let _ = self.out.send(&protocol::abort());
    }
}

/// Operations supported by a file transfer engine (UltraVNC FT or SFTP v3).
pub trait FileOps: Send + Sync {
    fn open(&self);
    fn list(&self, path: &str);
    fn download(&self, path: &str, dir: &Path);
    fn upload(&self, local: &Path, remote_dir: &str);
    fn create_dir(&self, path: &str);
    fn delete(&self, path: &str);
    fn rename(&self, old: &str, new: &str);
    fn cancel(&self);
}

impl FileOps for RemoteFiles {
    fn open(&self) {
        self.open();
    }
    fn list(&self, path: &str) {
        self.list(path);
    }
    fn download(&self, path: &str, dir: &Path) {
        self.download(path, dir);
    }
    fn upload(&self, local: &Path, remote_dir: &str) {
        self.upload(local, remote_dir);
    }
    fn create_dir(&self, path: &str) {
        self.create_dir(path);
    }
    fn delete(&self, path: &str) {
        self.delete(path);
    }
    fn rename(&self, old: &str, new: &str) {
        self.rename(old, new);
    }
    fn cancel(&self) {
        self.cancel();
    }
}

pub const BACKEND_VNC: u8 = 0;
#[allow(dead_code)]
pub const BACKEND_SFTP: u8 = 1;

/// Dual engine manager: combines UltraVNC FT (Motor A) and native SFTP over SSH (Motor B).
pub struct DualFiles {
    vnc: Arc<RemoteFiles>,
    sftp: Option<Arc<dyn FileOps>>,
    active: Arc<AtomicU8>,
}

impl DualFiles {
    pub fn new(vnc: Arc<RemoteFiles>, sftp: Option<Arc<dyn FileOps>>) -> Self {
        let initial = if sftp.is_some() { BACKEND_SFTP } else { BACKEND_VNC };
        Self {
            vnc,
            sftp,
            active: Arc::new(AtomicU8::new(initial)),
        }
    }

    pub fn is_sftp_active(&self) -> bool {
        self.active.load(Ordering::SeqCst) == BACKEND_SFTP
    }

    pub fn has_sftp(&self) -> bool {
        self.sftp.is_some()
    }

    pub fn switch_to_sftp(&self) -> bool {
        if let Some(sftp) = &self.sftp {
            self.active.store(BACKEND_SFTP, Ordering::SeqCst);
            sftp.open();
            true
        } else {
            false
        }
    }

    #[allow(dead_code)]
    pub fn switch_to_vnc(&self) {
        self.active.store(BACKEND_VNC, Ordering::SeqCst);
        self.vnc.open();
    }
}

impl FileOps for DualFiles {
    fn open(&self) {
        if self.is_sftp_active()
            && let Some(s) = &self.sftp
        {
            s.open();
            return;
        }
        self.vnc.open();
    }

    fn list(&self, path: &str) {
        if self.is_sftp_active()
            && let Some(s) = &self.sftp
        {
            s.list(path);
            return;
        }
        self.vnc.list(path);
    }

    fn download(&self, path: &str, dir: &Path) {
        if self.is_sftp_active()
            && let Some(s) = &self.sftp
        {
            s.download(path, dir);
            return;
        }
        self.vnc.download(path, dir);
    }

    fn upload(&self, local: &Path, remote_dir: &str) {
        if self.is_sftp_active()
            && let Some(s) = &self.sftp
        {
            s.upload(local, remote_dir);
            return;
        }
        self.vnc.upload(local, remote_dir);
    }

    fn create_dir(&self, path: &str) {
        if self.is_sftp_active()
            && let Some(s) = &self.sftp
        {
            s.create_dir(path);
            return;
        }
        self.vnc.create_dir(path);
    }

    fn delete(&self, path: &str) {
        if self.is_sftp_active()
            && let Some(s) = &self.sftp
        {
            s.delete(path);
            return;
        }
        self.vnc.delete(path);
    }

    fn rename(&self, old: &str, new: &str) {
        if self.is_sftp_active()
            && let Some(s) = &self.sftp
        {
            s.rename(old, new);
            return;
        }
        self.vnc.rename(old, new);
    }

    fn cancel(&self) {
        if self.is_sftp_active()
            && let Some(s) = &self.sftp
        {
            s.cancel();
            return;
        }
        self.vnc.cancel();
    }
}

struct Download {
    file: File,
    part: PathBuf,
    dest: PathBuf,
    name: String,
    total: u64,
    done: u64,
    reported: Instant,
}

impl Drop for Download {
    /// Unfinished downloads leave nothing behind; finished ones were already renamed.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.part);
    }
}

/// Owned by the session reader thread.
#[derive(Default)]
pub struct Receiver {
    listing: Option<(String, Vec<Entry>)>,
    download: Option<Download>,
}

impl Receiver {
    pub fn handle(&mut self, msg: Msg, files: &RemoteFiles) -> io::Result<Option<Update>> {
        Ok(match msg {
            Msg::ProtocolVersion {
                version,
                block_size,
            } => {
                if version >= 3 && (4096..=MAX_BLOCK as u32).contains(&block_size) {
                    files
                        .block_size
                        .store(block_size as usize, Ordering::Relaxed);
                }
                None
            }
            Msg::Access(ok) => Some(Update::Access(ok)),
            Msg::Drives(data) => Some(Update::Drives(protocol::parse_drives(&data))),
            Msg::Dir { param, data } => self.dir(param, &data)?,
            Msg::FileHeader { size, low } => {
                let Some((path, dir)) = files.requested.lock().unwrap().take() else {
                    files.out.send(&protocol::header_reply(None))?;
                    return Ok(None);
                };
                let Some(total) = size else {
                    return Ok(Some(Update::Failed(format!(
                        "the server could not open {path}"
                    ))));
                };
                match start(&path, total, &dir) {
                    Ok(d) => {
                        files.out.send(&protocol::header_reply(Some(low)))?;
                        let update = Update::Progress {
                            name: d.name.clone(),
                            done: 0,
                            total,
                        };
                        self.download = Some(d);
                        Some(update)
                    }
                    Err(e) => {
                        files.out.send(&protocol::header_reply(None))?;
                        Some(Update::Failed(e.to_string()))
                    }
                }
            }
            Msg::FileAccept(accepted) => {
                let Some(upload) = files.offered.lock().unwrap().take() else {
                    if accepted {
                        files.out.send(&protocol::abort())?;
                    }
                    return Ok(None);
                };
                if !accepted {
                    return Ok(Some(Update::Failed(format!(
                        "the server refused {}",
                        upload.name
                    ))));
                }
                let out = files.out.clone();
                let emit = files.emit.clone();
                let cancelled = files.cancelled.clone();
                let block_size = files.block_size.load(Ordering::Relaxed);
                std::thread::spawn(move || {
                    emit(stream_upload(upload, block_size, &out, &cancelled, &*emit))
                });
                None
            }
            Msg::FilePacket { compressed, data } => {
                if files.cancelled.load(Ordering::Relaxed) {
                    return Ok(self
                        .download
                        .take()
                        .map(|d| Update::Failed(format!("{} was aborted", d.name))));
                }
                let Some(d) = &mut self.download else {
                    return Ok(None);
                };
                match d.write(compressed, &data) {
                    Ok(()) if d.reported.elapsed() < Duration::from_millis(100) => None,
                    Ok(()) => {
                        d.reported = Instant::now();
                        Some(Update::Progress {
                            name: d.name.clone(),
                            done: d.done,
                            total: d.total,
                        })
                    }
                    Err(e) => {
                        self.download = None;
                        files.cancel();
                        Some(Update::Failed(e.to_string()))
                    }
                }
            }
            Msg::EndOfFile => self.download.take().map(|d| match d.finish() {
                Ok(dest) => Update::Downloaded(dest),
                Err(e) => Update::Failed(e.to_string()),
            }),
            Msg::Abort => {
                files.cancelled.store(true, Ordering::Relaxed);
                self.download
                    .take()
                    .map(|d| Update::Failed(format!("{} was aborted", d.name)))
            }
            Msg::CommandReturn { param, ok } => {
                let (done, err) = match param {
                    A_DIR_CREATE => (
                        "Folder created.",
                        "The server could not create that folder.",
                    ),
                    A_FILE_DELETE => ("Deleted.", "The server could not delete that item."),
                    A_FILE_RENAME => ("Renamed.", "The server could not rename that item."),
                    _ => ("Done.", "Command failed on the server."),
                };
                Some(Update::Command {
                    ok,
                    message: (if ok { done } else { err }).into(),
                })
            }
            Msg::Other => None,
        })
    }

    fn dir(&mut self, param: u16, data: &[u8]) -> io::Result<Option<Update>> {
        let unicode = param & UNICODE != 0;
        Ok(match param & !UNICODE {
            A_DIR_INACCESSIBLE => {
                self.listing = None;
                Some(Update::Inaccessible)
            }
            A_DIRECTORY | A_FILE => {
                match &mut self.listing {
                    // The first packet is the resolved directory path, without find data.
                    None => {
                        let path = if unicode {
                            String::from_utf8_lossy(data).into_owned()
                        } else {
                            protocol::ansi(data)
                        };
                        self.listing = Some((path, Vec::new()));
                    }
                    Some((_, entries)) if entries.len() >= MAX_ENTRIES => {
                        return Err(invalid("directory listing too long"));
                    }
                    Some((_, entries)) => entries.push(protocol::parse_entry(data, unicode)?),
                }
                None
            }
            _ => self
                .listing
                .take()
                .map(|(path, entries)| Update::Listing { path, entries }),
        })
    }
}

fn stream_upload(
    upload: Upload,
    block_size: usize,
    out: &Outbound,
    cancelled: &AtomicBool,
    progress: &dyn Fn(Update),
) -> Update {
    let name = upload.name;
    let total = upload.total;
    let mut reader = upload.file.take(total);
    let mut buf = vec![0u8; block_size];
    let mut done = 0u64;
    let mut reported = Instant::now();
    loop {
        if cancelled.load(Ordering::Relaxed) {
            let _ = out.send(&protocol::abort());
            return Update::Failed(format!("{name} was aborted"));
        }
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Err(e) = out.send(&encode_packet(&buf[..n])) {
                    return Update::Failed(e.to_string());
                }
                done += n as u64;
                if reported.elapsed() >= Duration::from_millis(100) {
                    reported = Instant::now();
                    progress(Update::Progress {
                        name: name.clone(),
                        done,
                        total,
                    });
                }
            }
            Err(e) => {
                let _ = out.send(&protocol::abort());
                return Update::Failed(e.to_string());
            }
        }
    }
    match out.send(&protocol::end_of_file()) {
        Ok(()) => Update::Uploaded(name),
        Err(e) => Update::Failed(e.to_string()),
    }
}

/// Compresses `raw` with zlib when that makes it smaller; sends it raw otherwise.
fn encode_packet(raw: &[u8]) -> Vec<u8> {
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    if z.write_all(raw).is_ok()
        && let Ok(comp) = z.finish()
        && comp.len() < raw.len()
    {
        return protocol::file_packet(true, &comp);
    }
    protocol::file_packet(false, raw)
}

fn offer_timestamp(modified: Option<SystemTime>) -> String {
    let Some(secs) = modified
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
    else {
        return String::new();
    };
    let (y, m, d, hh, mm) = protocol::utc_parts(secs);
    format!(",{m:02}/{d:02}/{y:04} {hh:02}:{mm:02}")
}

impl Download {
    fn write(&mut self, compressed: bool, data: &[u8]) -> io::Result<()> {
        let block = if compressed {
            let mut out = Vec::with_capacity(32 << 10);
            ZlibDecoder::new(data)
                .take(MAX_BLOCK as u64 + 1)
                .read_to_end(&mut out)?;
            if out.len() > MAX_BLOCK {
                return Err(invalid("file block too large"));
            }
            out
        } else {
            data.to_vec()
        };
        self.done += block.len() as u64;
        if self.done > self.total {
            return Err(invalid(format!("{} is larger than announced", self.name)));
        }
        self.file.write_all(&block)
    }

    fn finish(self) -> io::Result<PathBuf> {
        if self.done != self.total {
            return Err(invalid(format!("{} arrived incomplete", self.name)));
        }
        self.file.sync_all()?;
        fs::rename(&self.part, &self.dest)?;
        Ok(self.dest.clone())
    }
}

pub fn downloads_dir() -> PathBuf {
    std::env::home_dir()
        .map(|h| h.join("Downloads"))
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

fn start(remote: &str, total: u64, dir: &Path) -> io::Result<Download> {
    let name = local_name(remote);
    let dest = unique(dir, &name);
    let part = dir.join(format!(".{name}.lucarna-part"));
    let file = File::create(&part)?;
    Ok(Download {
        file,
        part,
        dest,
        name,
        total,
        done: 0,
        reported: Instant::now(),
    })
}

/// Last component of a remote Windows path, made safe as a local file name.
/// A folder is requested as `[ name ]` and arrives as a zip the server made of it, so it is saved as `name.zip`.
// ponytail: the zip is kept as is (Finder and Explorer unzip it); unzip in-app if users ask for it.
pub fn local_name(remote: &str) -> String {
    let base = remote.rsplit(['\\', '/']).next().unwrap_or_default();
    let folder = base
        .strip_prefix("[ ")
        .and_then(|b| b.strip_suffix(" ]"))
        .map(|dir| format!("{dir}.zip"));
    let base = folder.as_deref().unwrap_or(base);
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim_matches([' ', '.']);
    if clean.is_empty() {
        "download".into()
    } else {
        clean.into()
    }
}

/// `name`, or `stem (n).ext` if it already exists in `dir`.
fn unique(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (0..)
        .map(|n| {
            dir.join(if n == 0 {
                name.to_string()
            } else {
                format!("{stem} ({n}){ext}")
            })
        })
        .find(|p| !p.exists())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_names_are_safe() {
        assert_eq!(local_name("C:\\Users\\ana\\informe.pdf"), "informe.pdf");
        assert_eq!(local_name("C:\\x\\.."), "download");
        assert_eq!(local_name("C:\\x\\a:b|c"), "a_b_c");
        assert_eq!(local_name("../../etc/passwd"), "passwd");
        assert_eq!(local_name(""), "download");
        assert_eq!(
            local_name("C:\\Users\\ana\\[ Fotos 2026 ]"),
            "Fotos 2026.zip"
        );
        assert_eq!(
            local_name("C:\\x\\[ a:b ]"),
            "a_b.zip",
            "folder names are sanitized too"
        );
    }

    #[test]
    fn packet_encoding_picks_smaller() {
        let repetitive = vec![b'A'; 1024];
        let pkt = encode_packet(&repetitive);
        assert_eq!(pkt[7], 1, "repetitive block should be zlib-compressed");
        assert!(pkt.len() < repetitive.len());

        let tiny = b"hi";
        let pkt = encode_packet(tiny);
        assert_eq!(pkt[7], 0, "tiny block is smaller uncompressed");
        assert_eq!(&pkt[12..], tiny);
    }

    #[test]
    fn offer_timestamp_formats_utc() {
        let t = UNIX_EPOCH + Duration::from_secs(1_792_245_900); // 2026-10-17 14:05 UTC
        assert_eq!(offer_timestamp(Some(t)), ",10/17/2026 14:05");
        assert_eq!(offer_timestamp(None), "");
    }

    #[test]
    fn listing_and_download() {
        let dir = std::env::temp_dir().join(format!("lucarna-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "old").unwrap();
        assert_eq!(unique(&dir, "a.txt"), dir.join("a (1).txt"));

        let mut d = start("C:\\a.txt", 6, &dir).unwrap();
        d.write(false, b"hel").unwrap();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(b"lo!").unwrap();
        d.write(true, &z.finish().unwrap()).unwrap();
        let dest = d.finish().unwrap();
        assert_eq!(
            (
                dest.file_name().unwrap().to_str().unwrap(),
                fs::read_to_string(&dest).unwrap().as_str()
            ),
            ("a (1).txt", "hello!")
        );
        assert!(!dir.join(".a.txt.lucarna-part").exists());

        let mut d = start("C:\\b.bin", 1, &dir).unwrap();
        assert!(d.write(false, b"xy").is_err(), "more than announced");
        drop(d);
        assert!(
            !dir.join(".b.bin.lucarna-part").exists(),
            "partial file removed"
        );

        let mut r = Receiver::default();
        let mut entry = vec![0; 44];
        entry.extend(b"f.txt");
        assert!(r.dir(A_DIRECTORY, b"C:\\").unwrap().is_none());
        assert!(r.dir(A_DIRECTORY, &entry).unwrap().is_none());
        let Some(Update::Listing { path, entries }) = r.dir(0, &[]).unwrap() else {
            panic!()
        };
        assert_eq!((path.as_str(), entries[0].name.as_str()), ("C:\\", "f.txt"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
