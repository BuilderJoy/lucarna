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
}
