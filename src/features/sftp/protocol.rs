//! SFTP v3 wire protocol (draft-ietf-secsh-filexfer-02).
//! Pure Rust serialization and deserialization with strict bounds checking.

#![allow(dead_code)]

use std::io::{self, Read};

use crate::features::file_transfer::Entry;
use crate::shared::wire::{invalid, read_bytes, read_string, read_u8, read_u32, read_u64};

pub const SFTP_VERSION: u32 = 3;

/// Maximum packet payload size we accept to prevent memory exhaustion (256 KiB).
pub const MAX_PACKET_LEN: u32 = 256 * 1024;
/// Maximum string length accepted on wire.
pub const MAX_STRING_LEN: usize = 64 * 1024;
/// Maximum entries in a single directory listing response.
pub const MAX_ENTRIES: usize = 100_000;

// Packet Types
pub const SSH_FXP_INIT: u8 = 1;
pub const SSH_FXP_VERSION: u8 = 2;
pub const SSH_FXP_OPEN: u8 = 3;
pub const SSH_FXP_CLOSE: u8 = 4;
pub const SSH_FXP_READ: u8 = 5;
pub const SSH_FXP_WRITE: u8 = 6;
pub const SSH_FXP_LSTAT: u8 = 7;
pub const SSH_FXP_FSTAT: u8 = 8;
pub const SSH_FXP_OPENDIR: u8 = 11;
pub const SSH_FXP_READDIR: u8 = 12;
pub const SSH_FXP_REMOVE: u8 = 13;
pub const SSH_FXP_MKDIR: u8 = 14;
pub const SSH_FXP_RMDIR: u8 = 15;
pub const SSH_FXP_REALPATH: u8 = 16;
pub const SSH_FXP_STAT: u8 = 17;
pub const SSH_FXP_RENAME: u8 = 18;
pub const SSH_FXP_STATUS: u8 = 101;
pub const SSH_FXP_HANDLE: u8 = 102;
pub const SSH_FXP_DATA: u8 = 103;
pub const SSH_FXP_NAME: u8 = 104;
pub const SSH_FXP_ATTRS: u8 = 105;

// Open flags (pflags)
pub const SSH_FXF_READ: u32 = 0x00000001;
pub const SSH_FXF_WRITE: u32 = 0x00000002;
pub const SSH_FXF_APPEND: u32 = 0x00000004;
pub const SSH_FXF_CREAT: u32 = 0x00000008;
pub const SSH_FXF_TRUNC: u32 = 0x00000010;
pub const SSH_FXF_EXCL: u32 = 0x00000020;

// Attribute flags
pub const SSH_FILEXFER_ATTR_SIZE: u32 = 0x00000001;
pub const SSH_FILEXFER_ATTR_UIDGID: u32 = 0x00000002;
pub const SSH_FILEXFER_ATTR_PERMISSIONS: u32 = 0x00000004;
pub const SSH_FILEXFER_ATTR_ACMODTIME: u32 = 0x00000008;
pub const SSH_FILEXFER_ATTR_EXTENDED: u32 = 0x80000000;

// Status codes
pub const SSH_FX_OK: u32 = 0;
pub const SSH_FX_EOF: u32 = 1;
pub const SSH_FX_NO_SUCH_FILE: u32 = 2;
pub const SSH_FX_PERMISSION_DENIED: u32 = 3;
pub const SSH_FX_FAILURE: u32 = 4;
pub const SSH_FX_BAD_MESSAGE: u32 = 5;
pub const SSH_FX_NO_CONNECTION: u32 = 6;
pub const SSH_FX_CONNECTION_LOST: u32 = 7;
pub const SSH_FX_OP_UNSUPPORTED: u32 = 8;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileAttrs {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub permissions: Option<u32>,
    pub atime: Option<u32>,
    pub mtime: Option<u32>,
}

