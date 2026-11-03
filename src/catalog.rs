//! Accounts and mailboxes: Mail.app's account list joined with the store's
//! mailbox table.

use crate::scripting;
use crate::store::{MailboxRecord, Store};
use anyhow::{Context, Result, bail};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

/// Overrides the directory holding the account cache.
pub const CACHE_DIR_ENV: &str = "MAILCTL_CACHE_DIR";

pub const SPECIAL_KINDS: [&str; 6] = ["inbox", "sent", "drafts", "trash", "junk", "archive"];
const GMAIL_ALL_MAIL: &str = "[Gmail]/All Mail";
/// How mailctl names the owner of `local://` mailboxes.
pub const LOCAL_OWNER: &str = "On My Mac";

/// Mail.app's accounts, plus store account ids Mail.app did not list.
#[derive(Debug, Default, Serialize, Deserialize)]
struct AccountCache {
    accounts: Vec<Account>,
    unknown: HashSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub emails: Vec<String>,
    pub enabled: bool,
    /// Special mailbox kind to mailbox path, as Mail.app reports them.
    pub special: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Mailbox {
    pub id: i64,
    /// Account id, or `None` for an On My Mac mailbox.
    pub account_id: Option<String>,
    pub account: Option<String>,
    /// Directory under the Mail data root that holds this mailbox.
    #[serde(skip)]
    pub dir: String,
    pub path: String,
    pub kind: Option<&'static str>,
    pub total: i64,
    pub unread: i64,
    /// Gmail label views such as INBOX draw their messages from All Mail.
    pub label_view: bool,
}

pub struct Catalog {
    pub accounts: Vec<Account>,
    pub mailboxes: Vec<Mailbox>,
}

impl Mailbox {
    /// The account name, the account id when names are unavailable, or
    /// `On My Mac`.
    pub fn owner(&self) -> &str {
        self.account
            .as_deref()
            .or(self.account_id.as_deref())
            .unwrap_or(LOCAL_OWNER)
    }
}

impl Catalog {
    /// Loads the cached account list, refreshing it from Mail.app when the
    /// store names an account the cache has not seen.
    pub fn load(store: &Store, refresh: bool) -> Result<Self> {
        let records = store.mailboxes()?;
        let store_accounts: HashSet<String> = records
            .iter()
            .filter_map(|record| parse_url(&record.url)?.0)
            .collect();
        let mut cache = if refresh { None } else { read_cache() };
        let stale = |cache: &AccountCache| {
            store_accounts.iter().any(|id| {
                !cache.unknown.contains(id)
                    && !cache.accounts.iter().any(|account| &account.id == id)
            })
        };
        if cache.as_ref().is_none_or(stale) {
            match fetch_accounts() {
                Ok(accounts) => {
                    // Ids Mail.app does not list, such as a removed account's
                    // leftover mailboxes, do not trigger another refresh.
                    let unknown = store_accounts
                        .iter()
                        .filter(|id| !accounts.iter().any(|account| &account.id == *id))
                        .cloned()
                        .collect();
                    let fresh = AccountCache { accounts, unknown };
                    if let Err(error) = write_cache(&fresh) {
                        eprintln!("warning: failed to write account cache: {error:#}");
                    }
                    cache = Some(fresh);
                }
                Err(error) if refresh => return Err(error),
                Err(error) => eprintln!("warning: account names unavailable: {error:#}"),
            }
        }
        let accounts = cache.map(|cache| cache.accounts).unwrap_or_default();
        Ok(Self::build(accounts, &records))
    }

    pub fn build(accounts: Vec<Account>, records: &[MailboxRecord]) -> Self {
        let mailboxes = records
            .iter()
            .filter_map(|record| {
                let (account_id, path) = parse_url(&record.url)?;
                let account = account_id
                    .as_ref()
                    .and_then(|id| accounts.iter().find(|account| &account.id == id));
                Some(Mailbox {
                    id: record.id,
                    dir: url_host(&record.url)?.to_string(),
                    account: account.map(|account| account.name.clone()),
                    kind: account.and_then(|account| special_kind(account, &path, records)),
                    account_id,
                    path,
                    total: record.total,
                    unread: record.unread,
                    label_view: record.source.is_some(),
                })
            })
            .collect();
        Self {
            accounts,
            mailboxes,
        }
    }

