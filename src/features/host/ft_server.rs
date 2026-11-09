//! Native RFB Message Type 7 (UltraVNC File Transfer) Server in pure Rust (`std::fs`).
//! Provides virtual drive mapping (`H:`, `M:`, `D:`, `V:`) and strict path-canonicalization sandboxing
//! without requiring SSH/sshd or elevated OS privileges.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use crate::features::file_transfer::{
    A_DIR_CREATE, A_DIR_INACCESSIBLE, A_DIRECTORY, A_FILE, A_FILE_DELETE, A_FILE_RENAME, MAX_BLOCK,
    UNICODE,
};
use crate::shared::wire::{invalid, read_bytes, read_u8, read_u16, read_u32};

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
const A_DRIVES_LIST: u16 = 3;

const C_DIR_CREATE: u16 = 1;
const C_FILE_DELETE: u16 = 4;
const C_FILE_RENAME: u16 = 5;

const SERVER_BLOCK_SIZE: usize = 32 << 10;
const MAX_DATA: usize = MAX_BLOCK + 1024;
const ERROR: u32 = 0xFFFF_FFFF;
const FILETIME_UNIX_DIFF_SECS: u64 = 11_644_473_600;

/// Sensitive filenames or directories that can never be listed, read, or overwritten remotely.
const BLOCKED_NAMES: &[&str] = &[
    "authorized_keys",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "known_hosts",
    "connections.txt",
    "launchagents",
    "launchdaemons",
    "startup",
    "system32",
];

/// Folder created in the home directory for files the person sharing wants to hand over.
pub const SHARE_FOLDER: &str = "Lucarna Share";

/// Maps single-letter virtual drives (`H:`, `M:`, `D:`, `V:`) to sandboxed host directories.
#[derive(Clone, Debug)]
pub struct VirtualDrives {
    roots: Vec<(char, PathBuf)>,
}

impl Default for VirtualDrives {
    fn default() -> Self {
        Self::discover_user_drives()
    }
}

impl VirtualDrives {
    /// Creates a `VirtualDrives` map from explicit `(letter, path)` pairs, canonicalizing existing roots.
    pub fn with_roots(roots: Vec<(char, PathBuf)>) -> Self {
        let mut clean = Vec::new();
        for (letter, path) in roots {
            let upper = letter.to_ascii_uppercase();
            if upper.is_ascii_alphabetic()
                && let Ok(canon) = fs::canonicalize(&path)
                && canon.is_dir()
            {
                clean.push((upper, canon));
            }
        }
        Self { roots: clean }
    }

    /// Discovers standard user-space studio folders (`H:` Home, `M:` Music, `D:` Downloads, `V:` Volumes).
    pub fn discover_user_drives() -> Self {
        // Two folders and nothing else: the person sharing can read what is in them, and the viewer can
        // only write there. The home folder is never shared: a file dropped in `~/Library/LaunchAgents`
        // or `~/Desktop` would run or be opened later without anyone asking.
        let mut candidates = Vec::new();
        if let Some(home) = std::env::home_dir() {
            let downloads = home.join("Downloads");
            if downloads.is_dir() {
                candidates.push(('D', downloads));
            }
            let share = home.join(SHARE_FOLDER);
            if std::fs::create_dir_all(&share).is_ok() {
                candidates.push(('S', share));
            }
        }
        Self::with_roots(candidates)
    }

    pub fn roots(&self) -> &[(char, PathBuf)] {
        &self.roots
    }

