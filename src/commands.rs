use crate::catalog::{Catalog, Mailbox, SPECIAL_KINDS};
use crate::cli::{Body, Command, Filters, FlagColor, Targets};
use crate::output::{
    ActionReport, ActionResult, ActionStatus, AddressView, AttachmentView, ComposeReport,
    MessageDetail, MessageView, Output,
};
use crate::store::{AddressRecord, MessageQuery, MessageRecord, Store};
use crate::{dates, emlx, scripting, version};
use anyhow::{Context, Result};
use chrono::Local;
use mail_parser::{MessageParser, html_to_text};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::time::{Duration, Instant};

pub fn run(command: Command) -> Result<Output> {
    let command = match command {
        Command::Version => return Ok(Output::Version(version::info())),
        // Composing needs neither the store nor the account catalog.
        Command::Draft {
            to,
            cc,
            bcc,
            subject,
            body,
            from,
            send,
        } => {
            return compose(
                "draft",
                json!({
                    "op": "compose", "to": to, "cc": cc, "bcc": bcc, "subject": subject,
                    "body": read_body(&body)?, "sender": from, "send": send,
                }),
            );
        }
        command => command,
    };
    let store = Store::open()?;
    let refresh = matches!(command, Command::Accounts { refresh: true });
    let catalog = Catalog::load(&store, refresh)?;
    let session = Session { store, catalog };

    match command {
        Command::Version | Command::Completions { .. } | Command::Draft { .. } => {
            unreachable!("handled earlier")
        }
        Command::Accounts { .. } => Ok(Output::Accounts(session.catalog.accounts.clone())),
        Command::Mailboxes { account } => session.mailboxes(account.as_deref()),
        Command::List(filters) => session.list(&filters, None),
        Command::Search { query, filters } => session.list(&filters, Some(query)),
        Command::Show { id, html, raw } => session.show(id, html, raw),
        Command::Thread { id } => session.thread(id),
        Command::Read(targets) => session.act(Action::Read, &targets),
        Command::Unread(targets) => session.act(Action::Unread, &targets),
        Command::Flag { targets, color } => session.act(Action::Flag(color), &targets),
        Command::Unflag(targets) => session.act(Action::Unflag, &targets),
        Command::Move {
            targets,
            to,
            account,
        } => {
            // Without --account, a special name means each message's own account.
            let destination = match SPECIAL_KINDS.iter().find(|kind| **kind == to) {
                Some(kind) if account.is_none() => MoveTo::Special(kind),
                _ => MoveTo::Mailbox(session.catalog.destination(&to, account.as_deref())?.id),
            };
            session.act(Action::Move(destination), &targets)
        }
        Command::Archive(targets) => session.act(Action::Archive, &targets),
        Command::Trash(targets) => session.act(Action::Trash, &targets),
        Command::Reply {
            id,
            all,
            body,
            send,
        } => {
            let target = session.location(&session.message(id)?)?;
            compose(
                "reply",
                json!({ "op": "reply", "target": target, "all": all, "body": read_body(&body)?, "send": send }),
            )
        }
        Command::Forward { id, to, body, send } => {
            let target = session.location(&session.message(id)?)?;
            compose(
                "forward",
                json!({ "op": "forward", "target": target, "to": to, "body": read_body(&body)?, "send": send }),
            )
        }
    }
}

struct Session {
    store: Store,
    catalog: Catalog,
}

/// A message as Mail.app's scripting addresses it.
#[derive(Debug, Clone, Serialize)]
struct Location {
    id: i64,
    /// Account id, or `null` for an On My Mac mailbox.
    account: Option<String>,
    mailbox: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Read,
    Unread,
    Flag(FlagColor),
    Unflag,
    Move(MoveTo),
    Archive,
    Trash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MoveTo {
    Mailbox(i64),
    Special(&'static str),
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Unread => "unread",
            Self::Flag(_) => "flag",
            Self::Unflag => "unflag",
            Self::Move(_) | Self::Archive => "move",
            Self::Trash => "trash",
        }
    }
}

/// What one message needs: the mailbox to address it through and, for
/// moves, the destination.
struct Step {
    record: MessageRecord,
    source: Option<i64>,
    destination: Option<i64>,
    outcome: Result<(), Skip>,
    /// Ids already carrying the message's Message-ID before the write, so a
    /// pre-existing duplicate is not mistaken for the moved copy.
    prior: Vec<i64>,
}

