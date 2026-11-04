//! Command results and their human-readable rendering. JSON output
//! serializes the same values.

use crate::catalog::{Account, Mailbox};
use crate::version::VersionInfo;
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;

pub enum Output {
    Version(VersionInfo),
    Accounts(Vec<Account>),
    Mailboxes(Vec<Mailbox>),
    Messages(Vec<MessageView>),
    Message(Box<MessageDetail>),
    Raw(Vec<u8>),
    Actions(ActionReport),
    Composed(ComposeReport),
    Permissions(PermissionReport),
}

#[derive(Debug, Clone, Serialize)]
pub struct AddressView {
    pub name: Option<String>,
    pub address: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageView {
    pub id: i64,
    pub message_id: Option<String>,
    pub account: Option<String>,
    pub account_id: Option<String>,
    pub mailbox: Option<String>,
    pub mailbox_id: i64,
    pub labels: Vec<String>,
    pub subject: String,
    pub from: Option<AddressView>,
    pub to: Vec<AddressView>,
    pub cc: Vec<AddressView>,
    pub date_received: Option<String>,
    pub date_sent: Option<String>,
    pub read: bool,
    pub flagged: bool,
    pub flag_color: Option<&'static str>,
    pub conversation_id: i64,
    pub size: i64,
    pub snippet: Option<String>,
    pub attachments: Vec<String>,
    #[serde(skip)]
    pub received: String,
}

#[derive(Debug, Serialize)]
pub struct MessageDetail {
    #[serde(flatten)]
    pub message: MessageView,
    pub reply_to: Vec<AddressView>,
    pub file: Option<String>,
    /// Attachments were not downloaded; the body may still be complete.
    pub partial: bool,
    pub text: Option<String>,
    pub html: Option<String>,
    pub parts: Vec<AttachmentView>,
}

#[derive(Debug, Serialize)]
pub struct AttachmentView {
    pub name: Option<String>,
    pub content_type: String,
    /// Decoded size; `None` when Mail has not downloaded the attachment.
    pub size: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ActionReport {
    pub action: String,
    pub dry_run: bool,
    pub results: Vec<ActionResult>,
    /// `send` only: ids of drafts with the sent draft's subject that remain
    /// in its Drafts mailbox, such as copies Mail autosaved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leftovers: Option<Vec<i64>>,
}

#[derive(Debug, Serialize)]
pub struct ActionResult {
    pub id: i64,
    /// The id Mail assigned after a move; `None` until the store shows it.
    pub new_id: Option<i64>,
    pub subject: String,
    pub from_mailbox: Option<String>,
    pub to_mailbox: Option<String>,
    pub status: ActionStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Done,
    Planned,
    Skipped,
    /// Mail accepted the move, but the store did not show the result in time.
    Unconfirmed,
    Failed,
}

#[derive(Debug, Serialize)]
pub struct ComposeReport {
    pub action: String,
    pub subject: String,
    pub sender: String,
    pub to: Vec<String>,
    pub sent: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionReport {
    pub binary: PathBuf,
    pub full_disk_access: PermissionStatus,
    /// Permission to control Mail.
    pub automation: PermissionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionStatus {
    Granted,
    Denied,
    NotDetermined,
    /// macOS reports Automation only while Mail is running.
    Unknown,
}

impl PermissionStatus {
    fn name(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::NotDetermined => "not determined",
            Self::Unknown => "unknown (Mail is not running)",
        }
    }
}

impl Output {
    pub fn failed(&self) -> bool {
        match self {
            Self::Actions(report) => report
                .results
                .iter()
                .any(|result| result.status == ActionStatus::Failed),
            Self::Permissions(report) => {
                report.full_disk_access != PermissionStatus::Granted
                    || report.automation != PermissionStatus::Granted
            }
            _ => false,
        }
    }

