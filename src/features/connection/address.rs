/// Where a VNC server listens, normalized to "host:port".
#[derive(Clone, Debug, PartialEq)]
pub struct ServerAddress(String);

impl ServerAddress {
    /// Accepts "host", "host:display" (port 5900+display) or "host::port".
    pub fn parse(input: &str) -> Option<Self> {
        let input = input.trim();
        if input.is_empty() {
            return None;
        }
        if let Some((host, port)) = input.split_once("::") {
            return Some(Self(format!("{host}:{port}")));
        }
        Some(Self(match input.rsplit_once(':').and_then(|(h, p)| Some((h, p.parse::<u16>().ok()?))) {
            Some((host, n)) if n < 100 => format!("{host}:{}", 5900 + n),
            Some((host, port)) => format!("{host}:{port}"),
            None => format!("{input}:5900"),
        }))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Host and port; None for repeater IDs like `ID:1234`, whose "port" is not one.
    pub fn host_port(&self) -> Option<(&str, u16)> {
        let (host, port) = self.0.rsplit_once(':')?;
        Some((host.trim_start_matches('[').trim_end_matches(']'), port.parse().ok()?))
    }
}

/// Where to log in over SSH: `user@host[:port]`. User and port are optional so `~/.ssh/config` can fill them in.
#[derive(Clone, Debug, PartialEq)]
pub struct SshTarget {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

impl SshTarget {
    /// Empty input means "no SSH". Anything ssh could read as an option, or that would split into
    /// several arguments, is refused: a bad target must never fall back to an unencrypted connection.
    pub fn parse(input: &str) -> Result<Option<Self>, String> {
        let input = input.trim();
        if input.is_empty() {
            return Ok(None);
        }
        let bad = || format!("invalid SSH target \"{input}\": use user@host or user@host:port");
        if input.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(bad());
        }
        let (user, rest) = match input.rsplit_once('@') {
            Some((user, rest)) => (Some(user), rest),
            None => (None, input),
        };
        let (host, port) = match rest.strip_prefix('[') {
            Some(v6) => {
                let (host, after) = v6.split_once(']').ok_or_else(bad)?;
                (host, if after.is_empty() { None } else { Some(after.strip_prefix(':').ok_or_else(bad)?) })
            }
            None if rest.matches(':').count() > 1 => return Err(bad()),
            None => match rest.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (rest, None),
            },
        };
        let port = match port {
            Some(p) => Some(p.parse::<u16>().ok().filter(|&p| p > 0).ok_or_else(bad)?),
            None => None,
        };
        let starts_badly = |s: &str| s.is_empty() || s.starts_with('-');
        if starts_badly(host) || user.is_some_and(starts_badly) || host.contains('@') {
            return Err(bad());
        }
        Ok(Some(Self { user: user.map(str::to_string), host: host.to_string(), port }))
    }

    /// Normalized `user@host[:port]`, with brackets around IPv6 hosts.
    pub fn as_str(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        let user = self.user.as_ref().map(|u| format!("{u}@")).unwrap_or_default();
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        format!("{user}{host}{port}")
    }

    /// Keychain account for this target's SSH password, kept apart from VNC passwords.
    pub fn account(&self) -> String {
        format!("ssh:{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_forms() {
        let parse = |s| ServerAddress::parse(s).map(|a| a.0);
        assert_eq!(parse("10.0.0.5").unwrap(), "10.0.0.5:5900");
        assert_eq!(parse("10.0.0.5:1").unwrap(), "10.0.0.5:5901");
        assert_eq!(parse("10.0.0.5:5905").unwrap(), "10.0.0.5:5905");
        assert_eq!(parse("host::22").unwrap(), "host:22");
        assert_eq!(parse("  "), None);
    }

    #[test]
    fn host_port() {
        assert_eq!(ServerAddress::parse("pc:1").unwrap().host_port(), Some(("pc", 5901)));
        assert_eq!(ServerAddress::parse("ID:12345").unwrap().host_port(), Some(("ID", 12345)));
    }

    #[test]
    fn ssh_targets() {
        let ok = |s| SshTarget::parse(s).unwrap().unwrap();
        assert_eq!(SshTarget::parse("  ").unwrap(), None);
        assert_eq!(ok("ana@office.example.com"), SshTarget { user: Some("ana".into()), host: "office.example.com".into(), port: None });
        assert_eq!(ok("office:2222").port, Some(2222));
        assert_eq!(ok("ana@corp.local@pc").user.as_deref(), Some("ana@corp.local"), "UPN-style Windows users");
        assert_eq!(ok("ana@[fd00::1]:22").as_str(), "ana@[fd00::1]:22");
        assert_eq!(ok("ana@pc:22").account(), "ssh:ana@pc:22");
        for bad in ["-oProxyCommand=x", "ana@-oProxyCommand=x", "-l@pc", "@pc", "ana@", "ana pc", "pc:0", "pc:99999", "pc:x", "fd00::1", "a\tb@pc"] {
            assert!(SshTarget::parse(bad).is_err(), "{bad:?} should be refused");
        }
    }
}
