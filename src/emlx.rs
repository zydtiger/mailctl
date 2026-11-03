//! Locates and reads Mail's `.emlx` message files.
//!
//! A message lives at
//! `<account>/<mailbox>.mbox/<store uuid>/Data/<digits>/Messages/<id>.emlx`,
//! where `<digits>` are the digits of `id / 1000` in reverse order. Files
//! whose attachments were not downloaded end in `.partial.emlx`. The file
//! sits under the mailbox the message currently occupies, which can lag
//! behind the store, so every mailbox of the account is a fallback.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Emlx {
    pub path: PathBuf,
    pub partial: bool,
    /// The RFC 822 message, without the length line and trailing plist.
    pub message: Vec<u8>,
}

pub fn find(root: &Path, account: &str, mailbox: &str, id: i64) -> Option<PathBuf> {
    let account_dir = root.join(account);
    let preferred = mailbox_dir(&account_dir, mailbox);
    if let Some(path) = find_in_mailbox(&preferred, id) {
        return Some(path);
    }
    let mut stack = vec![account_dir];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "mbox")
            {
                if path != preferred
                    && let Some(found) = find_in_mailbox(&path, id)
                {
                    return Some(found);
                }
                stack.push(path);
            }
        }
    }
    None
}

pub fn read(path: &Path) -> Result<Emlx> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let newline = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .context("emlx file has no length line")?;
    let length: usize = std::str::from_utf8(&bytes[..newline])
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .context("emlx file has an invalid length line")?;
    let start = newline + 1;
    let Some(message) = bytes.get(start..start + length) else {
        bail!("emlx file is shorter than its declared length");
    };
    Ok(Emlx {
        path: path.to_path_buf(),
        partial: path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".partial.emlx")),
        message: message.to_vec(),
    })
}

/// `[Gmail]/All Mail` becomes `[Gmail].mbox/All Mail.mbox`.
fn mailbox_dir(account_dir: &Path, mailbox: &str) -> PathBuf {
    mailbox
        .split('/')
        .fold(account_dir.to_path_buf(), |dir, part| {
            dir.join(format!("{part}.mbox"))
        })
}

fn find_in_mailbox(mailbox_dir: &Path, id: i64) -> Option<PathBuf> {
    let relative = relative_dir(id);
    let entries = fs::read_dir(mailbox_dir).ok()?;
    for entry in entries.flatten() {
        let messages = entry.path().join(&relative);
        for name in [format!("{id}.emlx"), format!("{id}.partial.emlx")] {
            let path = messages.join(name);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn relative_dir(id: i64) -> PathBuf {
    let mut path = PathBuf::from("Data");
    let thousands = id / 1000;
    if thousands > 0 {
        for digit in thousands.to_string().chars().rev() {
            path.push(digit.to_string());
        }
    }
    path.join("Messages")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_dir_reverses_thousands_digits() {
        assert_eq!(relative_dir(76034), PathBuf::from("Data/6/7/Messages"));
        assert_eq!(relative_dir(48054), PathBuf::from("Data/8/4/Messages"));
        assert_eq!(relative_dir(123_456), PathBuf::from("Data/3/2/1/Messages"));
        assert_eq!(relative_dir(999), PathBuf::from("Data/Messages"));
    }

    #[test]
    fn finds_and_reads_messages() {
        let root = tempfile::tempdir().unwrap();
        let account = "ACCOUNT";
        let store = "STORE-UUID";
        let message = b"Subject: Hi\r\n\r\nHello\r\n";
        let write = |mailbox: &str, name: &str| {
            let dir = mailbox_dir(&root.path().join(account), mailbox)
                .join(store)
                .join(relative_dir(76034));
            fs::create_dir_all(&dir).unwrap();
            let mut bytes = format!("{}\n", message.len()).into_bytes();
            bytes.extend_from_slice(message);
            bytes.extend_from_slice(b"<?xml version=\"1.0\"?><plist></plist>");
            fs::write(dir.join(name), bytes).unwrap();
        };
        write("[Gmail]/Trash", "76034.partial.emlx");

        // The store still says All Mail; the file has moved to Trash.
        let path = find(root.path(), account, "[Gmail]/All Mail", 76034).unwrap();
        assert!(
            path.ends_with(
                "[Gmail].mbox/Trash.mbox/STORE-UUID/Data/6/7/Messages/76034.partial.emlx"
            )
        );
        let emlx = read(&path).unwrap();
        assert!(emlx.partial);
        assert_eq!(emlx.message, message);
        assert!(find(root.path(), account, "INBOX", 1).is_none());
    }
}