    /// Formats available drives as 4-byte UltraVNC chunks (`"H:l\0M:l\0..."`).
    pub fn drives_wire_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.roots.len() * 4);
        for &(letter, _) in &self.roots {
            out.extend_from_slice(&[letter as u8, b':', b'l', 0]);
        }
        out
    }

    /// Resolves an existing virtual path (e.g. `"M:\\Jingles\\intro.mp3"`) to a canonical host path
    /// guaranteed to reside strictly inside the mapped drive's canonical root.
    pub fn resolve_existing(&self, virtual_path: &str) -> io::Result<(char, PathBuf, PathBuf)> {
        let (drive, root, segments) = self.parse_virtual_path(virtual_path)?;
        let mut candidate = root.clone();
        for seg in segments {
            candidate.push(seg);
        }
        let canon = fs::canonicalize(&candidate)?;
        if !canon.starts_with(&root) {
            return Err(invalid("path escapes virtual drive sandbox via symlink"));
        }
        Ok((drive, root, canon))
    }

    /// Resolves the parent directory and validates the leaf name for creating a new file or folder.
    pub fn resolve_new_child(&self, virtual_path: &str) -> io::Result<PathBuf> {
        let (_drive, root, mut segments) = self.parse_virtual_path(virtual_path)?;
        let Some(leaf) = segments.pop() else {
            return Err(invalid("cannot overwrite virtual drive root"));
        };
        validate_segment(leaf)?;

        let mut parent = root.clone();
        for seg in segments {
            parent.push(seg);
        }
        let canon_parent = fs::canonicalize(&parent)?;
        if !canon_parent.starts_with(&root) {
            return Err(invalid("parent directory escapes virtual drive sandbox"));
        }
        let target = canon_parent.join(leaf);
        // Refuse to overwrite an existing symlink.
        if let Ok(meta) = fs::symlink_metadata(&target)
            && meta.file_type().is_symlink()
        {
            return Err(invalid("refusing to write through a symlink"));
        }
        Ok(target)
    }

    fn parse_virtual_path<'a>(
        &self,
        virtual_path: &'a str,
    ) -> io::Result<(char, PathBuf, Vec<&'a str>)> {
        let trimmed = virtual_path.trim();
        let bytes = trimmed.as_bytes();
        if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
            return Err(invalid(
                "virtual path must start with a drive letter (e.g. M:\\)",
            ));
        }
        let drive = (bytes[0] as char).to_ascii_uppercase();
        let Some((_, root)) = self.roots.iter().find(|(d, _)| *d == drive) else {
            return Err(invalid(format!("unknown virtual drive {drive}:")));
        };

        let rest = &trimmed[2..];
        let mut segments = Vec::new();
        for seg in rest.split(['\\', '/']).filter(|s| !s.is_empty()) {
            validate_segment(seg)?;
            segments.push(seg);
        }
        Ok((drive, root.clone(), segments))
    }
}

fn validate_segment(seg: &str) -> io::Result<()> {
    if seg.is_empty() || seg == "." || seg == ".." {
        return Err(invalid("relative path traversal is forbidden"));
    }
    if seg.starts_with('.') {
        return Err(invalid("hidden files and dotfiles are blocked"));
    }
    if seg
        .chars()
        .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return Err(invalid("illegal characters in path segment"));
    }
    let lower = seg.to_ascii_lowercase();
    if BLOCKED_NAMES.contains(&lower.as_str()) {
        return Err(invalid(
            "access to sensitive system or credential file blocked",
        ));
    }
    for comp in Path::new(seg).components() {
        if !matches!(comp, Component::Normal(_)) {
            return Err(invalid("non-normal path component rejected"));
        }
    }
    Ok(())
}

struct PendingDownload {
    file: File,
    total: u64,
    compress: bool,
}

struct ActiveUpload {
    file: File,
    part_path: PathBuf,
    dest_path: PathBuf,
    total: u64,
    done: u64,
}

impl Drop for ActiveUpload {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.part_path);
    }
}

/// Server-side state machine for RFB Message Type 7 (UltraVNC File Transfer).
pub struct FtServer {
    pub drives: VirtualDrives,
    pub read_only: bool,
    pending_download: Option<PendingDownload>,
    active_upload: Option<ActiveUpload>,
}

impl FtServer {
    pub fn new(drives: VirtualDrives, read_only: bool) -> Self {
        Self {
            drives,
            read_only,
            pending_download: None,
            active_upload: None,
        }
    }

    /// Announces FT Protocol v4 with 32 KiB blocks when the viewer advertises `ENC_PROTOCOL_VERSION`.
    pub fn send_protocol_version(&self, s: &mut impl Write) -> io::Result<()> {
        s.write_all(&wire_msg(
            PROTOCOL_VERSION,
            4,
            SERVER_BLOCK_SIZE as u32,
            &[],
        ))?;
        s.flush()
    }

