//! SSH through the system OpenSSH client (no SSH library): the tunnel that carries VNC, and askpass
//! mode, in which ssh runs Lucarna itself to answer its password prompt from the keychain.
//! Design and security notes: docs/ARCHITECTURE.md.

use std::env;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::features::connection::{self, ServerAddress, SshTarget};

/// Set on ssh so the askpass process (Lucarna again) knows which target's password to give. Not a secret.
const ASKPASS_ENV: &str = "LUCARNA_ASKPASS";
/// Long enough for a slow login, short enough that an ssh stuck on a prompt nobody can answer is noticed.
const LOGIN_DEADLINE: Duration = Duration::from_secs(20);
const STDERR_KEPT: usize = 4096;
const MISSING: &str =
    "OpenSSH client not found. On Windows: Settings → System → Optional features → OpenSSH Client";

/// The ssh binary. On Windows the inbox one, so Git's or MSYS's ssh earlier in PATH is not picked up.
fn program() -> PathBuf {
    #[cfg(windows)]
    if let Some(root) = env::var_os("SystemRoot") {
        let inbox = PathBuf::from(root).join(r"System32\OpenSSH\ssh.exe");
        if inbox.is_file() {
            return inbox;
        }
    }
    PathBuf::from("ssh")
}

/// An ssh command with the options Lucarna always wants; callers add their own and then `--` and the host.
/// With a saved password, ssh asks Lucarna for it (askpass mode); without one, BatchMode makes keys and
/// the agent the only way in, so ssh fails fast instead of waiting on a prompt nobody sees.
fn command(target: &SshTarget) -> Command {
    let mut c = Command::new(program());
    // No pty, no X11 or agent forwarding, and nothing from ~/.ssh/config that could hijack or share the connection.
    c.args(["-T", "-x", "-a"]);
    for option in [
        "StrictHostKeyChecking=accept-new",
        "ConnectTimeout=10",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
        "NumberOfPasswordPrompts=1",
        // INFO, not ERROR: refused forwards ("channel N: open failed: …") are only reported at INFO.
        "LogLevel=INFO",
        "ControlMaster=no",
        "ControlPath=none",
        "PermitLocalCommand=no",
    ] {
        c.args(["-o", option]);
    }
    if connection::ssh_password(target).is_some() {
        if let Ok(exe) = env::current_exe() {
            c.env("SSH_ASKPASS", exe);
        }
        c.env("SSH_ASKPASS_REQUIRE", "force")
            .env(ASKPASS_ENV, target.as_str());
        // OpenSSH before 8.4 ignores SSH_ASKPASS_REQUIRE and only uses askpass when DISPLAY is set.
        if env::var_os("DISPLAY").is_none() {
            c.env("DISPLAY", ":0");
        }
    } else {
        c.args(["-o", "BatchMode=yes"]);
    }
    if let Some(user) = &target.user {
        c.args(["-l", user]);
    }
    if let Some(port) = target.port {
        c.args(["-p", &port.to_string()]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Builds an SSH command targeting the remote SFTP subsystem: `ssh [opts] -s -- <host> sftp`
pub fn command_sftp(target: &SshTarget) -> Command {
    let mut c = command(target);
    c.args(["-s", "--", &target.host, "sftp"]);
    c
}

/// Friendly description of an SSH error from its stderr output.
pub fn friendly_error(stderr: &str) -> String {
    friendly(stderr)
}

/// A running ssh, killed when dropped. Keeps the tail of its stderr for error messages.
pub struct Process {
    child: Child,
    stderr: Arc<Mutex<String>>,
    reader: Option<JoinHandle<()>>,
}

impl Process {
    fn spawn(mut c: Command) -> io::Result<Self> {
        c.stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                io::Error::new(e.kind(), MISSING)
            } else {
                e
            }
        })?;
        let stderr = Arc::new(Mutex::new(String::new()));
        let (mut pipe, log) = (child.stderr.take().expect("piped stderr"), stderr.clone());
        let reader = thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n @ 1..) = pipe.read(&mut buf) {
                let mut s = log.lock().unwrap();
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                if s.len() > STDERR_KEPT {
                    let mut cut = s.len() - STDERR_KEPT;
                    while !s.is_char_boundary(cut) {
                        cut += 1;
                    }
                    s.drain(..cut);
                }
            }
        });
        Ok(Self {
            child,
            stderr,
            reader: Some(reader),
        })
    }

    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// What went wrong, if ssh exited or reported a known failure. Banners and warnings alone don't count.
    fn complaint(&mut self) -> Option<String> {
        let exited = self.exited();
        if exited && let Some(reader) = self.reader.take() {
            let _ = reader.join(); // the pipe closed with the process, so this reads the last lines
        }
        let stderr = self.stderr.lock().unwrap();
        (exited || failure_line(&stderr).is_some()).then(|| friendly(&stderr))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `ssh -N -L` from a local port to the VNC server. Dropping it closes the tunnel.
pub struct Tunnel(Process);

impl Tunnel {
    /// Opens the tunnel and returns the VNC connection through it. The connection itself is the readiness
    /// check: ssh only listens on the local port once it has logged in, so no throwaway probe ever reaches
    /// the VNC server.
    pub fn open(target: &SshTarget, vnc: &ServerAddress) -> io::Result<(Self, TcpStream)> {
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        // ponytail: another program could take the port before ssh does; ExitOnForwardFailure then reports it.
        let forward = format!("127.0.0.1:{port}:{}", forward_target(target, vnc)?);
        let mut c = command(target);
        c.args([
            "-N",
            "-n",
            "-o",
            "ExitOnForwardFailure=yes",
            "-L",
            &forward,
            "--",
            &target.host,
        ]);
        c.stdin(Stdio::null()).stdout(Stdio::null());
        let mut ssh = Process::spawn(c)?;

        let local = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let deadline = Instant::now() + LOGIN_DEADLINE;
        loop {
            if ssh.exited() {
                let why = ssh
                    .complaint()
                    .unwrap_or_else(|| "SSH: the connection closed while logging in".into());
                return Err(io::Error::other(with_version_hint(target, why)));
            }
            if let Ok(stream) = TcpStream::connect_timeout(&local, Duration::from_secs(1)) {
                return Ok((Self(ssh), stream));
            }
            if Instant::now() > deadline {
                let why = "SSH did not finish logging in within 20 s".to_string();
                return Err(io::Error::other(with_version_hint(target, why)));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Why the tunnel failed, if it did: ssh died, or it reported that it could not reach the VNC server.
    /// Waits a moment first: our socket's EOF can arrive before ssh's explanation on stderr.
    pub fn error(&mut self) -> Option<String> {
        thread::sleep(Duration::from_millis(300));
        self.0.complaint()
    }
}

/// Where the tunnel leads, seen from the SSH host. When the VNC server runs on the SSH host itself we
/// forward to its loopback, so UltraVNC can be set to accept loopback connections only.
fn forward_target(target: &SshTarget, vnc: &ServerAddress) -> io::Result<String> {
    let (host, port) = vnc.host_port().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("can't tunnel to {}", vnc.as_str()),
        )
    })?;
    let host = if host.eq_ignore_ascii_case(&target.host) {
        "localhost"
    } else {
        host
    };
    Ok(if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    })
}