enum Skip {
    Already(String),
    Refused(String),
}

impl Session {
    fn mailboxes(&self, account: Option<&str>) -> Result<Output> {
        let account_id = account
            .map(|selector| self.catalog.account(selector))
            .transpose()?;
        let mailboxes = self
            .catalog
            .mailboxes
            .iter()
            .filter(|mailbox| {
                account_id.is_none_or(|account| mailbox.account_id.as_deref() == Some(&account.id))
            })
            .cloned()
            .collect();
        Ok(Output::Mailboxes(mailboxes))
    }

    fn list(&self, filters: &Filters, text: Option<String>) -> Result<Output> {
        let default_scope = if text.is_some() { "all" } else { "inbox" };
        let specs = if filters.mailbox.is_empty() {
            vec![default_scope.to_string()]
        } else {
            filters.mailbox.clone()
        };
        let now = Local::now();
        let query = MessageQuery {
            mailboxes: self.catalog.select(&specs, filters.account.as_deref())?,
            unread: filters.unread,
            flagged: filters.flagged,
            from: filters.from.clone(),
            to: filters.to.clone(),
            subject: filters.subject.clone(),
            text,
            since: filters
                .since
                .as_deref()
                .map(|value| dates::parse_time(value, now))
                .transpose()?,
            before: filters
                .before
                .as_deref()
                .map(|value| dates::parse_time(value, now))
                .transpose()?,
            conversation: None,
            limit: Some(filters.limit),
        };
        let records = self.store.messages(&query)?;
        Ok(Output::Messages(
            records.iter().map(|record| self.view(record)).collect(),
        ))
    }

    fn thread(&self, id: i64) -> Result<Output> {
        let record = self.message(id)?;
        let records = self.store.messages(&MessageQuery {
            conversation: Some(record.conversation_id),
            ..MessageQuery::default()
        })?;
        Ok(Output::Messages(
            records.iter().map(|record| self.view(record)).collect(),
        ))
    }

