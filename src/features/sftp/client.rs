//! SFTP client process manager and worker thread.
//! Drives `ssh -s <target> sftp` and handles file operations in background streaming chunks.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::protocol::{
    self, FileAttrs, Response, SSH_FX_EOF, SSH_FX_OK, SSH_FXF_CREAT, SSH_FXF_READ, SSH_FXF_TRUNC,
    SSH_FXF_WRITE,
};
use crate::features::connection::SshTarget;
use crate::features::file_transfer::{FileOps, Update, local_name};
use crate::features::ssh;

const CHUNK_SIZE: u32 = 32 * 1024; // 32 KiB streaming chunks

enum Command {
    Open,
    List(String),
    Download {
        remote_path: String,
        local_dir: PathBuf,
    },
    Upload {
        local_path: PathBuf,
        remote_dir: String,
    },
    CreateDir(String),
    Rename {
        old: String,
        new: String,
    },
    Delete(String),
    Cancel,
    Close,
}

pub struct SftpClient {
    tx: Sender<Command>,
    cancelled: Arc<AtomicBool>,
    _worker: Mutex<Option<JoinHandle<()>>>,
}

impl SftpClient {
    pub fn new(target: SshTarget, emit: impl Fn(Update) + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_flag = cancelled.clone();
        let emit_arc = Arc::new(emit);

        let worker = thread::Builder::new()
            .name("lucarna-sftp".into())
            .spawn(move || {
                run_worker(target, rx, cancel_flag, emit_arc);
            })
            .expect("spawn sftp worker thread");

        Self {
            tx,
            cancelled,
            _worker: Mutex::new(Some(worker)),
        }
    }

    pub fn open(&self) {
        let _ = self.tx.send(Command::Open);
    }

    pub fn list(&self, path: &str) {
        let _ = self.tx.send(Command::List(path.to_string()));
    }

    pub fn download(&self, path: &str, dir: &Path) {
        self.cancelled.store(false, Ordering::SeqCst);
        let _ = self.tx.send(Command::Download {
            remote_path: path.to_string(),
            local_dir: dir.to_path_buf(),
        });
    }

    pub fn upload(&self, local: &Path, remote_dir: &str) {
        self.cancelled.store(false, Ordering::SeqCst);
        let _ = self.tx.send(Command::Upload {
            local_path: local.to_path_buf(),
            remote_dir: remote_dir.to_string(),
        });
    }

    pub fn create_dir(&self, path: &str) {
        let _ = self.tx.send(Command::CreateDir(path.to_string()));
    }

    pub fn rename(&self, old: &str, new: &str) {
        let _ = self.tx.send(Command::Rename {
            old: old.to_string(),
            new: new.to_string(),
        });
    }

    pub fn delete(&self, path: &str) {
        let _ = self.tx.send(Command::Delete(path.to_string()));
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Command::Cancel);
    }
}

impl FileOps for SftpClient {
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

impl Drop for SftpClient {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Close);
    }
}

struct SftpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: Arc<Mutex<String>>,
    next_id: AtomicU32,
    default_dir: String,
}