    /// Reads and handles one RFB message type 7 body (after the leading `7` byte).
    pub fn handle_message(&mut self, s: &mut (impl Read + Write)) -> io::Result<()> {
        let content_type = read_u8(s)?;
        let param = read_u16(s)?.swap_bytes(); // little-endian on wire
        let size = read_u32(s)?;
        let length = read_u32(s)? as usize;
        if length > MAX_DATA {
            return Err(invalid("FT client message exceeds MAX_DATA"));
        }
        let data = read_bytes(s, length)?;

        match content_type {
            ABORT if param != 0 => {
                // Official viewer permission probe (`permission_request`).
                s.write_all(&wire_msg(ACCESS, 0, 1, &[]))?;
                s.flush()
            }
            ABORT => {
                self.pending_download = None;
                self.active_upload = None;
                Ok(())
            }
            DIR_CONTENT_REQUEST => match param & !UNICODE {
                R_DRIVES_LIST => {
                    let payload = self.drives.drives_wire_payload();
                    s.write_all(&wire_msg(DIR_PACKET, A_DRIVES_LIST, 0, &payload))?;
                    s.flush()
                }
                R_DIR_CONTENT => {
                    let req_path = String::from_utf8_lossy(&data).into_owned();
                    self.send_dir_listing(s, &req_path)
                }
                _ => Ok(()),
            },
            FILE_TRANSFER_REQUEST => {
                let req_path = String::from_utf8_lossy(&data).into_owned();
                let compress = size != 0;
                self.handle_download_request(s, &req_path, &data, compress)
            }
            FILE_HEADER => {
                if size == ERROR {
                    self.pending_download = None;
                    return Ok(());
                }
                self.stream_pending_download(s)
            }
            FILE_TRANSFER_OFFER => {
                // High 32 bits of the 64-bit file size follow `data` outside `length`.
                let high = read_u32(s)?;
                let total = (u64::from(high) << 32) | u64::from(size);
                let spec = String::from_utf8_lossy(&data).into_owned();
                self.handle_upload_offer(s, &spec, total)
            }
            FILE_PACKET => self.handle_upload_packet(s, size == 1, &data),
            END_OF_FILE => self.finish_upload(),
            COMMAND => {
                let spec = String::from_utf8_lossy(&data).into_owned();
                self.handle_command(s, param & !UNICODE, &spec, &data)
            }
            _ => Ok(()),
        }
    }