    fn show(&self, id: i64, html: bool, raw: bool) -> Result<Output> {
        let record = self.message(id)?;
        let mailbox = self.mailbox_of(&record)?;
        let file = emlx::find(self.store.root(), &mailbox.dir, &mailbox.path, id)
            .map(|path| emlx::read(&path))
            .transpose()?;
        if raw {
            let file = file.context("the message source is not stored locally")?;
            return Ok(Output::Raw(file.message));
        }
        let mut detail = MessageDetail {
            message: self.view(&record),
            reply_to: Vec::new(),
            file: None,
            partial: false,
            text: None,
            html: None,
            parts: Vec::new(),
        };
        if let Some(file) = file {
            detail.file = Some(file.path.display().to_string());
            detail.partial = file.partial;
            let parsed = MessageParser::default()
                .parse(&file.message)
                .context("failed to parse the message source")?;
            detail.reply_to = parsed
                .reply_to()
                .map(|addresses| {
                    addresses
                        .mailboxes()
                        .filter_map(|address| {
                            Some(AddressView {
                                name: address.name().map(str::to_string),
                                address: address.address()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if html {
                detail.html = parsed.body_html(0).map(|body| body.into_owned());
            }
            // Mail's own drafts carry an empty text/plain part beside the HTML.
            detail.text = parsed
                .body_text(0)
                .filter(|body| !body.trim().is_empty())
                .or_else(|| parsed.body_html(0).map(|html| html_to_text(&html).into()))
                .map(|body| body.into_owned());
            detail.parts = parsed
                .attachments()
                .map(|part| AttachmentView {
                    name: part.attachment_name().map(str::to_string),
                    content_type: part
                        .content_type()
                        .map(|kind| match kind.subtype() {
                            Some(subtype) => format!("{}/{subtype}", kind.ctype()),
                            None => kind.ctype().to_string(),
                        })
                        .unwrap_or_else(|| "application/octet-stream".into()),
                    size: Some(part.decoded_len()).filter(|size| *size > 0 || !file.partial),
                })
                .collect();
        }
        Ok(Output::Message(Box::new(detail)))
    }

    fn act(&self, action: Action, targets: &Targets) -> Result<Output> {
        let mut ids = targets.ids.clone();
        let mut seen = HashSet::new();
        ids.retain(|id| seen.insert(*id));
        let records = self.store.messages_by_id(&ids)?;
        let mut steps = records
            .into_iter()
            .map(|record| self.plan(action, record))
            .collect::<Result<Vec<_>>>()?;
        let moves = matches!(action, Action::Move(_) | Action::Archive | Action::Trash);
        if moves && !targets.dry_run {
            for step in steps.iter_mut().filter(|step| step.outcome.is_ok()) {
                if let Some(message_id) = &step.record.message_id {
                    step.prior = self
                        .store
                        .copies(message_id)?
                        .into_iter()
                        .map(|(id, _)| id)
                        .collect();
                }
            }
        }

        let mut results: Vec<ActionResult> = steps
            .iter()
            .map(|step| ActionResult {
                id: step.record.id,
                new_id: None,
                subject: step.record.subject.clone(),
                from_mailbox: step.source.and_then(|id| self.label(id)),
                to_mailbox: step.destination.and_then(|id| self.label(id)),
                status: match &step.outcome {
                    Ok(()) if targets.dry_run => ActionStatus::Planned,
                    Ok(()) => ActionStatus::Done,
                    Err(Skip::Already(_)) => ActionStatus::Skipped,
                    Err(Skip::Refused(_)) => ActionStatus::Failed,
                },
                error: match &step.outcome {
                    Ok(()) => None,
                    Err(Skip::Already(reason) | Skip::Refused(reason)) => Some(reason.clone()),
                },
            })
            .collect();

        if !targets.dry_run {
            // One scripting call per destination, since a request carries one.
            let mut groups: BTreeMap<Option<i64>, Vec<usize>> = BTreeMap::new();
            for (position, step) in steps.iter().enumerate() {
                if step.outcome.is_ok() {
                    groups.entry(step.destination).or_default().push(position);
                }
            }
            for (destination, positions) in groups {
                let request = json!({
                    "op": "messages",
                    "action": action.name(),
                    "color": match action { Action::Flag(color) => color.index(), _ => 0 },
                    "destination": destination.map(|id| self.address(id)).transpose()?,
                    "targets": positions
                        .iter()
                        .map(|&position| self.location_in(&steps[position].record, steps[position].source.expect("planned step has a source")))
                        .collect::<Result<Vec<_>>>()?,
                });
                match scripting::call::<Vec<ScriptResult>>(&request) {
                    Ok(replies) => {
                        for (position, reply) in positions.iter().zip(replies) {
                            if !reply.ok {
                                results[*position].status = ActionStatus::Failed;
                                results[*position].error = reply.error;
                            }
                        }
                    }
                    Err(error) => {
                        for position in positions {
                            results[position].status = ActionStatus::Failed;
                            results[position].error = Some(format!("{error:#}"));
                        }
                    }
                }
            }
        }

        if moves && !targets.dry_run {
            self.confirm_moves(&steps, &mut results)?;
        }

        let name = match action {
            Action::Archive => "archive",
            other => other.name(),
        };
        Ok(Output::Actions(ActionReport {
            action: name.to_string(),
            dry_run: targets.dry_run,
            results,
        }))
    }

    fn plan(&self, action: Action, record: MessageRecord) -> Result<Step> {
        let mailbox = self.mailbox_of(&record)?;
        let inbox_label = record
            .labels
            .iter()
            .filter_map(|id| self.catalog.mailbox(*id))
            .find(|label| label.kind == Some("inbox") && label.label_view)
            .map(|label| label.id);
        let mut step = Step {
            source: Some(mailbox.id),
            destination: None,
            outcome: Ok(()),
            prior: Vec::new(),
            record,
        };
        let already = |reason: &str| Err(Skip::Already(reason.to_string()));
        let local = mailbox.account_id.is_none();
        // Read and flag changes are idempotent, so they always run rather than
        // trusting a store that may lag behind Mail.
        step.outcome = match action {
            Action::Read | Action::Unread | Action::Flag(_) | Action::Unflag => Ok(()),
            Action::Trash | Action::Archive if local => Err(Skip::Refused(
                "On My Mac messages have no trash or archive; use `mailctl move --to <id>`".into(),
            )),
            Action::Trash if mailbox.kind == Some("trash") => Err(Skip::Refused(
                "already in trash; mailctl does not delete permanently".into(),
            )),
            Action::Trash => match self.resolve_move(MoveTo::Special("trash"), mailbox) {
                Ok(trash) => {
                    step.destination = Some(trash);
                    Ok(())
                }
                Err(reason) => Err(Skip::Refused(reason)),
            },
            Action::Move(target) => {
                // A Gmail inbox message leaves the inbox when moved, as in Mail.
                step.source = inbox_label.or(step.source);
                match self.resolve_move(target, mailbox) {
                    Err(reason) => Err(Skip::Refused(reason)),
                    Ok(destination) => {
                        step.destination = Some(destination);
                        let labelled = step.record.labels.contains(&destination);
                        // A labelled Gmail inbox message still has to leave the inbox.
                        if destination == mailbox.id || (labelled && inbox_label.is_none()) {
                            already("already in that mailbox")
                        } else {
                            Ok(())
                        }
                    }
                }
            }
            Action::Archive => {
                let account = mailbox.account_id.as_deref();
                match account.and_then(|id| self.catalog.archive_for(id)) {
                    _ if account.is_some_and(|id| self.catalog.is_gmail(id)) => Err(Skip::Refused(
                        "Mail scripting cannot archive Gmail messages: a move to All Mail keeps \
                         the INBOX label. Archive in Mail, or move to another mailbox."
                            .into(),
                    )),
                    None => Err(Skip::Refused(
                        "the account has no archive mailbox; use `mailctl move --to`".into(),
                    )),
                    Some(archive) => {
                        step.destination = Some(archive.id);
                        if mailbox.id == archive.id {
                            already("already archived")
                        } else {
                            Ok(())
                        }
                    }
                }
            }
        };
        Ok(step)
    }

    /// Waits briefly for the store to show each moved message in its
    /// destination, records the id Mail gave it there, and fails moves that
    /// left the message in its source.
    fn confirm_moves(&self, steps: &[Step], results: &mut [ActionResult]) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut pending = Vec::new();
        for position in 0..steps.len() {
            if results[position].status != ActionStatus::Done {
                continue;
            }
            if steps[position].record.message_id.is_some() {
                pending.push(position);
            } else {
                results[position].status = ActionStatus::Unconfirmed;
                results[position].error =
                    Some("the message has no Message-ID, so the move cannot be checked".into());
            }
        }
        loop {
            let mut unsettled = Vec::new();
            for position in pending {
                let (in_destination, in_source) = self.whereabouts(&steps[position])?;
                match (in_destination, in_source) {
                    (Some(id), false) => results[position].new_id = Some(id),
                    _ => unsettled.push(position),
                }
            }
            pending = unsettled;
            if pending.is_empty() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        for position in pending {
            let (in_destination, in_source) = self.whereabouts(&steps[position])?;
            let result = &mut results[position];
            if in_source {
                result.status = ActionStatus::Failed;
                result.error = Some(format!(
                    "Mail kept the message in {}",
                    result.from_mailbox.as_deref().unwrap_or("its mailbox")
                ));
            } else if in_destination.is_some() {
                result.new_id = in_destination;
            } else {
                result.status = ActionStatus::Unconfirmed;
                result.error = Some(format!(
                    "the message has not appeared in {} yet; search for it before acting on it again",
                    result.to_mailbox.as_deref().unwrap_or("the destination")
                ));
            }
        }
        Ok(())
    }

    /// The id of the copy in the step's destination, and whether a copy
    /// remains in its source.
    fn whereabouts(&self, step: &Step) -> Result<(Option<i64>, bool)> {
        let message_id = step.record.message_id.as_deref().unwrap_or_default();
        let copies = self.store.copies(message_id)?;
        let in_destination = step.destination.and_then(|destination| {
            copies
                .iter()
                .find(|(id, mailboxes)| {
                    mailboxes.contains(&destination) && !step.prior.contains(id)
                })
                .map(|(id, _)| *id)
        });
        let in_source = step
            .source
            .filter(|source| !self.catalog.is_all_mail(*source))
            .is_some_and(|source| {
                copies
                    .iter()
                    .any(|(_, mailboxes)| mailboxes.contains(&source))
            });
        Ok((in_destination, in_source))
    }

    fn resolve_move(&self, target: MoveTo, mailbox: &Mailbox) -> Result<i64, String> {
        let kind = match target {
            MoveTo::Mailbox(id) => return Ok(id),
            MoveTo::Special(kind) => kind,
        };
        let missing = || format!("the account has no {kind} mailbox");
        let account = mailbox.account_id.as_deref().ok_or_else(missing)?;
        match self.catalog.select(&[kind.to_string()], Some(account)) {
            Ok(Some(ids)) if ids.len() == 1 => Ok(ids[0]),
            _ => Err(missing()),
        }
    }

    fn message(&self, id: i64) -> Result<MessageRecord> {
        self.store
            .messages_by_id(&[id])?
            .pop()
            .context("message lookup returned nothing")
    }

    fn mailbox_of(&self, record: &MessageRecord) -> Result<&Mailbox> {
        self.catalog.mailbox(record.mailbox).with_context(|| {
            format!(
                "message {} is in unknown mailbox {}",
                record.id, record.mailbox
            )
        })
    }

    fn location(&self, record: &MessageRecord) -> Result<Location> {
        self.location_in(record, record.mailbox)
    }

    fn location_in(&self, record: &MessageRecord, mailbox: i64) -> Result<Location> {
        let address = self.address(mailbox)?;
        Ok(Location {
            id: record.id,
            account: address.account,
            mailbox: address.mailbox,
        })
    }

    fn address(&self, mailbox: i64) -> Result<MailboxAddress> {
        let mailbox = self
            .catalog
            .mailbox(mailbox)
            .with_context(|| format!("unknown mailbox {mailbox}"))?;
        Ok(MailboxAddress {
            account: mailbox.account_id.clone(),
            mailbox: mailbox.path.clone(),
        })
    }

    fn label(&self, mailbox: i64) -> Option<String> {
        let mailbox = self.catalog.mailbox(mailbox)?;
        Some(format!("{}/{}", mailbox.owner(), mailbox.path))
    }

    fn view(&self, record: &MessageRecord) -> MessageView {
        let mailbox = self.catalog.mailbox(record.mailbox);
        MessageView {
            id: record.id,
            message_id: record
                .message_id
                .as_deref()
                .map(|value| value.trim_matches(['<', '>']).to_string()),
            account: mailbox.and_then(|mailbox| mailbox.account.clone()),
            account_id: mailbox.and_then(|mailbox| mailbox.account_id.clone()),
            mailbox: mailbox.map(|mailbox| mailbox.path.clone()),
            mailbox_id: record.mailbox,
            labels: record
                .labels
                .iter()
                .filter_map(|id| self.catalog.mailbox(*id))
                .map(|label| label.path.clone())
                .collect(),
            subject: record.subject.clone(),
            from: record.from.as_ref().map(address_view),
            to: record.to.iter().map(address_view).collect(),
            cc: record.cc.iter().map(address_view).collect(),
            date_received: dates::rfc3339(record.date_received),
            date_sent: record.date_sent.and_then(dates::rfc3339),
            read: record.read,
            flagged: record.flagged,
            flag_color: record
                .flagged
                .then_some(record.flag_color)
                .flatten()
                .and_then(FlagColor::from_index)
                .map(FlagColor::name),
            conversation_id: record.conversation_id,
            size: record.size,
            snippet: record.snippet.as_ref().map(|text| text.trim().to_string()),
            attachments: record.attachments.clone(),
            received: dates::short(record.date_received),
        }
    }
}

#[derive(Debug, Serialize)]
struct MailboxAddress {
    account: Option<String>,
    mailbox: String,
}

#[derive(Debug, Deserialize)]
struct ScriptResult {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ComposeResult {
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    sender: Option<String>,
    #[serde(default)]
    to: Vec<String>,
    sent: bool,
}

fn compose(action: &str, request: serde_json::Value) -> Result<Output> {
    let result: ComposeResult = scripting::call(&request)?;
    Ok(Output::Composed(ComposeReport {
        action: action.to_string(),
        subject: result.subject.unwrap_or_default(),
        sender: result.sender.unwrap_or_default(),
        to: result.to,
        sent: result.sent,
    }))
}

fn read_body(body: &Body) -> Result<String> {
    match (&body.body, &body.body_file) {
        (Some(text), _) => Ok(text.clone()),
        (None, Some(path)) if path.as_os_str() == "-" => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("failed to read stdin")?;
            Ok(text)
        }
        (None, Some(path)) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display())),
        (None, None) => Ok(String::new()),
    }
}

fn address_view(record: &AddressRecord) -> AddressView {
    AddressView {
        name: Some(record.name.clone()).filter(|name| !name.is_empty()),
        address: record.address.clone(),
    }
}