impl SftpSession {
    fn spawn(target: &SshTarget) -> io::Result<Self> {
        let mut cmd = ssh::command_sftp(target);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::new(e.kind(), "OpenSSH client not found")
            } else {
                e
            }
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "no stdout"))?;
        let mut err_pipe = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "no stderr"))?;

        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_clone = stderr.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n @ 1..) = err_pipe.read(&mut buf) {
                let mut s = stderr_clone.lock().unwrap();
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });

        Ok(Self {
            child,
            stdin,
            stdout,
            stderr,
            next_id: AtomicU32::new(1),
            default_dir: "/".into(),
        })
    }

    fn next_id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    fn send(&mut self, packet: &[u8]) -> io::Result<()> {
        self.stdin.write_all(packet)?;
        self.stdin.flush()
    }

    fn read_response(&mut self) -> io::Result<Response> {
        protocol::read_packet(&mut self.stdout)
    }

    fn handshake(&mut self) -> io::Result<u32> {
        self.send(&protocol::init_request())?;
        match self.read_response()? {
            Response::Version { version } => Ok(version),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected response to INIT: {other:?}"),
            )),
        }
    }

    fn realpath(&mut self, path: &str) -> io::Result<String> {
        let id = self.next_id();
        self.send(&protocol::realpath_request(id, path))?;
        match self.read_response()? {
            Response::Name { items, .. } if !items.is_empty() => Ok(items[0].filename.clone()),
            Response::Status { code, message, .. } => {
                Err(io::Error::other(protocol::status_to_string(code, &message)))
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected response to REALPATH: {other:?}"),
            )),
        }
    }

    fn stat(&mut self, path: &str) -> io::Result<FileAttrs> {
        let id = self.next_id();
        self.send(&protocol::stat_request(id, path))?;
        match self.read_response()? {
            Response::Attrs { attrs, .. } => Ok(attrs),
            Response::Status { code, message, .. } => {
                Err(io::Error::other(protocol::status_to_string(code, &message)))
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected response to STAT: {other:?}"),
            )),
        }
    }

    fn open_handle(&mut self, path: &str, pflags: u32) -> io::Result<Vec<u8>> {
        let id = self.next_id();
        self.send(&protocol::open_request(id, path, pflags))?;
        match self.read_response()? {
            Response::Handle { handle, .. } => Ok(handle),
            Response::Status { code, message, .. } => {
                Err(io::Error::other(protocol::status_to_string(code, &message)))
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected response to OPEN: {other:?}"),
            )),
        }
    }

    fn open_dir_handle(&mut self, path: &str) -> io::Result<Vec<u8>> {
        let id = self.next_id();
        self.send(&protocol::opendir_request(id, path))?;
        match self.read_response()? {
            Response::Handle { handle, .. } => Ok(handle),
            Response::Status { code, message, .. } => {
                Err(io::Error::other(protocol::status_to_string(code, &message)))
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected response to OPENDIR: {other:?}"),
            )),
        }
    }

    fn close_handle(&mut self, handle: &[u8]) -> io::Result<()> {
        let id = self.next_id();
        self.send(&protocol::close_request(id, handle))?;
        match self.read_response()? {
            Response::Status {
                code: SSH_FX_OK, ..
            } => Ok(()),
            Response::Status { code, message, .. } => {
                Err(io::Error::other(protocol::status_to_string(code, &message)))
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected response to CLOSE: {other:?}"),
            )),
        }
    }

    fn complaint(&self) -> String {
        let err = self.stderr.lock().unwrap();
        if err.trim().is_empty() {
            "SFTP process exited unexpectedly".into()
        } else {
            ssh::friendly_error(&err)
        }
    }
}