    fn send_dir_listing(&self, s: &mut impl Write, req_path: &str) -> io::Result<()> {
        let Ok((drive, root, canon_dir)) = self.drives.resolve_existing(req_path) else {
            s.write_all(&wire_msg(DIR_PACKET, A_DIR_INACCESSIBLE | UNICODE, 0, &[]))?;
            return s.flush();
        };
        let Ok(read_dir) = fs::read_dir(&canon_dir) else {
            s.write_all(&wire_msg(DIR_PACKET, A_DIR_INACCESSIBLE | UNICODE, 0, &[]))?;
            return s.flush();
        };

        let normalized = format_virtual_dir(drive, &root, &canon_dir);
        let mut out = wire_msg(DIR_PACKET, A_DIRECTORY | UNICODE, 0, normalized.as_bytes());

        let mut entries = Vec::new();
        for item in read_dir.flatten() {
            let Ok(name) = item.file_name().into_string() else {
                continue;
            };
            if validate_segment(&name).is_err() {
                continue;
            }
            // Skip symlinks or entries escaping the canonical root.
            let Ok(sym_meta) = item.metadata() else {
                continue;
            };
            let Ok(canon_entry) = fs::canonicalize(item.path()) else {
                continue;
            };
            if !canon_entry.starts_with(&root) {
                continue;
            }
            entries.push((name, sym_meta));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        for (name, meta) in entries {
            let is_dir = meta.is_dir();
            let size = if is_dir { 0 } else { meta.len() };
            let modified = to_filetime(meta.modified().ok());
            let entry_bytes = encode_win32_find_data_w(&name, is_dir, size, modified);
            let kind = if is_dir { A_DIRECTORY } else { A_FILE };
            out.extend_from_slice(&wire_msg(DIR_PACKET, kind | UNICODE, 0, &entry_bytes));
        }

        // End-of-listing marker: param & !UNICODE == 0.
        out.extend_from_slice(&wire_msg(DIR_PACKET, UNICODE, 0, &[]));
        s.write_all(&out)?;
        s.flush()
    }

    fn handle_download_request(
        &mut self,
        s: &mut impl Write,
        req_path: &str,
        raw_path: &[u8],
        compress: bool,
    ) -> io::Result<()> {
        let opened = self
            .drives
            .resolve_existing(req_path)
            .and_then(|(_, _, path)| {
                let file = File::open(&path)?;
                let meta = file.metadata()?;
                if !meta.is_file() {
                    return Err(invalid("not a regular file"));
                }
                Ok((file, meta.len()))
            });

        match opened {
            Ok((file, total)) => {
                self.pending_download = Some(PendingDownload {
                    file,
                    total,
                    compress,
                });
                let mut m = wire_msg(FILE_HEADER, 0, total as u32, raw_path);
                m.extend_from_slice(&((total >> 32) as u32).to_be_bytes());
                s.write_all(&m)?;
                s.flush()
            }
            Err(_) => {
                self.pending_download = None;
                let mut m = wire_msg(FILE_HEADER, 0, ERROR, raw_path);
                m.extend_from_slice(&ERROR.to_be_bytes());
                s.write_all(&m)?;
                s.flush()
            }
        }
    }

    fn stream_pending_download(&mut self, s: &mut impl Write) -> io::Result<()> {
        let Some(dl) = self.pending_download.take() else {
            return Ok(());
        };
        let mut reader = dl.file.take(dl.total);
        let mut buf = vec![0u8; SERVER_BLOCK_SIZE];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let chunk = &buf[..n];
            let pkt = if dl.compress {
                encode_compressed_or_raw(chunk)
            } else {
                wire_msg(FILE_PACKET, 0, 0, chunk)
            };
            s.write_all(&pkt)?;
        }
        s.write_all(&wire_msg(END_OF_FILE, 0, 0, &[]))?;
        s.flush()
    }

    fn handle_upload_offer(
        &mut self,
        s: &mut impl Write,
        spec: &str,
        total: u64,
    ) -> io::Result<()> {
        let remote_path = strip_offer_timestamp(spec);
        if self.read_only {
            s.write_all(&wire_msg(
                FILE_ACCEPT_HEADER,
                0,
                ERROR,
                remote_path.as_bytes(),
            ))?;
            return s.flush();
        }

        let prep = self.drives.resolve_new_child(remote_path).and_then(|dest| {
            let file_name = dest
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| invalid("invalid destination filename"))?;
            let part = dest.with_file_name(format!(".{file_name}.lucarna-part"));
            let file = File::create(&part)?;
            Ok((file, part, dest))
        });

