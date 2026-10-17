//! Session-thread side of file transfer: assembles listings and streams downloads to disk.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flate2::read::ZlibDecoder;

use super::protocol::{self, Entry, Msg, A_DIRECTORY, A_DIR_INACCESSIBLE, A_FILE, MAX_BLOCK, UNICODE};
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
    Failed(String),
}

/// UI handle: sends requests and remembers which file was asked for, so a server can't push unrequested files.
pub struct RemoteFiles {
    out: Arc<Outbound>,
    requested: Mutex<Option<String>>,
}

impl RemoteFiles {
    pub fn new(out: Arc<Outbound>) -> Self {
        Self { out, requested: Mutex::new(None) }
    }

    // Send errors surface through the session reader as a Closed event, so they are ignored here.
    pub fn open(&self) {
        let _ = self.out.send(&[protocol::permission_request(), protocol::drives_request()].concat());
    }

    pub fn list(&self, path: &str) {
        let _ = self.out.send(&protocol::dir_request(path));
    }

    pub fn download(&self, path: &str) {
        *self.requested.lock().unwrap() = Some(path.to_string());
        let _ = self.out.send(&protocol::download_request(path));
    }

    pub fn cancel(&self) {
        let _ = self.out.send(&protocol::abort());
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
            Msg::Access(ok) => Some(Update::Access(ok)),
            Msg::Drives(data) => Some(Update::Drives(protocol::parse_drives(&data))),
            Msg::Dir { param, data } => self.dir(param, &data)?,
            Msg::FileHeader { size, low } => {
                let Some(path) = files.requested.lock().unwrap().take() else {
                    files.out.send(&protocol::header_reply(None))?;
                    return Ok(None);
                };
                let Some(total) = size else { return Ok(Some(Update::Failed(format!("the server could not open {path}")))) };
                match start(&path, total, &downloads_dir()) {
                    Ok(d) => {
                        files.out.send(&protocol::header_reply(Some(low)))?;
                        let update = Update::Progress { name: d.name.clone(), done: 0, total };
                        self.download = Some(d);
                        Some(update)
                    }
                    Err(e) => {
                        files.out.send(&protocol::header_reply(None))?;
                        Some(Update::Failed(e.to_string()))
                    }
                }
            }
            Msg::FilePacket { compressed, data } => {
                let Some(d) = &mut self.download else { return Ok(None) };
                match d.write(compressed, &data) {
                    Ok(()) if d.reported.elapsed() < Duration::from_millis(100) => None,
                    Ok(()) => {
                        d.reported = Instant::now();
                        Some(Update::Progress { name: d.name.clone(), done: d.done, total: d.total })
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
            Msg::Abort => self.download.take().map(|d| Update::Failed(format!("{} was aborted", d.name))),
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
                        let path = if unicode { String::from_utf8_lossy(data).into_owned() } else { protocol::ansi(data) };
                        self.listing = Some((path, Vec::new()));
                    }
                    Some((_, entries)) if entries.len() >= MAX_ENTRIES => return Err(invalid("directory listing too long")),
                    Some((_, entries)) => entries.push(protocol::parse_entry(data, unicode)?),
                }
                None
            }
            _ => self.listing.take().map(|(path, entries)| Update::Listing { path, entries }),
        })
    }
}

impl Download {
    fn write(&mut self, compressed: bool, data: &[u8]) -> io::Result<()> {
        let block = if compressed {
            let mut out = Vec::with_capacity(32 << 10);
            ZlibDecoder::new(data).take(MAX_BLOCK as u64 + 1).read_to_end(&mut out)?;
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

fn downloads_dir() -> PathBuf {
    std::env::home_dir().map(|h| h.join("Downloads")).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir)
}

fn start(remote: &str, total: u64, dir: &Path) -> io::Result<Download> {
    let name = local_name(remote);
    let dest = unique(dir, &name);
    let part = dir.join(format!(".{name}.lucarna-part"));
    let file = File::create(&part)?;
    Ok(Download { file, part, dest, name, total, done: 0, reported: Instant::now() })
}

/// Last component of a remote Windows path, made safe as a local file name.
fn local_name(remote: &str) -> String {
    let base = remote.rsplit(['\\', '/']).next().unwrap_or_default();
    let clean: String = base.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let clean = clean.trim_matches([' ', '.']);
    if clean.is_empty() { "download".into() } else { clean.into() }
}

/// `name`, or `stem (n).ext` if it already exists in `dir`.
fn unique(dir: &Path, name: &str) -> PathBuf {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (0..)
        .map(|n| dir.join(if n == 0 { name.to_string() } else { format!("{stem} ({n}){ext}") }))
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
        assert_eq!((dest.file_name().unwrap().to_str().unwrap(), fs::read_to_string(&dest).unwrap().as_str()), ("a (1).txt", "hello!"));
        assert!(!dir.join(".a.txt.lucarna-part").exists());

        let mut d = start("C:\\b.bin", 1, &dir).unwrap();
        assert!(d.write(false, b"xy").is_err(), "more than announced");
        drop(d);
        assert!(!dir.join(".b.bin.lucarna-part").exists(), "partial file removed");

        let mut r = Receiver::default();
        let mut entry = vec![0; 44];
        entry.extend(b"f.txt");
        assert!(r.dir(A_DIRECTORY, b"C:\\").unwrap().is_none());
        assert!(r.dir(A_DIRECTORY, &entry).unwrap().is_none());
        let Some(Update::Listing { path, entries }) = r.dir(0, &[]).unwrap() else { panic!() };
        assert_eq!((path.as_str(), entries[0].name.as_str()), ("C:\\", "f.txt"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
