//! Saved connections: names, addresses, labels and options in a plain file, desktop previews
//! compressed on disk, and passwords only in the OS keychain (Keychain on macOS, Credential
//! Manager on Windows, Secret Service on Linux).

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use super::{ServerAddress, SshTarget};

const SERVICE: &str = "Lucarna";
const PREVIEW_MAGIC: &[u8; 4] = b"LCPV";
const MAX_PREVIEW_DIM: u16 = 640;

#[derive(Clone, Debug, PartialEq)]
pub struct SessionOptions {
    pub view_only: bool,
    pub scale_1_1: bool,
    pub fullscreen: bool,
    pub share_clipboard: bool,
    pub save_preview: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            view_only: false,
            scale_1_1: false,
            fullscreen: false,
            share_clipboard: true,
            save_preview: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedConnection {
    pub name: String,
    pub address: ServerAddress,
    /// Windows user name for MS-Logon servers; empty for plain VNC passwords.
    pub user: String,
    /// UltraVNC repeater to dial instead of `address` (then `address` is `host:port` or `ID:number`); empty for none.
    pub repeater: String,
    /// `user@host[:port]` to tunnel VNC (and files) through SSH; empty for a direct connection.
    pub ssh: String,
    /// User-defined organization labels/groups (e.g. "Cambridge", "London").
    pub labels: Vec<String>,
    pub favorite: bool,
    pub options: SessionOptions,
    /// Unix epoch seconds of the last successful connection (0 = never).
    pub last_connected: u64,
}

impl SavedConnection {
    #[cfg(test)]
    pub fn new(name: impl Into<String>, address: ServerAddress, user: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            address,
            user: user.into(),
            repeater: String::new(),
            ssh: String::new(),
            labels: Vec::new(),
            favorite: false,
            options: SessionOptions::default(),
            last_connected: 0,
        }
    }
}

fn file() -> Option<PathBuf> {
    eframe::storage_dir("Lucarna").map(|d| d.join("connections.txt"))
}

fn previews_dir() -> Option<PathBuf> {
    eframe::storage_dir("Lucarna").map(|d| d.join("previews"))
}

fn preview_path(address: &ServerAddress) -> Option<PathBuf> {
    let safe: String = address
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    previews_dir().map(|d| d.join(format!("{safe}.lcpv")))
}

pub fn load() -> Vec<SavedConnection> {
    file().and_then(|f| fs::read_to_string(f).ok()).map(|t| parse(&t)).unwrap_or_default()
}

pub fn store(list: &[SavedConnection]) -> Result<(), String> {
    let f = file().ok_or_else(|| "could not save connections: no data directory".to_string())?;
    export_to(&f, list)
}

pub fn export_to(path: &Path, list: &[SavedConnection]) -> Result<(), String> {
    let write = || -> io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, render(list))
    };
    write().map_err(|e| format!("could not save connections: {e}"))
}

pub fn import_from(path: &Path) -> Result<Vec<SavedConnection>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    Ok(parse(&text))
}

/// Format per line: `name\taddress\tuser[\tlabels\tflags\tlast_connected\trepeater\tssh]`.
/// Earlier 2-column and 3-column files remain valid.
fn parse(text: &str) -> Vec<SavedConnection> {
    text.lines()
        .filter_map(|l| {
            let mut cols = l.splitn(8, '\t');
            let (name, address) = (cols.next()?, cols.next()?);
            let address = ServerAddress::parse(address)?;
            let user = cols.next().unwrap_or_default().into();
            let labels = parse_labels(cols.next().unwrap_or_default());
            let (favorite, options) = parse_flags(cols.next().unwrap_or_default());
            let last_connected = cols.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            let repeater = cols.next().unwrap_or_default().trim().into();
            let ssh = cols.next().unwrap_or_default().trim().into();
            Some(SavedConnection {
                name: name.into(),
                address,
                user,
                repeater,
                ssh,
                labels,
                favorite,
                options,
                last_connected,
            })
        })
        .collect()
}