    pub fn write_json(&self, out: &mut impl Write) -> serde_json::Result<()> {
        match self {
            Self::Version(value) => serde_json::to_writer(&mut *out, value),
            Self::Accounts(value) => serde_json::to_writer(&mut *out, value),
            Self::Mailboxes(value) => serde_json::to_writer(&mut *out, value),
            Self::Messages(value) => serde_json::to_writer(&mut *out, value),
            Self::Message(value) => serde_json::to_writer(&mut *out, value),
            Self::Raw(bytes) => serde_json::to_writer(
                &mut *out,
                &serde_json::json!({ "raw": String::from_utf8_lossy(bytes) }),
            ),
            Self::Actions(value) => serde_json::to_writer(&mut *out, value),
            Self::Composed(value) => serde_json::to_writer(&mut *out, value),
            Self::Permissions(value) => serde_json::to_writer(&mut *out, value),
        }
    }

    pub fn write_human(&self, out: &mut impl Write) -> std::io::Result<()> {
        match self {
            Self::Version(info) => writeln!(out, "{}", info.human()),
            Self::Accounts(accounts) => write_accounts(out, accounts),
            Self::Mailboxes(mailboxes) => write_mailboxes(out, mailboxes),
            Self::Messages(messages) => write_messages(out, messages),
            Self::Message(detail) => write_detail(out, detail),
            Self::Raw(bytes) => out.write_all(bytes),
            Self::Actions(report) => write_actions(out, report),
            Self::Composed(report) => {
                let verb = if report.sent { "Sent" } else { "Saved draft" };
                writeln!(
                    out,
                    "{verb}: {} -> {} (from {})",
                    report.subject,
                    report.to.join(", "),
                    report.sender
                )
            }
            Self::Permissions(report) => {
                writeln!(out, "mailctl:          {}", report.binary.display())?;
                writeln!(out, "Full Disk Access: {}", report.full_disk_access.name())?;
                writeln!(out, "Automation:       {}", report.automation.name())
            }
        }
    }
}

fn write_accounts(out: &mut impl Write, accounts: &[Account]) -> std::io::Result<()> {
    for account in accounts {
        let state = if account.enabled { "" } else { " (disabled)" };
        writeln!(out, "{}{state}", account.name)?;
        writeln!(out, "  id:     {}", account.id)?;
        writeln!(out, "  type:   {}", account.kind)?;
        writeln!(out, "  emails: {}", account.emails.join(", "))?;
        for (kind, path) in &account.special {
            writeln!(out, "  {kind}: {path}")?;
        }
    }
    Ok(())
}

fn write_mailboxes(out: &mut impl Write, mailboxes: &[Mailbox]) -> std::io::Result<()> {
    writeln!(
        out,
        "{:>5}  {:>7}  {:>6}  {:<8}  MAILBOX",
        "ID", "TOTAL", "UNREAD", "KIND"
    )?;
    for mailbox in mailboxes {
        writeln!(
            out,
            "{:>5}  {:>7}  {:>6}  {:<8}  {}/{}",
            mailbox.id,
            mailbox.total,
            mailbox.unread,
            mailbox.kind.unwrap_or(""),
            mailbox.owner(),
            mailbox.path
        )?;
    }
    Ok(())
}

fn write_messages(out: &mut impl Write, messages: &[MessageView]) -> std::io::Result<()> {
    if messages.is_empty() {
        return writeln!(out, "No messages.");
    }
    for message in messages {
        let marks = format!(
            "{}{}{}",
            if message.read { ' ' } else { '*' },
            if message.flagged { 'F' } else { ' ' },
            if message.attachments.is_empty() {
                ' '
            } else {
                '@'
            },
        );
        let from = message
            .from
            .as_ref()
            .map(|from| {
                from.name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| from.address.clone())
            })
            .unwrap_or_default();
        writeln!(
            out,
            "{:>6}  {}  {marks}  {:<28}  {}",
            message.id,
            message.received,
            truncate(&from, 28),
            message.subject
        )?;
    }
    Ok(())
}

fn write_detail(out: &mut impl Write, detail: &MessageDetail) -> std::io::Result<()> {
    let message = &detail.message;
    writeln!(out, "Id:       {}", message.id)?;
    if let Some(from) = &message.from {
        writeln!(out, "From:     {}", display_address(from))?;
    }
    write_address_line(out, "To:", &message.to)?;
    write_address_line(out, "Cc:", &message.cc)?;
    write_address_line(out, "Reply-To:", &detail.reply_to)?;
    writeln!(out, "Date:     {}", message.received)?;
    writeln!(out, "Subject:  {}", message.subject)?;
    let mailbox = message.mailbox.as_deref().unwrap_or("?");
    let account = message
        .account
        .as_deref()
        .or(message.account_id.as_deref())
        .unwrap_or(crate::catalog::LOCAL_OWNER);
    writeln!(
        out,
        "Mailbox:  {account}/{mailbox} ({})",
        message.mailbox_id
    )?;
    if !message.labels.is_empty() {
        writeln!(out, "Labels:   {}", message.labels.join(", "))?;
    }
    let mut status = vec![if message.read { "read" } else { "unread" }];
    if message.flagged {
        status.push(message.flag_color.unwrap_or("flagged"));
    }
    writeln!(out, "Status:   {}", status.join(", "))?;
    for part in &detail.parts {
        let size = part
            .size
            .map(|size| format!("{size} bytes"))
            .unwrap_or_else(|| "not downloaded".into());
        writeln!(
            out,
            "Attach:   {} ({}, {size})",
            part.name.as_deref().unwrap_or("(unnamed)"),
            part.content_type,
        )?;
    }
    if detail.partial && detail.parts.is_empty() && !message.attachments.is_empty() {
        writeln!(
            out,
            "Attach:   {} (not downloaded)",
            message.attachments.join(", ")
        )?;
    }
    writeln!(out)?;
    match (&detail.html, &detail.text) {
        (Some(html), _) => writeln!(out, "{html}"),
        (None, Some(text)) => writeln!(out, "{}", text.trim_end()),
        (None, None) => writeln!(out, "(message body is not stored locally)"),
    }
}

fn write_address_line(
    out: &mut impl Write,
    label: &str,
    addresses: &[AddressView],
) -> std::io::Result<()> {
    if addresses.is_empty() {
        return Ok(());
    }
    let joined = addresses
        .iter()
        .map(display_address)
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "{label:<10}{joined}")
}

fn write_actions(out: &mut impl Write, report: &ActionReport) -> std::io::Result<()> {
    for result in &report.results {
        let status = match result.status {
            ActionStatus::Done => "done",
            ActionStatus::Planned => "would",
            ActionStatus::Skipped => "skipped",
            ActionStatus::Unconfirmed => "unconfirmed",
            ActionStatus::Failed => "failed",
        };
        let route = match (&result.from_mailbox, &result.to_mailbox) {
            (Some(from), Some(to)) => format!(" [{from} -> {to}]"),
            (Some(from), None) => format!(" [{from}]"),
            _ => String::new(),
        };
        let renamed = result
            .new_id
            .map(|id| format!(" (now {id})"))
            .unwrap_or_default();
        write!(
            out,
            "{status:<7} {} {:>6}  {}{route}{renamed}",
            report.action, result.id, result.subject
        )?;
        match &result.error {
            Some(error) => writeln!(out, ": {error}")?,
            None => writeln!(out)?,
        }
    }
    if let Some(leftovers) = report.leftovers.as_ref().filter(|ids| !ids.is_empty()) {
        let ids: Vec<String> = leftovers.iter().map(i64::to_string).collect();
        writeln!(out, "Drafts with the same subject: {}", ids.join(" "))?;
    }
    Ok(())
}

fn display_address(address: &AddressView) -> String {
    match address.name.as_deref().filter(|name| !name.is_empty()) {
        Some(name) => format!("{name} <{}>", address.address),
        None => address.address.clone(),
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut shortened: String = text.chars().take(width - 1).collect();
    shortened.push('…');
    shortened
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_leftover_drafts() {
        let report = ActionReport {
            action: "send".into(),
            dry_run: false,
            results: Vec::new(),
            leftovers: Some(vec![76298, 76299]),
        };
        let mut out = Vec::new();
        write_actions(&mut out, &report).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Drafts with the same subject: 76298 76299\n"
        );
    }

    #[test]
    fn truncates_by_characters() {
        assert_eq!(truncate("短文本", 5), "短文本");
        assert_eq!(truncate("abcdefgh", 5), "abcd…");
    }
}
