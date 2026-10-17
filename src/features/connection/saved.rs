//! Saved connections: names and addresses in a plain file, passwords only in the OS keychain
//! (Keychain on macOS, Credential Manager on Windows, Secret Service on Linux).

use std::{fs, io, path::PathBuf};

use super::ServerAddress;

const SERVICE: &str = "Lucarna";

#[derive(Clone, Debug, PartialEq)]
pub struct SavedConnection {
    pub name: String,
    pub address: ServerAddress,
    /// Windows user name for MS-Logon servers; empty for plain VNC passwords.
    pub user: String,
}

fn file() -> Option<PathBuf> {
    eframe::storage_dir("Lucarna").map(|d| d.join("connections.txt"))
}

pub fn load() -> Vec<SavedConnection> {
    file().and_then(|f| fs::read_to_string(f).ok()).map(|t| parse(&t)).unwrap_or_default()
}

pub fn store(list: &[SavedConnection]) -> Result<(), String> {
    let write = || -> io::Result<()> {
        let f = file().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no data directory"))?;
        fs::create_dir_all(f.parent().unwrap())?;
        fs::write(f, render(list))
    };
    write().map_err(|e| format!("could not save connections: {e}"))
}

/// One "name<TAB>address<TAB>user" per line (the user column is optional); lines that don't parse are dropped.
fn parse(text: &str) -> Vec<SavedConnection> {
    text.lines()
        .filter_map(|l| {
            let mut cols = l.splitn(3, '\t');
            let (name, address) = (cols.next()?, cols.next()?);
            Some(SavedConnection { name: name.into(), address: ServerAddress::parse(address)?, user: cols.next().unwrap_or_default().into() })
        })
        .collect()
}

fn render(list: &[SavedConnection]) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    list.iter().map(|c| format!("{}\t{}\t{}\n", clean(&c.name), c.address.as_str(), clean(&c.user))).collect()
}

/// The keychain password for `address`, if one was saved.
pub fn password(address: &ServerAddress) -> Option<String> {
    keyring::Entry::new(SERVICE, address.as_str()).ok()?.get_password().ok()
}

/// Stores the password in the keychain; an empty one removes the entry.
pub fn set_password(address: &ServerAddress, password: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, address.as_str()).map_err(|e| format!("keychain: {e}"))?;
    match if password.is_empty() { entry.delete_credential() } else { entry.set_password(password) } {
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
            SavedConnection { name: "Office\tPC".into(), address: ServerAddress::parse("10.0.0.5:1").unwrap(), user: "CORP\\ana".into() },
            SavedConnection { name: "Lab".into(), address: ServerAddress::parse("lab::6000").unwrap(), user: String::new() },
        ];
        let text = render(&list);
        assert_eq!(text, "Office PC\t10.0.0.5:5901\tCORP\\ana\nLab\tlab:6000\t\n");
        // Files written before the user column existed still load.
        let old = "Old\t10.0.0.9:5900\n";
        assert_eq!(parse(&format!("{text}garbage line\n{old}")), [
            SavedConnection { name: "Office PC".into(), ..list[0].clone() },
            list[1].clone(),
            SavedConnection { name: "Old".into(), address: ServerAddress::parse("10.0.0.9").unwrap(), user: String::new() },
        ]);
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