fn render(list: &[SavedConnection]) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    list.iter()
        .map(|c| {
            let labels = c.labels.iter().map(|l| clean(l).replace(',', " ")).collect::<Vec<_>>().join(",");
            let flags = render_flags(c.favorite, &c.options);
            if labels.is_empty() && flags.is_empty() && c.last_connected == 0 && c.repeater.is_empty() && c.ssh.is_empty() {
                format!("{}\t{}\t{}\n", clean(&c.name), c.address.as_str(), clean(&c.user))
            } else {
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    clean(&c.name),
                    c.address.as_str(),
                    clean(&c.user),
                    labels,
                    flags,
                    c.last_connected,
                    clean(&c.repeater),
                    clean(&c.ssh)
                )
            }
        })
        .collect()
}

pub fn parse_labels(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let trimmed = part.trim();
        if !trimmed.is_empty() && !out.iter().any(|e: &String| e.eq_ignore_ascii_case(trimmed)) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn parse_flags(raw: &str) -> (bool, SessionOptions) {
    let mut favorite = false;
    let mut opt = SessionOptions::default();
    for f in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match f {
            "fav" => favorite = true,
            "view_only" => opt.view_only = true,
            "scale_1_1" => opt.scale_1_1 = true,
            "fullscreen" => opt.fullscreen = true,
            "no_clipboard" => opt.share_clipboard = false,
            "no_preview" => opt.save_preview = false,
            _ => {}
        }
    }
    (favorite, opt)
}

fn render_flags(favorite: bool, opt: &SessionOptions) -> String {
    let mut flags = Vec::new();
    if favorite {
        flags.push("fav");
    }
    if opt.view_only {
        flags.push("view_only");
    }
    if opt.scale_1_1 {
        flags.push("scale_1_1");
    }
    if opt.fullscreen {
        flags.push("fullscreen");
    }
    if !opt.share_clipboard {
        flags.push("no_clipboard");
    }
    if !opt.save_preview {
        flags.push("no_preview");
    }
    flags.join(",")
}

/// Saves a compressed desktop thumbnail for `address`.
pub fn save_preview(address: &ServerAddress, width: u16, height: u16, rgba: &[u8]) -> Result<(), String> {
    let path = preview_path(address).ok_or_else(|| "no data directory for preview".to_string())?;
    let bytes = encode_preview(width, height, rgba).map_err(|e| format!("preview encode: {e}"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("preview dir: {e}"))?;
    }
    fs::write(path, bytes).map_err(|e| format!("preview write: {e}"))
}

/// Loads the saved desktop thumbnail `(width, height, rgba)` for `address`, if present and valid.
pub fn load_preview(address: &ServerAddress) -> Option<(u16, u16, Vec<u8>)> {
    let bytes = fs::read(preview_path(address)?).ok()?;
    decode_preview(&bytes)
}

pub fn delete_preview(address: &ServerAddress) {
    if let Some(p) = preview_path(address) {
        let _ = fs::remove_file(p);
    }
}

fn encode_preview(width: u16, height: u16, rgba: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(8 + rgba.len() / 2);
    out.extend_from_slice(PREVIEW_MAGIC);
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    let mut z = ZlibEncoder::new(out, Compression::fast());
    z.write_all(rgba)?;
    z.finish()
}

fn decode_preview(bytes: &[u8]) -> Option<(u16, u16, Vec<u8>)> {
    if bytes.len() < 8 || &bytes[..4] != PREVIEW_MAGIC {
        return None;
    }
    let width = u16::from_be_bytes([bytes[4], bytes[5]]);
    let height = u16::from_be_bytes([bytes[6], bytes[7]]);
    if width == 0 || height == 0 || width > MAX_PREVIEW_DIM || height > MAX_PREVIEW_DIM {
        return None;
    }
    let expected = usize::from(width) * usize::from(height) * 4;
    let mut rgba = Vec::with_capacity(expected);
    ZlibDecoder::new(&bytes[8..]).take(expected as u64 + 1).read_to_end(&mut rgba).ok()?;
    (rgba.len() == expected).then_some((width, height, rgba))
}

/// The keychain password for `address`, if one was saved.
pub fn password(address: &ServerAddress) -> Option<String> {
    get(address.as_str())
}