        match prep {
            Ok((file, part_path, dest_path)) => {
                self.active_upload = Some(ActiveUpload {
                    file,
                    part_path,
                    dest_path,
                    total,
                    done: 0,
                });
                s.write_all(&wire_msg(FILE_ACCEPT_HEADER, 0, 0, remote_path.as_bytes()))?;
                s.flush()
            }
            Err(_) => {
                self.active_upload = None;
                s.write_all(&wire_msg(
                    FILE_ACCEPT_HEADER,
                    0,
                    ERROR,
                    remote_path.as_bytes(),
                ))?;
                s.flush()
            }
        }
    }

    fn handle_upload_packet(
        &mut self,
        s: &mut impl Write,
        compressed: bool,
        data: &[u8],
    ) -> io::Result<()> {
        let Some(up) = &mut self.active_upload else {
            return Ok(());
        };
        let block = if compressed {
            let mut inflated = Vec::with_capacity(SERVER_BLOCK_SIZE);
            ZlibDecoder::new(data)
                .take(MAX_BLOCK as u64 + 1)
                .read_to_end(&mut inflated)?;
            if inflated.len() > MAX_BLOCK {
                self.active_upload = None;
                s.write_all(&wire_msg(ABORT, 0, 0, &[]))?;
                s.flush()?;
                return Err(invalid("decompressed upload block exceeds MAX_BLOCK"));
            }
            inflated
        } else {
            data.to_vec()
        };

        up.done += block.len() as u64;
        if up.done > up.total {
            self.active_upload = None;
            s.write_all(&wire_msg(ABORT, 0, 0, &[]))?;
            s.flush()?;
            return Err(invalid("upload exceeded announced file size"));
        }
        up.file.write_all(&block)
    }

    fn finish_upload(&mut self) -> io::Result<()> {
        let Some(up) = self.active_upload.take() else {
            return Ok(());
        };
        if up.done != up.total {
            return Err(invalid("upload ended before reaching announced size"));
        }
        up.file.sync_all()?;
        fs::rename(&up.part_path, &up.dest_path)
    }

    fn handle_command(
        &mut self,
        s: &mut impl Write,
        cmd: u16,
        spec: &str,
        raw: &[u8],
    ) -> io::Result<()> {
        let (reply_param, result) = match cmd {
            C_DIR_CREATE => {
                let res = if self.read_only {
                    Err(invalid("read-only mode"))
                } else {
                    self.drives.resolve_new_child(spec).and_then(fs::create_dir)
                };
                (A_DIR_CREATE, res)
            }
            C_FILE_DELETE => {
                let res = if self.read_only {
                    Err(invalid("read-only mode"))
                } else {
                    self.drives
                        .resolve_existing(spec)
                        .and_then(|(_, root, target)| {
                            if target == root {
                                return Err(invalid("cannot delete virtual drive root"));
                            }
                            if target.is_dir() {
                                fs::remove_dir(target)
                            } else {
                                fs::remove_file(target)
                            }
                        })
                };
                (A_FILE_DELETE, res)
            }
            C_FILE_RENAME => {
                let res = if self.read_only {
                    Err(invalid("read-only mode"))
                } else if let Some((old_s, new_s)) = spec.split_once('*') {
                    self.drives
                        .resolve_existing(old_s)
                        .and_then(|(_, root, old_p)| {
                            if old_p == root {
                                return Err(invalid("cannot rename virtual drive root"));
                            }
                            let new_p = self.drives.resolve_new_child(new_s)?;
                            fs::rename(old_p, new_p)
                        })
                } else {
                    Err(invalid("missing '*' separator in rename command"))
                };
                (A_FILE_RENAME, res)
            }
            _ => return Ok(()),
        };

        let status = if result.is_ok() { 0 } else { ERROR };
        s.write_all(&wire_msg(COMMAND_RETURN, reply_param, status, raw))?;
        s.flush()
    }
}

fn wire_msg(content_type: u8, param: u16, size: u32, data: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(12 + data.len());
    m.push(7);
    m.push(content_type);
    m.extend_from_slice(&param.to_le_bytes());
    m.extend_from_slice(&size.to_be_bytes());
    m.extend_from_slice(&(data.len() as u32).to_be_bytes());
    m.extend_from_slice(data);
    m
}

fn encode_compressed_or_raw(raw: &[u8]) -> Vec<u8> {
    let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
    if z.write_all(raw).is_ok()
        && let Ok(comp) = z.finish()
        && comp.len() < raw.len()
    {
        return wire_msg(FILE_PACKET, 0, 1, &comp);
    }
    wire_msg(FILE_PACKET, 0, 0, raw)
}

fn format_virtual_dir(drive: char, root: &Path, target: &Path) -> String {
    let mut out = format!("{drive}:\\");
    if let Ok(rel) = target.strip_prefix(root) {
        for comp in rel.components() {
            if let Component::Normal(os) = comp
                && let Some(s) = os.to_str()
            {
                out.push_str(s);
                out.push('\\');
            }
        }
    }
    out
}

/// Strips the trailing `,MM/DD/YYYY HH:MM` timestamp appended by `RemoteFiles::upload`.
fn strip_offer_timestamp(spec: &str) -> &str {
    if let Some((path, suffix)) = spec.rsplit_once(',')
        && suffix.len() == 16
        && suffix.as_bytes()[2] == b'/'
        && suffix.as_bytes()[5] == b'/'
    {
        return path;
    }
    spec
}

fn to_filetime(modified: Option<SystemTime>) -> u64 {
    let unix_secs = modified
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    unix_secs
        .saturating_add(FILETIME_UNIX_DIFF_SECS)
        .saturating_mul(10_000_000)
}