impl Drop for SftpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_worker(
    target: SshTarget,
    rx: Receiver<Command>,
    cancel: Arc<AtomicBool>,
    emit: Arc<dyn Fn(Update) + Send + Sync>,
) {
    let mut session: Option<SftpSession> = None;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Close => break,
            Command::Cancel => {
                cancel.store(true, Ordering::SeqCst);
            }
            Command::Open => {
                match SftpSession::spawn(&target) {
                    Ok(mut s) => {
                        match s.handshake() {
                            Ok(_version) => {
                                let default_dir = s.realpath(".").unwrap_or_else(|_| "/".into());
                                s.default_dir =
                                    if default_dir.ends_with('/') || default_dir.ends_with('\\') {
                                        default_dir.clone()
                                    } else if default_dir.contains('\\') {
                                        format!("{default_dir}\\")
                                    } else {
                                        format!("{default_dir}/")
                                    };

                                emit(Update::Access(true));

                                // Discover drives:
                                let drives = if s.default_dir.contains(':') {
                                    // Windows OpenSSH (e.g. C:/Users/name)
                                    let drive_letter = s
                                        .default_dir
                                        .chars()
                                        .next()
                                        .unwrap_or('C')
                                        .to_ascii_uppercase();
                                    vec![(format!("{drive_letter}:"), 'l')]
                                } else {
                                    // POSIX
                                    vec![("/".into(), 'l'), ("~".into(), 'l')]
                                };
                                emit(Update::Drives(drives));

                                let path_to_list = s.default_dir.clone();
                                let mut active = s;
                                list_dir(&mut active, &path_to_list, &emit);
                                session = Some(active);
                            }
                            Err(e) => {
                                let complaint = s.complaint();
                                emit(Update::Failed(format!(
                                    "SFTP handshake failed ({e}): {complaint}"
                                )));
                            }
                        }
                    }
                    Err(e) => {
                        emit(Update::Failed(format!("Cannot launch SFTP client: {e}")));
                    }
                }
            }
            Command::List(path) => {
                if let Some(s) = &mut session {
                    list_dir(s, &path, &emit);
                } else {
                    emit(Update::Failed("SFTP session not open".into()));
                }
            }
            Command::Download {
                remote_path,
                local_dir,
            } => {
                if let Some(s) = &mut session {
                    download_file(s, &remote_path, &local_dir, &cancel, &emit);
                } else {
                    emit(Update::Failed("SFTP session not open".into()));
                }
            }
            Command::Upload {
                local_path,
                remote_dir,
            } => {
                if let Some(s) = &mut session {
                    upload_file(s, &local_path, &remote_dir, &cancel, &emit);
                } else {
                    emit(Update::Failed("SFTP session not open".into()));
                }
            }
            Command::CreateDir(path) => {
                if let Some(s) = &mut session {
                    let id = s.next_id();
                    let trimmed = path.trim_end_matches(['/', '\\']);
                    let _ = s.send(&protocol::mkdir_request(id, trimmed));
                    match s.read_response() {
                        Ok(Response::Status {
                            code: SSH_FX_OK, ..
                        }) => {
                            emit(Update::Command {
                                ok: true,
                                message: format!("Created folder {trimmed}"),
                            });
                        }
                        Ok(Response::Status { code, message, .. }) => {
                            emit(Update::Command {
                                ok: false,
                                message: format!(
                                    "Create folder failed: {}",
                                    protocol::status_to_string(code, &message)
                                ),
                            });
                        }
                        _ => emit(Update::Command {
                            ok: false,
                            message: "Create folder failed".into(),
                        }),
                    }
                }
            }
            Command::Rename { old, new } => {
                if let Some(s) = &mut session {
                    let id = s.next_id();
                    let _ = s.send(&protocol::rename_request(id, &old, &new));
                    match s.read_response() {
                        Ok(Response::Status {
                            code: SSH_FX_OK, ..
                        }) => {
                            emit(Update::Command {
                                ok: true,
                                message: "Renamed successfully".into(),
                            });
                        }
                        Ok(Response::Status { code, message, .. }) => {
                            emit(Update::Command {
                                ok: false,
                                message: format!(
                                    "Rename failed: {}",
                                    protocol::status_to_string(code, &message)
                                ),
                            });
                        }
                        _ => emit(Update::Command {
                            ok: false,
                            message: "Rename failed".into(),
                        }),
                    }
                }
            }
            Command::Delete(path) => {
                if let Some(s) = &mut session {
                    let trimmed = path.trim_end_matches(['/', '\\']);
                    let id1 = s.next_id();
                    let _ = s.send(&protocol::remove_request(id1, trimmed));
                    let is_file_ok = matches!(
                        s.read_response(),
                        Ok(Response::Status {
                            code: SSH_FX_OK,
                            ..
                        })
                    );
                    if is_file_ok {
                        emit(Update::Command {
                            ok: true,
                            message: format!("Deleted {trimmed}"),
                        });
                    } else {
                        // Try rmdir in case it was a directory
                        let id2 = s.next_id();
                        let _ = s.send(&protocol::rmdir_request(id2, trimmed));
                        match s.read_response() {
                            Ok(Response::Status {
                                code: SSH_FX_OK, ..
                            }) => {
                                emit(Update::Command {
                                    ok: true,
                                    message: format!("Deleted folder {trimmed}"),
                                });
                            }
                            Ok(Response::Status { code, message, .. }) => {
                                emit(Update::Command {
                                    ok: false,
                                    message: format!(
                                        "Delete failed: {}",
                                        protocol::status_to_string(code, &message)
                                    ),
                                });
                            }
                            _ => emit(Update::Command {
                                ok: false,
                                message: "Delete failed".into(),
                            }),
                        }
                    }
                }
            }
        }
    }
}

fn list_dir(s: &mut SftpSession, path: &str, emit: &Arc<dyn Fn(Update) + Send + Sync>) {
    let resolved_path = if path == "~" {
        s.default_dir.clone()
    } else {
        path.to_string()
    };

    let query_path = resolved_path.trim_end_matches(['/', '\\']);
    let query_path = if query_path.is_empty() {
        "/"
    } else {
        query_path
    };

    let handle = match s.open_dir_handle(query_path) {
        Ok(h) => h,
        Err(_) => {
            emit(Update::Inaccessible);
            return;
        }
    };

    let mut entries = Vec::new();
    loop {
        let id = s.next_id();
        if s.send(&protocol::readdir_request(id, &handle)).is_err() {
            break;
        }
        match s.read_response() {
            Ok(Response::Name { items, .. }) => {
                if items.is_empty() {
                    break;
                }
                for item in items {
                    if item.filename == "." {
                        continue;
                    }
                    entries.push(item.into_entry());
                }
            }
            Ok(Response::Status {
                code: SSH_FX_EOF, ..
            }) => break,
            _ => break,
        }
    }
    let _ = s.close_handle(&handle);

    // Ensure path ends with separator
    let sep = if resolved_path.contains('\\') {
        '\\'
    } else {
        '/'
    };
    let path_normalized = if resolved_path.ends_with(sep) {
        resolved_path
    } else {
        format!("{resolved_path}{sep}")
    };

    emit(Update::Listing {
        path: path_normalized,
        entries,
    });
}