/// Stores the password in the keychain; an empty one removes the entry.
pub fn set_password(address: &ServerAddress, password: &str) -> Result<(), String> {
    set(address.as_str(), password)
}

/// The SSH password for `target`, under its own `ssh:` account so it never mixes with VNC passwords.
pub fn ssh_password(target: &SshTarget) -> Option<String> {
    get(&target.account())
}

pub fn set_ssh_password(target: &SshTarget, password: &str) -> Result<(), String> {
    set(&target.account(), password)
}

fn get(account: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, account).ok()?.get_password().ok()
}

fn set(account: &str, password: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, account).map_err(|e| format!("keychain: {e}"))?;
    match if password.is_empty() {
        entry.delete_credential()
    } else {
        entry.set_password(password)
    } {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keychain: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip() {
        let list = vec![
            SavedConnection::new("Office\tPC", ServerAddress::parse("10.0.0.5:1").unwrap(), "CORP\\ana"),
            SavedConnection::new("Lab", ServerAddress::parse("lab::6000").unwrap(), ""),
        ];
        let text = render(&list);
        assert_eq!(text, "Office PC\t10.0.0.5:5901\tCORP\\ana\nLab\tlab:6000\t\n");
        // Files written before the user or metadata columns existed still load.
        let old = "Old\t10.0.0.9:5900\n";
        assert_eq!(
            parse(&format!("{text}garbage line\n{old}")),
            [
                SavedConnection::new("Office PC", list[0].address.clone(), "CORP\\ana"),
                list[1].clone(),
                SavedConnection::new("Old", ServerAddress::parse("10.0.0.9").unwrap(), ""),
            ]
        );
    }

    #[test]
    fn repeater_round_trip() {
        let mut c = SavedConnection::new("Behind NAT", ServerAddress::parse("ID:1234").unwrap(), "");
        c.repeater = "repeater.example.com:5901".into();
        let text = render(std::slice::from_ref(&c));
        assert_eq!(text, "Behind NAT\tID:1234\t\t\t\t0\trepeater.example.com:5901\t\n");
        assert_eq!(parse(&text), [c]);
    }

    #[test]
    fn ssh_round_trip() {
        let mut c = SavedConnection::new("Office", ServerAddress::parse("localhost").unwrap(), "");
        c.ssh = "ana@office.example.com:2222".into();
        let text = render(std::slice::from_ref(&c));
        assert_eq!(text, "Office\tlocalhost:5900\t\t\t\t0\t\tana@office.example.com:2222\n");
        assert_eq!(parse(&text), [c]);
        // A line written before the ssh column existed still loads, without SSH.
        assert_eq!(parse("Old\tpc:5900\t\t\t\t0\trep:5901\n")[0].ssh, "");
    }

    #[test]
    fn metadata_and_options_round_trip() {
        let mut c = SavedConnection::new("Prod DB", ServerAddress::parse("10.0.1.8").unwrap(), "admin");
        c.labels = vec!["London".into(), "Production".into()];
        c.favorite = true;
        c.options = SessionOptions {
            view_only: true,
            scale_1_1: true,
            fullscreen: false,
            share_clipboard: false,
            save_preview: false,
        };
        c.last_connected = 1_792_300_000;
        let text = render(&[c.clone()]);
        assert_eq!(parse(&text), [c]);
    }

    #[test]
    fn preview_round_trip() {
        let rgba: Vec<u8> = (0..16 * 10 * 4).map(|i| (i & 0xff) as u8).collect();
        let encoded = encode_preview(16, 10, &rgba).unwrap();
        let (w, h, decoded) = decode_preview(&encoded).unwrap();
        assert_eq!((w, h, decoded), (16, 10, rgba));
        assert!(decode_preview(b"bad").is_none());
    }

    #[test]
    #[ignore = "touches the OS keychain"]
    fn keychain_round_trip() {
        let address = ServerAddress::parse("lucarna-test.invalid").unwrap();
        set_password(&address, "s3cret").unwrap();
        assert_eq!(password(&address).as_deref(), Some("s3cret"));
        set_password(&address, "").unwrap();
        assert_eq!(password(&address), None);
    }
}