/// Lines ssh writes when something actually failed, as opposed to banners and warnings.
const FAILURES: &[&str] = &[
    "open failed",
    "Permission denied",
    "Host key verification failed",
    "IDENTIFICATION HAS CHANGED",
    "Connection refused",
    "Connection timed out",
    "Could not resolve",
    "No route to host",
    "Network is unreachable",
    "Connection closed",
    "Address already in use",
];

fn failure_line(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| FAILURES.iter().any(|f| l.contains(f)))
}

/// ssh's most relevant complaint plus a hint about what usually causes it.
fn friendly(stderr: &str) -> String {
    let line = failure_line(stderr)
        .or_else(|| stderr.lines().rev().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("SSH failed");
    let hint = if stderr.contains("administratively prohibited") {
        " (the SSH server forbids tunnels: set AllowTcpForwarding yes in its sshd_config)"
    } else if stderr.contains("open failed") {
        " (is the VNC server running and reachable from the SSH host?)"
    } else if stderr.contains("Permission denied") {
        " (check the SSH user and password, or add your key to ssh-agent)"
    } else if stderr.contains("IDENTIFICATION HAS CHANGED")
        || stderr.contains("Host key verification failed")
    {
        " (the server's SSH host key changed; if that was expected, run: ssh-keygen -R <host>)"
    } else if stderr.contains("Connection refused") {
        " (is the SSH server running?)"
    } else {
        ""
    };
    format!("SSH: {line}{hint}")
}

/// Before 8.4 OpenSSH can't be told to always ask Lucarna for the password (common on Windows 10).
fn with_version_hint(target: &SshTarget, why: String) -> String {
    let old = connection::ssh_password(target).is_some()
        && Command::new(program())
            .arg("-V")
            .output()
            .ok()
            .and_then(|o| version(&String::from_utf8_lossy(&o.stderr)))
            .is_some_and(|v| v < (8, 4));
    if old {
        format!(
            "{why}. This OpenSSH is older than 8.4 and can't take the password from Lucarna: use an SSH key instead"
        )
    } else {
        why
    }
}

/// `OpenSSH_10.3p1, LibreSSL…` or `OpenSSH_for_Windows_8.1p1, …` → (major, minor).
fn version(banner: &str) -> Option<(u32, u32)> {
    let digits = banner
        .split("OpenSSH_")
        .nth(1)?
        .trim_start_matches("for_Windows_");
    let (major, rest) = digits.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Askpass mode. ssh runs Lucarna with its prompt as the only argument; Lucarna prints the saved SSH password
/// for the target in `LUCARNA_ASKPASS`, and only for password prompts (never key passphrases, one-time codes
/// or yes/no questions), then exits. Returns that exit code, or None when Lucarna was started normally.
pub fn askpass() -> Option<i32> {
    let target = env::var(ASKPASS_ENV).ok()?;
    let prompt = env::args().nth(1).unwrap_or_default();
    let password = SshTarget::parse(&target)
        .ok()
        .flatten()
        .filter(|_| answers(&prompt))
        .and_then(|t| connection::ssh_password(&t));
    Some(match password {
        // Write errors (no stdout) count as "no answer" instead of panicking like println! would.
        Some(p) if writeln!(io::stdout(), "{p}").is_ok() => 0,
        _ => 1,
    })
}

fn answers(prompt: &str) -> bool {
    let p = prompt.to_lowercase();
    p.contains("password") && !p.contains("passphrase")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(s: &str) -> SshTarget {
        SshTarget::parse(s).unwrap().unwrap()
    }

    #[test]
    fn askpass_only_answers_password_prompts() {
        assert!(answers("ana@office's password: "));
        assert!(answers("(ana@office) Password:"));
        assert!(!answers(
            "Enter passphrase for key '/Users/ana/.ssh/id_ed25519': "
        ));
        assert!(!answers(
            "Are you sure you want to continue connecting (yes/no/[fingerprint])?"
        ));
        assert!(!answers("Verification code: "));
    }

    #[test]
    fn openssh_versions() {
        assert_eq!(version("OpenSSH_10.3p1, LibreSSL 3.3.6"), Some((10, 3)));
        assert_eq!(
            version("OpenSSH_for_Windows_8.1p1, LibreSSL 3.0.2"),
            Some((8, 1))
        );
        assert_eq!(version("garbage"), None);
    }

    #[test]
    fn forwards_to_loopback_when_vnc_runs_on_the_ssh_host() {
        let vnc = |s| ServerAddress::parse(s).unwrap();
        assert_eq!(
            forward_target(&target("ana@Office"), &vnc("office")).unwrap(),
            "localhost:5900"
        );
        assert_eq!(
            forward_target(&target("ana@office"), &vnc("localhost:1")).unwrap(),
            "localhost:5901"
        );
        assert_eq!(
            forward_target(&target("ana@gateway"), &vnc("10.0.0.7")).unwrap(),
            "10.0.0.7:5900",
            "SSH host as gateway"
        );
    }

    #[test]
    fn friendly_errors() {
        assert!(friendly("ana@pc: Permission denied (publickey,password).").contains("ssh-agent"));
        assert!(
            friendly("channel 2: open failed: connect failed: Connection refused")
                .contains("VNC server")
        );
        assert!(
            friendly("ssh: connect to host pc port 22: Connection refused").contains("SSH server")
        );
        assert!(friendly("Host key verification failed.").contains("ssh-keygen -R"));
        assert!(
            friendly("channel 2: open failed: administratively prohibited: open failed")
                .contains("AllowTcpForwarding")
        );
        // A server banner after the real failure must not hide it.
        let with_banner =
            "channel 2: open failed: connect failed: Connection refused\nWelcome to Office PC";
        assert!(friendly(with_banner).contains("open failed"));
        assert_eq!(
            failure_line("Warning: Permanently added '[pc]:22' to the list of known hosts."),
            None
        );
    }

    #[test]
    fn host_always_follows_the_options() {
        let c = command(&target("ana@office:2222"));
        let args: Vec<_> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(2).any(|w| w == ["-l", "ana"])
                && args.windows(2).any(|w| w == ["-p", "2222"])
        );
        assert!(
            !args.iter().any(|a| a == "office"),
            "the host is added by callers after `--`"
        );
    }
}