    pub fn mailbox(&self, id: i64) -> Option<&Mailbox> {
        self.mailboxes.iter().find(|mailbox| mailbox.id == id)
    }

    pub fn account(&self, selector: &str) -> Result<&Account> {
        let matches: Vec<&Account> = self
            .accounts
            .iter()
            .filter(|account| {
                account.id.eq_ignore_ascii_case(selector)
                    || account.name.eq_ignore_ascii_case(selector)
                    || account
                        .emails
                        .iter()
                        .any(|email| email.eq_ignore_ascii_case(selector))
            })
            .collect();
        match matches.as_slice() {
            [account] => Ok(account),
            [] => bail!("no account matches `{selector}`; run `mailctl accounts`"),
            _ => bail!("`{selector}` matches several accounts; use the account id"),
        }
    }

    /// Resolves `--mailbox` values to mailbox ids. `None` means every mailbox.
    pub fn select(&self, specs: &[String], account: Option<&str>) -> Result<Option<Vec<i64>>> {
        let account_id = account
            .map(|selector| self.account(selector).map(|account| account.id.clone()))
            .transpose()?;
        let in_account = |mailbox: &&Mailbox| {
            account_id
                .as_ref()
                .is_none_or(|id| mailbox.account_id.as_ref() == Some(id))
        };
        if specs.iter().any(|spec| spec == "all") {
            return Ok(account_id.as_ref().map(|_| {
                let mut ids: Vec<i64> = self
                    .mailboxes
                    .iter()
                    .filter(in_account)
                    .map(|mailbox| mailbox.id)
                    .collect();
                ids.sort_unstable();
                ids
            }));
        }
        let mut ids = Vec::new();
        for spec in specs {
            // With --account, a numeric spec may also be a mailbox named `2024`.
            let named = account_id
                .as_deref()
                .and_then(|id| self.by_path(spec, Some(id)).ok());
            let found: Vec<i64> = if let Some(mailbox) = named {
                vec![mailbox.id]
            } else if let Ok(id) = spec.parse::<i64>() {
                vec![
                    self.mailbox(id)
                        .with_context(|| format!("no mailbox with id {id}"))?
                        .id,
                ]
            } else if SPECIAL_KINDS.contains(&spec.as_str()) {
                self.mailboxes
                    .iter()
                    .filter(in_account)
                    .filter(|mailbox| mailbox.kind == Some(spec.as_str()))
                    .map(|mailbox| mailbox.id)
                    .collect()
            } else {
                vec![self.by_path(spec, account_id.as_deref())?.id]
            };
            if found.is_empty() {
                bail!("no `{spec}` mailbox found; run `mailctl accounts --refresh`");
            }
            ids.extend(found);
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(Some(ids))
    }

    /// Resolves one destination mailbox for a move.
    pub fn destination(&self, spec: &str, account: Option<&str>) -> Result<&Mailbox> {
        let ids = self
            .select(&[spec.to_string()], account)?
            .context("`all` is not a destination mailbox")?;
        match ids.as_slice() {
            [id] => Ok(self.mailbox(*id).expect("selected mailbox exists")),
            _ => bail!("`{spec}` matches several mailboxes; add --account or use a mailbox id"),
        }
    }

    /// Finds `<account>/<path>`, `On My Mac/<path>`, or `<path>` within
    /// `account_id`.
    fn by_path(&self, spec: &str, account_id: Option<&str>) -> Result<&Mailbox> {
        let candidates: Vec<(Option<String>, &str)> = match account_id {
            Some(id) => vec![(Some(id.to_string()), spec)],
            None => spec
                .match_indices('/')
                .filter_map(|(position, _)| {
                    let (owner, path) = (&spec[..position], &spec[position + 1..]);
                    if owner.eq_ignore_ascii_case(LOCAL_OWNER) {
                        return Some((None, path));
                    }
                    Some((Some(self.account(owner).ok()?.id.clone()), path))
                })
                .collect(),
        };
        for (account, path) in &candidates {
            let in_account = self
                .mailboxes
                .iter()
                .filter(|mailbox| &mailbox.account_id == account);
            let exact = in_account.clone().find(|mailbox| mailbox.path == *path);
            if let Some(mailbox) = exact.or_else(|| {
                let mut folded =
                    in_account.filter(|mailbox| mailbox.path.eq_ignore_ascii_case(path));
                folded.next().filter(|_| folded.next().is_none())
            }) {
                return Ok(mailbox);
            }
        }
        bail!("no mailbox `{spec}`; use `<account>/<path>` or an id from `mailctl mailboxes`")
    }

    /// The mailbox `archive` moves messages to for an account.
    pub fn archive_for(&self, account_id: &str) -> Option<&Mailbox> {
        self.mailboxes.iter().find(|mailbox| {
            mailbox.account_id.as_deref() == Some(account_id) && mailbox.kind == Some("archive")
        })
    }

    pub fn is_gmail(&self, account_id: &str) -> bool {
        self.mailboxes.iter().any(|mailbox| {
            mailbox.account_id.as_deref() == Some(account_id) && mailbox.path == GMAIL_ALL_MAIL
        })
    }

    /// Gmail's All Mail keeps every message outside Trash and Spam, so a
    /// move out of it only adds a label.
    pub fn is_all_mail(&self, id: i64) -> bool {
        self.mailbox(id)
            .is_some_and(|mailbox| mailbox.account_id.is_some() && mailbox.path == GMAIL_ALL_MAIL)
    }
}

fn special_kind(account: &Account, path: &str, records: &[MailboxRecord]) -> Option<&'static str> {
    if let Some((kind, _)) = account.special.iter().find(|(_, special)| *special == path) {
        return SPECIAL_KINDS.iter().copied().find(|known| known == kind);
    }
    let gmail = records.iter().any(|record| {
        parse_url(&record.url).is_some_and(|(id, path)| {
            id.as_deref() == Some(account.id.as_str()) && path == GMAIL_ALL_MAIL
        })
    });
    let archive = if gmail {
        path == GMAIL_ALL_MAIL
    } else {
        path.eq_ignore_ascii_case("Archive")
    };
    archive.then_some("archive")
}

/// Splits a mailbox URL such as `imap://<account>/%5BGmail%5D/All%20Mail`
/// into its account id and decoded path. `local://` mailboxes have no account.
pub fn parse_url(url: &str) -> Option<(Option<String>, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let (account, path) = rest.split_once('/')?;
    let path = percent_decode_str(path).decode_utf8().ok()?.into_owned();
    let account = (scheme != "local").then(|| account.to_string());
    Some((account, path))
}

fn url_host(url: &str) -> Option<&str> {
    url.split_once("://")?.1.split('/').next()
}

fn fetch_accounts() -> Result<Vec<Account>> {
    scripting::call(&json!({ "op": "accounts" }))
}

fn cache_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(CACHE_DIR_ENV) {
        return Some(PathBuf::from(dir).join("accounts.json"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Caches/mailctl/accounts.json"))
}

fn read_cache() -> Option<AccountCache> {
    let text = std::fs::read_to_string(cache_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(cache: &AccountCache) -> Result<()> {
    let path = cache_path().context("HOME is not set")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(cache)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GMAIL: &str = "62E331AB-E11C-4E22-A124-E562194E448F";
    const EWS: &str = "1D8D7FF7-B132-4DAB-96DA-274CD8F5D1CB";

    fn record(id: i64, url: &str, source: Option<i64>) -> MailboxRecord {
        MailboxRecord {
            id,
            url: url.into(),
            total: 0,
            unread: 0,
            source,
        }
    }

    fn account(id: &str, name: &str, special: &[(&str, &str)]) -> Account {
        Account {
            id: id.into(),
            name: name.into(),
            kind: "imap".into(),
            emails: vec![name.into()],
            enabled: true,
            special: special
                .iter()
                .map(|(kind, path)| (kind.to_string(), path.to_string()))
                .collect(),
        }
    }

    fn catalog() -> Catalog {
        let records = vec![
            record(2, &format!("ews://{EWS}/Archive"), None),
            record(6, &format!("ews://{EWS}/Inbox"), None),
            record(4, &format!("ews://{EWS}/Deleted%20Items"), None),
            record(13, &format!("imap://{GMAIL}/%5BGmail%5D/All%20Mail"), None),
            record(14, &format!("imap://{GMAIL}/INBOX"), Some(13)),
            record(19, &format!("imap://{GMAIL}/%5BGmail%5D/Trash"), None),
            record(
                38,
                "local://1DA5C0A9-34E1-4333-9043-967214493D29/Outbox",
                None,
            ),
            record(50, &format!("ews://{EWS}/2024"), None),
        ];
        let accounts = vec![
            account(
                EWS,
                "work@example.edu",
                &[("inbox", "Inbox"), ("trash", "Deleted Items")],
            ),
            account(
                GMAIL,
                "me@gmail.com",
                &[("inbox", "INBOX"), ("trash", "[Gmail]/Trash")],
            ),
        ];
        Catalog::build(accounts, &records)
    }

    #[test]
    fn parses_mailbox_urls() {
        assert_eq!(
            parse_url(&format!("imap://{GMAIL}/%5BGmail%5D/All%20Mail")),
            Some((Some(GMAIL.to_string()), "[Gmail]/All Mail".to_string()))
        );
        assert_eq!(
            parse_url("local://1DA5/Recovered%20Messages%20(a@b.com)"),
            Some((None, "Recovered Messages (a@b.com)".to_string()))
        );
    }

    #[test]
    fn assigns_special_kinds() {
        let catalog = catalog();
        let kind = |id| catalog.mailbox(id).unwrap().kind;
        assert_eq!(kind(6), Some("inbox"));
        assert_eq!(kind(14), Some("inbox"));
        assert_eq!(kind(19), Some("trash"));
        assert_eq!(kind(2), Some("archive"));
        assert_eq!(kind(13), Some("archive"));
        assert_eq!(kind(38), None);
        assert!(catalog.mailbox(14).unwrap().label_view);
        assert!(catalog.is_gmail(GMAIL));
        assert!(!catalog.is_gmail(EWS));
    }

    #[test]
    fn selects_mailboxes() {
        let catalog = catalog();
        let select = |specs: &[&str], account| {
            let specs: Vec<String> = specs.iter().map(|spec| spec.to_string()).collect();
            catalog.select(&specs, account)
        };
        assert_eq!(select(&["inbox"], None).unwrap(), Some(vec![6, 14]));
        assert_eq!(
            select(&["inbox"], Some("me@gmail.com")).unwrap(),
            Some(vec![14])
        );
        assert_eq!(select(&["all"], None).unwrap(), None);
        assert_eq!(
            select(&["all"], Some(EWS)).unwrap(),
            Some(vec![2, 4, 6, 50])
        );
        assert_eq!(
            select(&["me@gmail.com/[Gmail]/All Mail"], None).unwrap(),
            Some(vec![13])
        );
        assert_eq!(
            select(&["work@example.edu/inbox"], None).unwrap(),
            Some(vec![6])
        );
        assert_eq!(
            select(&["Deleted Items"], Some(EWS)).unwrap(),
            Some(vec![4])
        );
        assert_eq!(select(&["19"], None).unwrap(), Some(vec![19]));
        assert_eq!(select(&["2024"], Some(EWS)).unwrap(), Some(vec![50]));
        assert_eq!(select(&["on my mac/Outbox"], None).unwrap(), Some(vec![38]));
        assert!(select(&["Nope"], None).is_err());
        assert!(select(&["99"], None).is_err());
    }

    #[test]
    fn destination_requires_one_mailbox() {
        let catalog = catalog();
        assert!(catalog.destination("inbox", None).is_err());
        assert_eq!(catalog.destination("inbox", Some(GMAIL)).unwrap().id, 14);
        assert_eq!(catalog.archive_for(GMAIL).unwrap().id, 13);
    }
}