fn download_file(
    s: &mut SftpSession,
    remote_path: &str,
    local_dir: &Path,
    cancel: &Arc<AtomicBool>,
    emit: &Arc<dyn Fn(Update) + Send + Sync>,
) {
    let raw_name = Path::new(remote_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    let name = local_name(raw_name);

    let dest = local_dir.join(&name);
    let part = local_dir.join(format!(".{name}.lucarna-part"));

    let total = s
        .stat(remote_path)
        .map(|a| a.size.unwrap_or(0))
        .unwrap_or(0);

    let handle = match s.open_handle(remote_path, SSH_FXF_READ) {
        Ok(h) => h,
        Err(e) => {
            emit(Update::Failed(format!("Download {name} failed: {e}")));
            return;
        }
    };

    let mut out_file = match File::create(&part) {
        Ok(f) => f,
        Err(e) => {
            let _ = s.close_handle(&handle);
            emit(Update::Failed(format!(
                "Cannot create local file {}: {e}",
                part.display()
            )));
            return;
        }
    };

    emit(Update::Progress {
        name: name.clone(),
        done: 0,
        total,
    });

    let mut offset = 0u64;
    let mut success = false;

    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = s.close_handle(&handle);
            let _ = fs::remove_file(&part);
            emit(Update::Failed("Download cancelled".into()));
            return;
        }

        let id = s.next_id();
        if s.send(&protocol::read_request(id, &handle, offset, CHUNK_SIZE))
            .is_err()
        {
            break;
        }

        match s.read_response() {
            Ok(Response::Data { data, .. }) => {
                if data.is_empty() {
                    success = true;
                    break;
                }
                if out_file.write_all(&data).is_err() {
                    break;
                }
                offset += data.len() as u64;
                emit(Update::Progress {
                    name: name.clone(),
                    done: offset,
                    total: total.max(offset),
                });
            }
            Ok(Response::Status {
                code: SSH_FX_EOF, ..
            }) => {
                success = true;
                break;
            }
            Ok(Response::Status { code, message, .. }) => {
                let _ = s.close_handle(&handle);
                let _ = fs::remove_file(&part);
                emit(Update::Failed(format!(
                    "Read failed: {}",
                    protocol::status_to_string(code, &message)
                )));
                return;
            }
            _ => break,
        }
    }

    let _ = s.close_handle(&handle);
    let _ = out_file.flush();
    drop(out_file);

    if success {
        if fs::rename(&part, &dest).is_ok() {
            emit(Update::Downloaded(dest));
        } else {
            emit(Update::Failed("Failed to finalize downloaded file".into()));
        }
    } else {
        let _ = fs::remove_file(&part);
        emit(Update::Failed(format!(
            "Download of {name} was interrupted"
        )));
    }
}

fn upload_file(
    s: &mut SftpSession,
    local_path: &Path,
    remote_dir: &str,
    cancel: &Arc<AtomicBool>,
    emit: &Arc<dyn Fn(Update) + Send + Sync>,
) {
    let raw_name = local_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("upload");
    let name = local_name(raw_name);

    let (mut file, total) =
        match File::open(local_path).and_then(|f| f.metadata().map(|m| (f, m.len()))) {
            Ok((f, len)) => (f, len),
            Err(e) => {
                emit(Update::Failed(format!("Cannot open local file: {e}")));
                return;
            }
        };

    let sep = if remote_dir.contains('\\') { '\\' } else { '/' };
    let remote_target = if remote_dir.ends_with(sep) {
        format!("{remote_dir}{name}")
    } else {
        format!("{remote_dir}{sep}{name}")
    };

    let handle = match s.open_handle(
        &remote_target,
        SSH_FXF_WRITE | SSH_FXF_CREAT | SSH_FXF_TRUNC,
    ) {
        Ok(h) => h,
        Err(e) => {
            emit(Update::Failed(format!(
                "Cannot create remote file {remote_target}: {e}"
            )));
            return;
        }
    };

    emit(Update::Progress {
        name: name.clone(),
        done: 0,
        total,
    });

    let mut offset = 0u64;
    let mut buf = vec![0u8; CHUNK_SIZE as usize];
    let mut success = true;

    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = s.close_handle(&handle);
            emit(Update::Failed("Upload cancelled".into()));
            return;
        }

        let n = match file.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(e) => {
                emit(Update::Failed(format!("Failed reading local file: {e}")));
                success = false;
                break;
            }
        };

        let id = s.next_id();
        if s.send(&protocol::write_request(id, &handle, offset, &buf[..n]))
            .is_err()
        {
            success = false;
            break;
        }

        match s.read_response() {
            Ok(Response::Status {
                code: SSH_FX_OK, ..
            }) => {
                offset += n as u64;
                emit(Update::Progress {
                    name: name.clone(),
                    done: offset,
                    total,
                });
            }
            Ok(Response::Status { code, message, .. }) => {
                emit(Update::Failed(format!(
                    "Write failed: {}",
                    protocol::status_to_string(code, &message)
                )));
                success = false;
                break;
            }
            _ => {
                success = false;
                break;
            }
        }
    }

    let _ = s.close_handle(&handle);

    if success {
        emit(Update::Uploaded(name));
    }
}