impl FileAttrs {
    pub fn is_dir(&self, longname: &str) -> bool {
        if let Some(perm) = self.permissions {
            (perm & 0o170000) == 0o040000
        } else {
            longname.starts_with('d')
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameItem {
    pub filename: String,
    pub longname: String,
    pub attrs: FileAttrs,
}

impl NameItem {
    pub fn into_entry(self) -> Entry {
        let is_dir = self.attrs.is_dir(&self.longname);
        Entry {
            name: self.filename,
            size: self.attrs.size.unwrap_or(0),
            modified: self.attrs.mtime.map(|m| m as u64).unwrap_or(0),
            is_dir,
            inaccessible: false,
        }
    }
}

#[derive(Debug)]
pub enum Response {
    Version { version: u32 },
    Status { id: u32, code: u32, message: String },
    Handle { id: u32, handle: Vec<u8> },
    Data { id: u32, data: Vec<u8> },
    Name { id: u32, items: Vec<NameItem> },
    Attrs { id: u32, attrs: FileAttrs },
}

impl Response {
    pub fn id(&self) -> Option<u32> {
        match self {
            Response::Version { .. } => None,
            Response::Status { id, .. }
            | Response::Handle { id, .. }
            | Response::Data { id, .. }
            | Response::Name { id, .. }
            | Response::Attrs { id, .. } => Some(*id),
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding requests (Client -> Server)
// ---------------------------------------------------------------------------

fn frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let len = 1 + payload.len();
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(msg_type);
    out.extend_from_slice(payload);
    out
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    put_bytes(out, s.as_bytes());
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    put_u32(out, b.len() as u32);
    out.extend_from_slice(b);
}

fn put_empty_attrs(out: &mut Vec<u8>) {
    put_u32(out, 0); // flags = 0
}

/// SSH_FXP_INIT packet (version 3)
pub fn init_request() -> Vec<u8> {
    let mut payload = Vec::with_capacity(4);
    put_u32(&mut payload, SFTP_VERSION);
    frame(SSH_FXP_INIT, &payload)
}

/// SSH_FXP_OPENDIR
pub fn opendir_request(id: u32, path: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + path.len());
    put_u32(&mut payload, id);
    put_string(&mut payload, path);
    frame(SSH_FXP_OPENDIR, &payload)
}

/// SSH_FXP_READDIR
pub fn readdir_request(id: u32, handle: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + handle.len());
    put_u32(&mut payload, id);
    put_bytes(&mut payload, handle);
    frame(SSH_FXP_READDIR, &payload)
}

/// SSH_FXP_CLOSE
pub fn close_request(id: u32, handle: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + handle.len());
    put_u32(&mut payload, id);
    put_bytes(&mut payload, handle);
    frame(SSH_FXP_CLOSE, &payload)
}

/// SSH_FXP_OPEN
pub fn open_request(id: u32, path: &str, pflags: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + path.len() + 4 + 4);
    put_u32(&mut payload, id);
    put_string(&mut payload, path);
    put_u32(&mut payload, pflags);
    put_empty_attrs(&mut payload);
    frame(SSH_FXP_OPEN, &payload)
}

/// SSH_FXP_READ
pub fn read_request(id: u32, handle: &[u8], offset: u64, len: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + handle.len() + 8 + 4);
    put_u32(&mut payload, id);
    put_bytes(&mut payload, handle);
    put_u64(&mut payload, offset);
    put_u32(&mut payload, len);
    frame(SSH_FXP_READ, &payload)
}

/// SSH_FXP_WRITE
pub fn write_request(id: u32, handle: &[u8], offset: u64, data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + handle.len() + 8 + 4 + data.len());
    put_u32(&mut payload, id);
    put_bytes(&mut payload, handle);
    put_u64(&mut payload, offset);
    put_bytes(&mut payload, data);
    frame(SSH_FXP_WRITE, &payload)
}

/// SSH_FXP_REALPATH
pub fn realpath_request(id: u32, path: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + path.len());
    put_u32(&mut payload, id);
    put_string(&mut payload, path);
    frame(SSH_FXP_REALPATH, &payload)
}

/// SSH_FXP_STAT
pub fn stat_request(id: u32, path: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + path.len());
    put_u32(&mut payload, id);
    put_string(&mut payload, path);
    frame(SSH_FXP_STAT, &payload)
}

/// SSH_FXP_MKDIR
pub fn mkdir_request(id: u32, path: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + path.len() + 4);
    put_u32(&mut payload, id);
    put_string(&mut payload, path);
    put_empty_attrs(&mut payload);
    frame(SSH_FXP_MKDIR, &payload)
}

/// SSH_FXP_REMOVE (delete file)
pub fn remove_request(id: u32, path: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + path.len());
    put_u32(&mut payload, id);
    put_string(&mut payload, path);
    frame(SSH_FXP_REMOVE, &payload)
}

