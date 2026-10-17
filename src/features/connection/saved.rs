//! Saved connections: names and addresses in a plain file, passwords only in the OS keychain
//! (Keychain on macOS, Credential Manager on Windows, Secret Service on Linux).

use std::{fs, io, path::PathBuf};

use super::ServerAddress;

const SERVICE: &str = "Lucarna";

#[derive(Clone, Debug, PartialEq)]
pub struct SavedConnection {
    pub name: String,
    pub address: ServerAddress,
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

/// One "name<TAB>address" per line; lines that don't parse are dropped.
fn parse(text: &str) -> Vec<SavedConnection> {
    text.lines()
        .filter_map(|l| {
            let (name, address) = l.split_once('\t')?;
            Some(SavedConnection { name: name.into(), address: ServerAddress::parse(address)? })
        })
        .collect()
}

fn render(list: &[SavedConnection]) -> String {
    list.iter().map(|c| format!("{}\t{}\n", c.name.replace(['\t', '\n', '\r'], " "), c.address.as_str())).collect()
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
            SavedConnection { name: "Office\tPC".into(), address: ServerAddress::parse("10.0.0.5:1").unwrap() },
            SavedConnection { name: "Lab".into(), address: ServerAddress::parse("lab::6000").unwrap() },
        ];
        let text = render(&list);
        assert_eq!(text, "Office PC\t10.0.0.5:5901\nLab\tlab:6000\n");
        assert_eq!(parse(&format!("{text}garbage line\n")), [
            SavedConnection { name: "Office PC".into(), ..list[0].clone() },
            list[1].clone(),
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
