//! Read-only access to Mail's `Envelope Index` SQLite database.

use anyhow::{Context, Result, bail};
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, params_from_iter};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Overrides the versioned Mail directory, such as `~/Library/Mail/V10`.
pub const MAIL_DIR_ENV: &str = "MAILCTL_MAIL_DIR";

pub struct Store {
    conn: Connection,
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct MailboxRecord {
    pub id: i64,
    pub url: String,
    pub total: i64,
    pub unread: i64,
    /// The mailbox a Gmail label view draws its messages from.
    pub source: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AddressRecord {
    pub address: String,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub id: i64,
    pub message_id: Option<String>,
    pub mailbox: i64,
    pub subject: String,
    pub from: Option<AddressRecord>,
    pub date_sent: Option<i64>,
    pub date_received: i64,
    pub read: bool,
    pub flagged: bool,
    /// Mail's flag index (0 red to 6 gray) while the message is flagged.
    pub flag_color: Option<i64>,
    pub conversation_id: i64,
    pub size: i64,
    pub snippet: Option<String>,
    pub labels: Vec<i64>,
    pub to: Vec<AddressRecord>,
    pub cc: Vec<AddressRecord>,
    pub attachments: Vec<String>,
}

#[derive(Debug, Default)]
pub struct MessageQuery {
    /// Restrict to messages stored in or labelled with these mailboxes.
    pub mailboxes: Option<Vec<i64>>,
    pub unread: bool,
    pub flagged: bool,
    pub from: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    /// Matches subject, sender, or snippet.
    pub text: Option<String>,
    pub since: Option<i64>,
    pub before: Option<i64>,
    pub conversation: Option<i64>,
    pub limit: Option<u32>,
}

/// The subject as Mail shows it, with any `Re:`-style prefix.
const SUBJECT: &str = "(COALESCE(m.subject_prefix, '') || COALESCE(s.subject, ''))";

const MESSAGE_COLUMNS: &str = "
    m.ROWID, g.message_id_header, m.mailbox,
    COALESCE(m.subject_prefix, '') || COALESCE(s.subject, ''),
    a.address, a.comment, m.date_sent, COALESCE(m.date_received, 0),
    m.read, m.flagged, m.flags, m.conversation_id, m.size, su.summary
FROM messages m
LEFT JOIN subjects s ON s.ROWID = m.subject
LEFT JOIN addresses a ON a.ROWID = m.sender
LEFT JOIN summaries su ON su.ROWID = m.summary
LEFT JOIN message_global_data g ON g.ROWID = m.global_message_id";

impl Store {
    pub fn open() -> Result<Self> {
        let root = mail_dir()?;
        Self::open_at(&root)
    }

    pub fn open_at(root: &Path) -> Result<Self> {
        let path = root.join("MailData").join("Envelope Index");
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("failed to open {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self {
            conn,
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn mailboxes(&self) -> Result<Vec<MailboxRecord>> {
        let mut statement = self.conn.prepare(
            "SELECT ROWID, url, total_count, unread_count, source FROM mailboxes ORDER BY ROWID",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(MailboxRecord {
                id: row.get(0)?,
                url: row.get(1)?,
                total: row.get(2)?,
                unread: row.get(3)?,
                source: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    pub fn messages(&self, query: &MessageQuery) -> Result<Vec<MessageRecord>> {
        let mut filter = Filter::default();
        filter.clauses.push("m.deleted = 0".into());

        if let Some(mailboxes) = &query.mailboxes {
            if mailboxes.is_empty() {
                return Ok(Vec::new());
            }
            let list = mailboxes
                .iter()
                .map(|id| filter.bind(Value::Integer(*id)))
                .collect::<Vec<_>>()
                .join(", ");
            filter.clauses.push(format!(
                "(m.mailbox IN ({list}) OR m.ROWID IN \
                 (SELECT message_id FROM labels WHERE mailbox_id IN ({list})))"
            ));
        }
        if query.unread {
            filter.clauses.push("m.read = 0".into());
        }
        if query.flagged {
            filter.clauses.push("m.flagged = 1".into());
        }
        if let Some(text) = &query.from {
            let p = filter.bind(like(text));
            filter.clauses.push(format!(
                "(a.address LIKE {p} ESCAPE '\\' OR a.comment LIKE {p} ESCAPE '\\')"
            ));
        }
        if let Some(text) = &query.to {
            let p = filter.bind(like(text));
            filter.clauses.push(format!(
                "EXISTS (SELECT 1 FROM recipients r JOIN addresses ra ON ra.ROWID = r.address \
                 WHERE r.message = m.ROWID AND r.type IN (0, 1) \
                 AND (ra.address LIKE {p} ESCAPE '\\' OR ra.comment LIKE {p} ESCAPE '\\'))"
            ));
        }
        if let Some(text) = &query.subject {
            let p = filter.bind(like(text));
            filter
                .clauses
                .push(format!("{SUBJECT} LIKE {p} ESCAPE '\\'"));
        }
        if let Some(text) = &query.text {
            let p = filter.bind(like(text));
            filter.clauses.push(format!(
                "({SUBJECT} LIKE {p} ESCAPE '\\' OR a.address LIKE {p} ESCAPE '\\' \
                 OR a.comment LIKE {p} ESCAPE '\\' OR su.summary LIKE {p} ESCAPE '\\')"
            ));
        }
        if let Some(since) = query.since {
            let p = filter.bind(Value::Integer(since));
            filter.clauses.push(format!("m.date_received >= {p}"));
        }
        if let Some(before) = query.before {
            let p = filter.bind(Value::Integer(before));
            filter.clauses.push(format!("m.date_received < {p}"));
        }
        if let Some(conversation) = query.conversation {
            let p = filter.bind(Value::Integer(conversation));
            filter.clauses.push(format!("m.conversation_id = {p}"));
        }

        let order = if query.conversation.is_some() {
            "ASC"
        } else {
            "DESC"
        };
        let limit = query
            .limit
            .map(|limit| format!(" LIMIT {limit}"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} WHERE {} ORDER BY m.date_received {order}, m.ROWID {order}{limit}",
            filter.clauses.join(" AND ")
        );
        self.load(&sql, filter.params)
    }

    pub fn messages_by_id(&self, ids: &[i64]) -> Result<Vec<MessageRecord>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} WHERE m.ROWID IN ({})",
            placeholders(ids.len())
        );
        let found = self.load(&sql, ids.iter().map(|id| Value::Integer(*id)).collect())?;
        let by_id: HashMap<i64, MessageRecord> = found
            .into_iter()
            .map(|record| (record.id, record))
            .collect();
        ids.iter()
            .map(|id| {
                by_id
                    .get(id)
                    .cloned()
                    .with_context(|| format!("no message with id {id}"))
            })
            .collect()
    }

    /// Every stored copy of a message, by its Message-ID header, with the
    /// mailboxes it is stored in or labelled with.
    pub fn copies(&self, message_id: &str) -> Result<Vec<(i64, Vec<i64>)>> {
        let mut statement = self.conn.prepare(
            "SELECT m.ROWID, m.mailbox, (SELECT group_concat(mailbox_id) FROM labels WHERE message_id = m.ROWID) \
             FROM messages m JOIN message_global_data g ON g.ROWID = m.global_message_id \
             WHERE g.message_id_header = ?1 AND m.deleted = 0",
        )?;
        let rows = statement.query_map([message_id], |row| {
            let mut mailboxes = vec![row.get::<_, i64>(1)?];
            if let Some(labels) = row.get::<_, Option<String>>(2)? {
                mailboxes.extend(labels.split(',').filter_map(|id| id.parse::<i64>().ok()));
            }
            Ok((row.get(0)?, mailboxes))
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
    }

    fn load(&self, sql: &str, params: Vec<Value>) -> Result<Vec<MessageRecord>> {
        let mut statement = self.conn.prepare(sql)?;
        let rows = statement.query_map(params_from_iter(params), |row| {
            let address: Option<String> = row.get(4)?;
            let name: Option<String> = row.get(5)?;
            let flagged = row.get::<_, i64>(9)? != 0;
            let flags: i64 = row.get(10)?;
            Ok(MessageRecord {
                id: row.get(0)?,
                message_id: row
                    .get::<_, Option<String>>(1)?
                    .filter(|id| !id.trim().is_empty()),
                mailbox: row.get(2)?,
                subject: row.get(3)?,
                from: address.map(|address| AddressRecord {
                    address,
                    name: name.unwrap_or_default(),
                }),
                date_sent: row.get(6)?,
                date_received: row.get(7)?,
                read: row.get::<_, i64>(8)? != 0,
                flagged,
                flag_color: flagged.then_some(flag_color(flags)),
                conversation_id: row.get(11)?,
                size: row.get(12)?,
                snippet: row.get(13)?,
                labels: Vec::new(),
                to: Vec::new(),
                cc: Vec::new(),
                attachments: Vec::new(),
            })
        })?;
        let mut records = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        self.attach_details(&mut records)?;
        Ok(records)
    }

    fn attach_details(&self, records: &mut [MessageRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let ids: Vec<Value> = records
            .iter()
            .map(|record| Value::Integer(record.id))
            .collect();
        let list = placeholders(ids.len());
        let index: HashMap<i64, usize> = records
            .iter()
            .enumerate()
            .map(|(position, record)| (record.id, position))
            .collect();

        let mut statement = self.conn.prepare(&format!(
            "SELECT r.message, r.type, a.address, a.comment FROM recipients r \
             JOIN addresses a ON a.ROWID = r.address \
             WHERE r.message IN ({list}) ORDER BY r.message, r.type, r.position"
        ))?;
        let mut rows = statement.query(params_from_iter(ids.iter()))?;
        while let Some(row) = rows.next()? {
            let record = &mut records[index[&row.get::<_, i64>(0)?]];
            let address = AddressRecord {
                address: row.get(2)?,
                name: row.get(3)?,
            };
            match row.get::<_, Option<i64>>(1)? {
                Some(1) => record.cc.push(address),
                Some(0) | None => record.to.push(address),
                _ => {}
            }
        }

        let mut statement = self.conn.prepare(&format!(
            "SELECT message_id, mailbox_id FROM labels WHERE message_id IN ({list}) ORDER BY mailbox_id"
        ))?;
        let mut rows = statement.query(params_from_iter(ids.iter()))?;
        while let Some(row) = rows.next()? {
            records[index[&row.get::<_, i64>(0)?]]
                .labels
                .push(row.get(1)?);
        }

        let mut statement = self.conn.prepare(&format!(
            "SELECT message, name FROM attachments WHERE message IN ({list}) ORDER BY ROWID"
        ))?;
        let mut rows = statement.query(params_from_iter(ids.iter()))?;
        while let Some(row) = rows.next()? {
            if let Some(name) = row.get::<_, Option<String>>(1)? {
                records[index[&row.get::<_, i64>(0)?]]
                    .attachments
                    .push(name);
            }
        }
        Ok(())
    }
}

/// Finds the newest versioned Mail directory, such as `~/Library/Mail/V10`.
fn mail_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(MAIL_DIR_ENV) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let base = PathBuf::from(home).join("Library").join("Mail");
    let entries = match std::fs::read_dir(&base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => bail!(
            "cannot read {}: mailctl ({}) needs Full Disk Access; run `mailctl permissions`",
            base.display(),
            std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "this binary".into())
        ),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", base.display()));
        }
    };
    entries
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let version: u32 = name.strip_prefix('V')?.parse().ok()?;
            Some((version, base.join(name)))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, path)| path)
        .with_context(|| format!("no Mail data directory under {}", base.display()))
}

/// Mail keeps the flag index in bits 39 to 41 of `messages.flags`; the
/// `flag_color` column does not follow it.
fn flag_color(flags: i64) -> i64 {
    (flags >> 39) & 7
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

/// WHERE clauses with numbered parameters bound in order.
#[derive(Default)]
struct Filter {
    clauses: Vec<String>,
    params: Vec<Value>,
}

impl Filter {
    fn bind(&mut self, value: Value) -> String {
        self.params.push(value);
        format!("?{}", self.params.len())
    }
}

fn like(text: &str) -> Value {
    let mut pattern = String::from("%");
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    Value::Text(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The subset of the Envelope Index schema mailctl reads.
    const SCHEMA: &str = "
        CREATE TABLE mailboxes (ROWID INTEGER PRIMARY KEY, url TEXT NOT NULL,
            total_count INTEGER NOT NULL DEFAULT 0, unread_count INTEGER NOT NULL DEFAULT 0, source INTEGER);
        CREATE TABLE subjects (ROWID INTEGER PRIMARY KEY, subject TEXT NOT NULL);
        CREATE TABLE addresses (ROWID INTEGER PRIMARY KEY, address TEXT NOT NULL, comment TEXT NOT NULL);
        CREATE TABLE summaries (ROWID INTEGER PRIMARY KEY, summary TEXT NOT NULL);
        CREATE TABLE message_global_data (ROWID INTEGER PRIMARY KEY, message_id_header TEXT);
        CREATE TABLE messages (ROWID INTEGER PRIMARY KEY, global_message_id INTEGER NOT NULL,
            sender INTEGER, subject_prefix TEXT, subject INTEGER NOT NULL, summary INTEGER,
            date_sent INTEGER, date_received INTEGER, mailbox INTEGER NOT NULL,
            flags INTEGER NOT NULL DEFAULT 0, read INTEGER NOT NULL DEFAULT 0,
            flagged INTEGER NOT NULL DEFAULT 0, deleted INTEGER NOT NULL DEFAULT 0,
            size INTEGER NOT NULL DEFAULT 0, conversation_id INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE labels (message_id INTEGER, mailbox_id INTEGER, PRIMARY KEY(message_id, mailbox_id));
        CREATE TABLE recipients (ROWID INTEGER PRIMARY KEY, message INTEGER NOT NULL,
            address INTEGER NOT NULL, type INTEGER, position INTEGER);
        CREATE TABLE attachments (ROWID INTEGER PRIMARY KEY, message INTEGER NOT NULL,
            attachment_id TEXT, name TEXT);

        INSERT INTO mailboxes VALUES
            (6, 'ews://EWS/Inbox', 2, 1, NULL),
            (13, 'imap://GMAIL/%5BGmail%5D/All%20Mail', 2, 0, NULL),
            (14, 'imap://GMAIL/INBOX', 1, 0, 13);
        INSERT INTO subjects VALUES (1, 'Lease extension'), (2, '50% off today'), (3, 'Weekly digest');
        INSERT INTO addresses VALUES (1, 'landlord@example.com', 'Michelle Miller'),
            (2, 'deals@shop.example', 'Shop'), (3, 'me@example.edu', 'Me');
        INSERT INTO summaries VALUES (1, 'The unit 1415 lease runs through May');
        INSERT INTO message_global_data VALUES (1, '<lease@example.com>'), (2, '<deal@shop>'), (3, '<digest@x>');
        INSERT INTO messages VALUES
            (100, 1, 1, 'Re: ', 1, 1, 1000, 1001, 6, 0, 0, 0, 0, 10, 7),
            (101, 2, 2, NULL, 2, NULL, 2000, 2001, 13, 2207613451408, 1, 1, 0, 20, 8),
            (102, 3, 2, NULL, 3, NULL, 3000, 3001, 13, 0, 1, 0, 0, 30, 9),
            (103, 1, 3, 'Re: ', 1, NULL, 4000, 4001, 6, 0, 1, 0, 1, 40, 7);
        INSERT INTO labels VALUES (101, 14);
        INSERT INTO recipients VALUES (1, 100, 3, 0, 0), (2, 100, 2, 1, 0);
        INSERT INTO attachments VALUES (1, 100, '2', 'lease.pdf');
    ";

    fn fixture() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("MailData")).unwrap();
        Connection::open(root.path().join("MailData/Envelope Index"))
            .unwrap()
            .execute_batch(SCHEMA)
            .unwrap();
        let store = Store::open_at(root.path()).unwrap();
        (root, store)
    }

    fn query(store: &Store, query: MessageQuery) -> Vec<i64> {
        store
            .messages(&query)
            .unwrap()
            .iter()
            .map(|record| record.id)
            .collect()
    }

    #[test]
    fn reads_message_records() {
        let (_root, store) = fixture();
        let [lease] = store.messages_by_id(&[100]).unwrap().try_into().unwrap();
        assert_eq!(lease.subject, "Re: Lease extension");
        assert_eq!(lease.message_id.as_deref(), Some("<lease@example.com>"));
        assert_eq!(lease.from.unwrap().name, "Michelle Miller");
        assert_eq!(lease.to[0].address, "me@example.edu");
        assert_eq!(lease.cc[0].address, "deals@shop.example");
        assert_eq!(lease.attachments, vec!["lease.pdf"]);
        assert_eq!(
            lease.snippet.as_deref(),
            Some("The unit 1415 lease runs through May")
        );
        assert!(!lease.read);

        assert_eq!(store.messages_by_id(&[100, 100]).unwrap().len(), 2);
        let prefixed = MessageQuery {
            subject: Some("re: lease".into()),
            ..MessageQuery::default()
        };
        assert_eq!(query(&store, prefixed), vec![100]);

        let [deal] = store.messages_by_id(&[101]).unwrap().try_into().unwrap();
        assert_eq!(deal.labels, vec![14]);
        assert_eq!(deal.flag_color, Some(4));
        assert!(store.messages_by_id(&[999]).is_err());
    }

    #[test]
    fn filters_messages() {
        let (_root, store) = fixture();
        let all = MessageQuery::default;
        let none: Vec<i64> = Vec::new();

        // Newest first; deleted rows never appear.
        assert_eq!(query(&store, all()), vec![102, 101, 100]);
        // A Gmail label view matches labelled messages stored in All Mail.
        let inbox = Some(vec![6, 14]);
        let scoped = || MessageQuery {
            mailboxes: inbox.clone(),
            ..all()
        };
        assert_eq!(query(&store, scoped()), vec![101, 100]);
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    limit: Some(1),
                    ..scoped()
                }
            ),
            vec![101]
        );
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    mailboxes: Some(vec![]),
                    ..all()
                }
            ),
            none
        );
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    unread: true,
                    ..all()
                }
            ),
            vec![100]
        );
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    flagged: true,
                    ..all()
                }
            ),
            vec![101]
        );
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    from: Some("michelle".into()),
                    ..all()
                }
            ),
            vec![100]
        );
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    to: Some("shop".into()),
                    ..all()
                }
            ),
            vec![100]
        );
        // `%` is literal, not a wildcard.
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    subject: Some("50%".into()),
                    ..all()
                }
            ),
            vec![101]
        );
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    subject: Some("5%f".into()),
                    ..all()
                }
            ),
            none
        );
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    text: Some("unit 1415".into()),
                    ..all()
                }
            ),
            vec![100]
        );
        let window = MessageQuery {
            since: Some(2001),
            before: Some(3001),
            ..all()
        };
        assert_eq!(query(&store, window), vec![101]);
        assert_eq!(
            query(
                &store,
                MessageQuery {
                    conversation: Some(7),
                    ..all()
                }
            ),
            vec![100]
        );
    }

    #[test]
    fn finds_copies_by_message_id() {
        let (_root, store) = fixture();
        assert_eq!(
            store.copies("<deal@shop>").unwrap(),
            vec![(101, vec![13, 14])]
        );
        // The deleted copy in the lease thread is ignored.
        assert_eq!(
            store.copies("<lease@example.com>").unwrap(),
            vec![(100, vec![6])]
        );
        assert!(store.copies("<missing@x>").unwrap().is_empty());
    }

    #[test]
    fn decodes_flag_color_bits() {
        // Observed values for flag index 0, 1, 4, and 6 on one message.
        assert_eq!(flag_color(8_590_195_856), 0);
        assert_eq!(flag_color(558_346_009_744), 1);
        assert_eq!(flag_color(2_207_613_451_408), 4);
        assert_eq!(flag_color(3_307_125_079_184), 6);
    }

    #[test]
    fn like_escapes_wildcards() {
        assert_eq!(like("50%_off\\"), Value::Text("%50\\%\\_off\\\\%".into()));
    }
}