/// Encodes an entry in the 44-byte `WIN32_FIND_DATAW` prefix + UTF-16LE NUL-terminated filename format
/// expected by `file_transfer::protocol::parse_entry(data, true)`.
pub fn encode_win32_find_data_w(name: &str, is_dir: bool, size: u64, modified: u64) -> Vec<u8> {
    let mut buf = vec![0u8; 44];
    let attrs: u32 = if is_dir { 0x10 } else { 0x80 };
    buf[0..4].copy_from_slice(&attrs.to_le_bytes());
    buf[20..24].copy_from_slice(&(modified as u32).to_le_bytes());
    buf[24..28].copy_from_slice(&((modified >> 32) as u32).to_le_bytes());
    buf[28..32].copy_from_slice(&((size >> 32) as u32).to_le_bytes());
    buf[32..36].copy_from_slice(&(size as u32).to_le_bytes());
    for u in name.encode_utf16() {
        buf.extend_from_slice(&u.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::file_transfer::Msg;

    fn temp_sandbox(tag: &str) -> (PathBuf, VirtualDrives) {
        let base =
            std::env::temp_dir().join(format!("lucarna-ft-server-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let music = base.join("Music");
        let home = base.join("Home");
        fs::create_dir_all(&music).unwrap();
        fs::create_dir_all(&home).unwrap();
        let drives = VirtualDrives::with_roots(vec![('M', music), ('H', home)]);
        (base, drives)
    }

    #[test]
    fn virtual_drives_block_traversal_dotfiles_and_symlinks() {
        let (base, drives) = temp_sandbox("security");
        let outside = base.join("secret.txt");
        fs::write(&outside, "top-secret").unwrap();

        // 1. Relative traversal `..` is rejected.
        assert!(drives.resolve_existing("M:\\..\\secret.txt").is_err());
        assert!(drives.resolve_new_child("M:\\..\\pwn.txt").is_err());

        // 2. Hidden dotfiles (`.ssh`, `.bashrc`) and credential files (`authorized_keys`, `id_ed25519`) are blocked.
        assert!(
            drives
                .resolve_new_child("H:\\.ssh\\authorized_keys")
                .is_err()
        );
        assert!(drives.resolve_new_child("H:\\id_ed25519").is_err());
        assert!(drives.resolve_new_child("H:\\connections.txt").is_err());

        // 3. Symlink pointing outside the drive root is caught by `std::fs::canonicalize`.
        #[cfg(unix)]
        {
            let link_path = base.join("Music").join("escape_link.txt");
            std::os::unix::fs::symlink(&outside, &link_path).unwrap();
            assert!(
                drives.resolve_existing("M:\\escape_link.txt").is_err(),
                "symlink escaping sandbox root must be rejected"
            );
        }

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn ft_server_handles_drives_listing_upload_download_and_commands() {
        let (base, drives) = temp_sandbox("roundtrip");
        let mut server = FtServer::new(drives, false);

        // 1. Permission probe + Drives request.
        let mut req = Vec::new();
        req.extend_from_slice(&wire_msg(ABORT, 3, 0, &[])[1..]);
        req.extend_from_slice(&wire_msg(DIR_CONTENT_REQUEST, R_DRIVES_LIST, 0, &[])[1..]);

        let mut io_buf = crate::shared::wire::Duplex::new(req);
        server.handle_message(&mut io_buf).unwrap();
        server.handle_message(&mut io_buf).unwrap();

        let mut resp = &io_buf.output[..];
        assert_eq!(read_u8(&mut resp).unwrap(), 7);
        assert_eq!(
            crate::features::file_transfer::read(&mut resp).unwrap(),
            Msg::Access(true)
        );
        assert_eq!(read_u8(&mut resp).unwrap(), 7);
        assert_eq!(
            crate::features::file_transfer::read(&mut resp).unwrap(),
            Msg::Drives(b"M:l\0H:l\0".to_vec())
        );

        // 2. Upload a radio jingle `M:\cuna_apertura.wav` (with timestamp suffix).
        let audio_payload = b"RIFF-WAVE-STUDIO-AUDIO-PAYLOAD-1234567890".repeat(40);
        let total = audio_payload.len() as u64;
        let mut up_stream = Vec::new();
        let spec = b"M:\\cuna_apertura.wav,11/08/2026 20:30";
        up_stream.extend_from_slice(&wire_msg(FILE_TRANSFER_OFFER, 0, total as u32, spec)[1..]);
        up_stream.extend_from_slice(&0u32.to_be_bytes()); // sizeHigh
        let pkt = encode_compressed_or_raw(&audio_payload);
        up_stream.extend_from_slice(&pkt[1..]);
        up_stream.extend_from_slice(&wire_msg(END_OF_FILE, 0, 0, &[])[1..]);

        let mut up_io = crate::shared::wire::Duplex::new(up_stream);
        server.handle_message(&mut up_io).unwrap(); // Offer
        server.handle_message(&mut up_io).unwrap(); // Packet
        server.handle_message(&mut up_io).unwrap(); // EOF

        let mut up_resp = &up_io.output[..];
        assert_eq!(read_u8(&mut up_resp).unwrap(), 7);
        assert_eq!(
            crate::features::file_transfer::read(&mut up_resp).unwrap(),
            Msg::FileAccept(true)
        );
        assert_eq!(
            fs::read(base.join("Music").join("cuna_apertura.wav")).unwrap(),
            audio_payload
        );

        // 3. Create subfolder `M:\Jingles`, rename `cuna_apertura.wav` into `M:\Jingles\intro.wav`, and list `M:\Jingles\`.
        let mut cmd_stream = Vec::new();
        cmd_stream.extend_from_slice(&wire_msg(COMMAND, C_DIR_CREATE, 0, b"M:\\Jingles")[1..]);
        cmd_stream.extend_from_slice(
            &wire_msg(
                COMMAND,
                C_FILE_RENAME,
                0,
                b"M:\\cuna_apertura.wav*M:\\Jingles\\intro.wav",
            )[1..],
        );
        cmd_stream.extend_from_slice(
            &wire_msg(
                DIR_CONTENT_REQUEST,
                R_DIR_CONTENT | UNICODE,
                0,
                b"M:\\Jingles\\",
            )[1..],
        );

        let mut cmd_io = crate::shared::wire::Duplex::new(cmd_stream);
        server.handle_message(&mut cmd_io).unwrap();
        server.handle_message(&mut cmd_io).unwrap();
        server.handle_message(&mut cmd_io).unwrap();

        // 4. Download `M:\Jingles\intro.wav` back and verify contents.
        let mut dl_stream = Vec::new();
        dl_stream.extend_from_slice(
            &wire_msg(FILE_TRANSFER_REQUEST, 0, 1, b"M:\\Jingles\\intro.wav")[1..],
        );
        dl_stream.extend_from_slice(&wire_msg(FILE_HEADER, 0, total as u32, &[])[1..]);

        let mut dl_io = crate::shared::wire::Duplex::new(dl_stream);
        server.handle_message(&mut dl_io).unwrap(); // Request -> sends FileHeader
        server.handle_message(&mut dl_io).unwrap(); // Header reply -> streams packets + EOF

        let mut dl_resp = &dl_io.output[..];
        assert_eq!(read_u8(&mut dl_resp).unwrap(), 7);
        assert_eq!(
            crate::features::file_transfer::read(&mut dl_resp).unwrap(),
            Msg::FileHeader {
                size: Some(total),
                low: total as u32
            }
        );
        assert_eq!(read_u8(&mut dl_resp).unwrap(), 7);
        let Msg::FilePacket { compressed, data } =
            crate::features::file_transfer::read(&mut dl_resp).unwrap()
        else {
            panic!("expected FilePacket");
        };
        let mut decoded = Vec::new();
        if compressed {
            ZlibDecoder::new(&data[..])
                .read_to_end(&mut decoded)
                .unwrap();
        } else {
            decoded = data;
        }
        assert_eq!(decoded, audio_payload);
        assert_eq!(read_u8(&mut dl_resp).unwrap(), 7);
        assert_eq!(
            crate::features::file_transfer::read(&mut dl_resp).unwrap(),
            Msg::EndOfFile
        );

        let _ = fs::remove_dir_all(&base);
    }
}