/// SSH_FXP_RMDIR (delete directory)
pub fn rmdir_request(id: u32, path: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + path.len());
    put_u32(&mut payload, id);
    put_string(&mut payload, path);
    frame(SSH_FXP_RMDIR, &payload)
}

/// SSH_FXP_RENAME
pub fn rename_request(id: u32, oldpath: &str, newpath: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4 + 4 + oldpath.len() + 4 + newpath.len());
    put_u32(&mut payload, id);
    put_string(&mut payload, oldpath);
    put_string(&mut payload, newpath);
    frame(SSH_FXP_RENAME, &payload)
}

// ---------------------------------------------------------------------------
// Decoding responses (Server -> Client)
// ---------------------------------------------------------------------------

pub fn read_attrs(s: &mut impl Read) -> io::Result<FileAttrs> {
    let flags = read_u32(s)?;
    let mut attrs = FileAttrs::default();
    if flags & SSH_FILEXFER_ATTR_SIZE != 0 {
        attrs.size = Some(read_u64(s)?);
    }
    if flags & SSH_FILEXFER_ATTR_UIDGID != 0 {
        attrs.uid = Some(read_u32(s)?);
        attrs.gid = Some(read_u32(s)?);
    }
    if flags & SSH_FILEXFER_ATTR_PERMISSIONS != 0 {
        attrs.permissions = Some(read_u32(s)?);
    }
    if flags & SSH_FILEXFER_ATTR_ACMODTIME != 0 {
        attrs.atime = Some(read_u32(s)?);
        attrs.mtime = Some(read_u32(s)?);
    }
    if flags & SSH_FILEXFER_ATTR_EXTENDED != 0 {
        let count = read_u32(s)? as usize;
        if count > 100 {
            return Err(invalid("too many extended attributes"));
        }
        for _ in 0..count {
            let _ext_type = read_string(s, MAX_STRING_LEN)?;
            let _ext_data = read_string(s, MAX_STRING_LEN)?;
        }
    }
    Ok(attrs)
}

pub fn read_raw_bytes(s: &mut impl Read, max: usize) -> io::Result<Vec<u8>> {
    let len = read_u32(s)? as usize;
    if len > max {
        return Err(invalid("byte buffer exceeds maximum allowed length"));
    }
    read_bytes(s, len)
}

/// Reads one SFTP packet from the stream.
pub fn read_packet(s: &mut impl Read) -> io::Result<Response> {
    let length = read_u32(s)?;
    if length == 0 || length > MAX_PACKET_LEN {
        return Err(invalid(format!("invalid SFTP packet length: {length}")));
    }
    let msg_type = read_u8(s)?;
    let body_len = (length - 1) as usize;
    let mut body = io::Cursor::new(read_bytes(s, body_len)?);

    match msg_type {
        SSH_FXP_VERSION => {
            let version = read_u32(&mut body)?;
            Ok(Response::Version { version })
        }
        SSH_FXP_STATUS => {
            let id = read_u32(&mut body)?;
            let code = read_u32(&mut body)?;
            let message = read_string(&mut body, MAX_STRING_LEN).unwrap_or_default();
            Ok(Response::Status { id, code, message })
        }
        SSH_FXP_HANDLE => {
            let id = read_u32(&mut body)?;
            let handle = read_raw_bytes(&mut body, 1024)?;
            Ok(Response::Handle { id, handle })
        }
        SSH_FXP_DATA => {
            let id = read_u32(&mut body)?;
            let data = read_raw_bytes(&mut body, MAX_PACKET_LEN as usize)?;
            Ok(Response::Data { id, data })
        }
        SSH_FXP_NAME => {
            let id = read_u32(&mut body)?;
            let count = read_u32(&mut body)? as usize;
            if count > MAX_ENTRIES {
                return Err(invalid("too many items in SSH_FXP_NAME response"));
            }
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                let filename = read_string(&mut body, MAX_STRING_LEN)?;
                let longname = read_string(&mut body, MAX_STRING_LEN)?;
                let attrs = read_attrs(&mut body)?;
                items.push(NameItem {
                    filename,
                    longname,
                    attrs,
                });
            }
            Ok(Response::Name { id, items })
        }
        SSH_FXP_ATTRS => {
            let id = read_u32(&mut body)?;
            let attrs = read_attrs(&mut body)?;
            Ok(Response::Attrs { id, attrs })
        }
        other => Err(invalid(format!("unsupported SFTP packet type: {other}"))),
    }
}

pub fn status_to_string(code: u32, server_msg: &str) -> String {
    let base = match code {
        SSH_FX_OK => "Success",
        SSH_FX_EOF => "End of file",
        SSH_FX_NO_SUCH_FILE => "No such file or directory",
        SSH_FX_PERMISSION_DENIED => "Permission denied",
        SSH_FX_FAILURE => "Operation failed",
        SSH_FX_BAD_MESSAGE => "Bad message",
        SSH_FX_NO_CONNECTION => "No connection",
        SSH_FX_CONNECTION_LOST => "Connection lost",
        SSH_FX_OP_UNSUPPORTED => "Operation unsupported",
        _ => "Unknown error",
    };
    if server_msg.is_empty() {
        base.to_string()
    } else {
        format!("{base}: {server_msg}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_roundtrip() {
        let req = init_request();
        assert_eq!(&req[..4], &5u32.to_be_bytes());
        assert_eq!(req[4], SSH_FXP_INIT);
        assert_eq!(&req[5..9], &3u32.to_be_bytes());

        // Simulate version response
        let resp_payload = 3u32.to_be_bytes();
        let frame = frame(SSH_FXP_VERSION, &resp_payload);
        let mut cursor = io::Cursor::new(frame);
        let resp = read_packet(&mut cursor).unwrap();
        match resp {
            Response::Version { version } => assert_eq!(version, 3),
            _ => panic!("expected version response"),
        }
    }

    #[test]
    fn opendir_and_name_response() {
        let req = opendir_request(42, "/home/user");
        assert_eq!(req[4], SSH_FXP_OPENDIR);

        // Build a mock Name response with 2 entries
        let mut body = Vec::new();
        put_u32(&mut body, 42); // id
        put_u32(&mut body, 2); // count = 2
        // item 1: dir
        put_string(&mut body, "docs");
        put_string(&mut body, "drwxr-xr-x 2 user staff 4096 Oct 20 docs");
        put_u32(
            &mut body,
            SSH_FILEXFER_ATTR_PERMISSIONS | SSH_FILEXFER_ATTR_SIZE,
        );
        put_u64(&mut body, 4096);
        put_u32(&mut body, 0o040755); // directory mode

        // item 2: file
        put_string(&mut body, "hello.txt");
        put_string(&mut body, "-rw-r--r-- 1 user staff 12 Oct 20 hello.txt");
        put_u32(
            &mut body,
            SSH_FILEXFER_ATTR_PERMISSIONS | SSH_FILEXFER_ATTR_SIZE,
        );
        put_u64(&mut body, 12);
        put_u32(&mut body, 0o100644); // regular file mode

        let f = frame(SSH_FXP_NAME, &body);
        let mut cursor = io::Cursor::new(f);
        let resp = read_packet(&mut cursor).unwrap();
        match resp {
            Response::Name { id, items } => {
                assert_eq!(id, 42);
                assert_eq!(items.len(), 2);
                let e1 = items[0].clone().into_entry();
                assert_eq!(e1.name, "docs");
                assert!(e1.is_dir);
                assert_eq!(e1.size, 4096);

                let e2 = items[1].clone().into_entry();
                assert_eq!(e2.name, "hello.txt");
                assert!(!e2.is_dir);
                assert_eq!(e2.size, 12);
            }
            _ => panic!("expected Name response"),
        }
    }

    #[test]
    fn status_parsing() {
        let mut body = Vec::new();
        put_u32(&mut body, 99);
        put_u32(&mut body, SSH_FX_PERMISSION_DENIED);
        put_string(&mut body, "Access denied");
        put_string(&mut body, "en");

        let f = frame(SSH_FXP_STATUS, &body);
        let mut cursor = io::Cursor::new(f);
        let resp = read_packet(&mut cursor).unwrap();
        match resp {
            Response::Status { id, code, message } => {
                assert_eq!(id, 99);
                assert_eq!(code, SSH_FX_PERMISSION_DENIED);
                assert_eq!(message, "Access denied");
                assert_eq!(
                    status_to_string(code, &message),
                    "Permission denied: Access denied"
                );
            }
            _ => panic!("expected status response"),
        }
    }

    #[test]
    fn rejects_oversized_packet() {
        let mut bad = Vec::new();
        put_u32(&mut bad, MAX_PACKET_LEN + 10);
        bad.push(SSH_FXP_DATA);
        let mut cursor = io::Cursor::new(bad);
        assert!(read_packet(&mut cursor).is_err());
    }
}
