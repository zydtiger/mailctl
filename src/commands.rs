use crate::catalog::{Catalog, Mailbox, SPECIAL_KINDS};
use crate::cli::{Body, Command, Filters, FlagColor, Targets};
use crate::output::{
    ActionReport, ActionResult, ActionStatus, AddressView, AttachmentView, ComposeReport,
    MessageDetail, MessageView, Output,
};
use crate::store::{AddressRecord, MessageQuery, MessageRecord, Store};
use crate::{dates, emlx, permissions, scripting, version};
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
        Command::Permissions { check } => return permissions::run(check),
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
        Command::Version
        | Command::Completions { .. }
        | Command::Permissions { .. }
        | Command::Draft { .. } => {
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
        Command::Send { id, dry_run } => session.send(id, dry_run),
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
            leftovers: None,
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

    fn send(&self, id: i64, dry_run: bool) -> Result<Output> {
        let record = self.message(id)?;
        let mut result = ActionResult {
            id,
            new_id: None,
            subject: record.subject.clone(),
            from_mailbox: self.label(record.mailbox),
            to_mailbox: None,
            status: ActionStatus::Failed,
            error: None,
        };
        let report = |result, leftovers| {
            Ok(Output::Actions(ActionReport {
                action: "send".into(),
                dry_run,
                results: vec![result],
                leftovers,
            }))
        };
        let (drafts, sent) = match self.plan_send(&record) {
            Ok(plan) => plan,
            Err(reason) => {
                result.error = Some(reason);
                return report(result, None);
            }
        };
        result.from_mailbox = self.label(drafts);
        result.to_mailbox = self.label(sent);
        if dry_run {
            result.status = ActionStatus::Planned;
            let mut others = self.drafts_like(&record, drafts)?;
            others.retain(|other| *other != id);
            return report(result, Some(others));
        }

        // Mail gives the sent copy a new Message-ID, so it is found by
        // subject and recipients among messages filed from now on.
        let since = Local::now().timestamp() - 5;
        let check = SentCheck {
            draft: &record,
            drafts,
            sent,
            since,
            prior: self.sent_like(&record, sent, since, &[])?,
        };
        let reply: Result<SendResult> = scripting::call(&json!({
            "op": "send",
            "target": self.location(&record)?,
        }));
        if let Ok(SendResult {
            refused: Some(reason),
            ..
        }) = reply
        {
            result.error = Some(reason);
            return report(result, None);
        }
        let leftovers = self.confirm_send(&check, reply, SEND_TIMEOUT, &mut result);
        report(result, leftovers)
    }

    /// Waits for the sent copy after Mail was asked to send, sets the
    /// result's status, and returns the leftover drafts of a confirmed send.
    /// The mail may already be out, so nothing here fails the send: store
    /// errors count as not found yet, and a failed request as unconfirmed.
    fn confirm_send(
        &self,
        check: &SentCheck,
        reply: Result<SendResult>,
        timeout: Duration,
        result: &mut ActionResult,
    ) -> Option<Vec<i64>> {
        let draft = check.draft;
        let mut deadline = Instant::now() + timeout;
        loop {
            if result.new_id.is_none() {
                result.new_id = self
                    .sent_like(draft, check.sent, check.since, &check.prior)
                    .ok()
                    .and_then(|ids| ids.first().copied());
                if result.new_id.is_some() {
                    // Give Mail a moment to remove the draft, then report it
                    // as a leftover if it is still there.
                    deadline = deadline.min(Instant::now() + DRAFT_REMOVAL_WAIT);
                }
            }
            let draft_kept = self
                .drafts_like(draft, check.drafts)
                .map_or(true, |ids| ids.contains(&draft.id));
            if (result.new_id.is_some() && !draft_kept) || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if result.new_id.is_some() {
            result.status = ActionStatus::Done;
            return self.drafts_like(draft, check.drafts).ok();
        }
        let sent_label = result.to_mailbox.clone().unwrap_or_else(|| "Sent".into());
        result.status = ActionStatus::Unconfirmed;
        result.error = Some(match reply {
            Ok(SendResult { sent: true, .. }) => format!(
                "Mail sent the draft, but it has not appeared in {sent_label} yet; check there before sending again"
            ),
            Ok(SendResult { error, .. }) => {
                let reason = error.map(|error| format!(" ({error})")).unwrap_or_default();
                format!(
                    "Mail did not confirm the send{reason}; check Outbox and {sent_label} before trying again"
                )
            }
            Err(error) => format!(
                "the send request failed after it reached Mail ({error:#}); check Outbox and {sent_label} before trying again"
            ),
        });
        None
    }

    /// The draft's Drafts mailbox and the Sent mailbox of its account.
    fn plan_send(&self, record: &MessageRecord) -> Result<(i64, i64), String> {
        let mailbox = self
            .mailbox_of(record)
            .map_err(|error| format!("{error:#}"))?;
        let drafts = std::iter::once(mailbox.id)
            .chain(record.labels.iter().copied())
            .find(|id| self.catalog.mailbox(*id).is_some_and(|mailbox| mailbox.kind == Some("drafts")))
            .ok_or_else(|| {
                format!(
                    "not a draft: the message is in {}; `send` sends only messages in a Drafts mailbox",
                    self.label(mailbox.id).unwrap_or_default()
                )
            })?;
        let sent = self.resolve_move(MoveTo::Special("sent"), mailbox)?;
        // Mail stops a send without recipients with a sheet in the compose
        // window. The store does not record Bcc, so a Bcc-only draft is
        // refused as well.
        if record.to.is_empty() && record.cc.is_empty() {
            return Err("the draft has no To or Cc recipients".into());
        }
        Ok((drafts, sent))
    }

    /// Ids of messages in `drafts` with the draft's subject, the draft included.
    fn drafts_like(&self, draft: &MessageRecord, drafts: i64) -> Result<Vec<i64>> {
        Ok(self
            .with_subject(&draft.subject, drafts, None)?
            .into_iter()
            .map(|record| record.id)
            .collect())
    }

    /// Ids of messages filed in `sent` at or after `since`, outside `prior`,
    /// that match the draft's subject and recipients, newest first.
    fn sent_like(
        &self,
        draft: &MessageRecord,
        sent: i64,
        since: i64,
        prior: &[i64],
    ) -> Result<Vec<i64>> {
        Ok(self
            .with_subject(&draft.subject, sent, Some(since))?
            .into_iter()
            .filter(|record| !prior.contains(&record.id) && same_message(draft, record))
            .map(|record| record.id)
            .collect())
    }

    fn with_subject(
        &self,
        subject: &str,
        mailbox: i64,
        since: Option<i64>,
    ) -> Result<Vec<MessageRecord>> {
        let mut records = self.store.messages(&MessageQuery {
            mailboxes: Some(vec![mailbox]),
            subject: Some(subject.to_string()),
            since,
            ..MessageQuery::default()
        })?;
        records.retain(|record| record.subject == subject);
        Ok(records)
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

/// The bridge's `send` reply. Only `refused` means nothing was sent; every
/// field defaults so that an unexpected reply counts as an unconfirmed send.
#[derive(Debug, Default, Deserialize)]
struct SendResult {
    /// Why Mail was not asked to send.
    #[serde(default)]
    refused: Option<String>,
    #[serde(default)]
    sent: bool,
    #[serde(default)]
    error: Option<String>,
}

/// What `send` looks for in the store after Mail was asked to send.
struct SentCheck<'a> {
    draft: &'a MessageRecord,
    drafts: i64,
    sent: i64,
    /// Sent copies are filed at or after this time.
    since: i64,
    /// Matching Sent messages that were already there.
    prior: Vec<i64>,
}

/// How long `send` waits for the store to show the sent copy. Exchange took
/// about ten seconds to file a sent message.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// How long `send` waits for Mail to remove the draft once the sent copy shows.
const DRAFT_REMOVAL_WAIT: Duration = Duration::from_secs(5);

/// Whether a stored message has the draft's subject and To and Cc addresses.
fn same_message(draft: &MessageRecord, candidate: &MessageRecord) -> bool {
    let addresses = |record: &MessageRecord| {
        let mut addresses: Vec<String> = record
            .to
            .iter()
            .chain(&record.cc)
            .map(|address| address.address.to_lowercase())
            .collect();
        addresses.sort_unstable();
        addresses
    };
    draft.subject == candidate.subject && addresses(draft) == addresses(candidate)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Account;
    use crate::store::tests::fixture_with;

    /// An Exchange account with Drafts (7) and Sent Items (8). Draft 200 has
    /// an autosaved copy 201 without recipients; Sent Items holds an older
    /// send 202, the new send 203, and 204 to someone else. Gmail draft 205
    /// is stored in All Mail (13) with the Drafts label (15).
    const SEND_FIXTURE: &str = "
        INSERT INTO mailboxes VALUES (7, 'ews://EWS/Drafts', 2, 0, NULL),
            (8, 'ews://EWS/Sent%20Items', 3, 0, NULL),
            (15, 'imap://GMAIL/%5BGmail%5D/Drafts', 1, 0, 13),
            (16, 'imap://GMAIL/%5BGmail%5D/Sent%20Mail', 0, 0, 13);
        INSERT INTO subjects VALUES (4, 'Agenda');
        INSERT INTO message_global_data VALUES (4, '<a@x>'), (5, '<b@x>'), (6, '<c@x>'),
            (7, '<d@x>'), (8, '<e@x>'), (9, '<f@x>');
        INSERT INTO messages VALUES
            (200, 4, 3, NULL, 4, NULL, 5000, 5000, 7, 0, 1, 0, 0, 10, 20),
            (201, 5, 3, NULL, 4, NULL, 4999, 4999, 7, 0, 1, 0, 0, 10, 21),
            (202, 6, 3, NULL, 4, NULL, 1000, 1000, 8, 0, 1, 0, 0, 10, 22),
            (203, 7, 3, NULL, 4, NULL, 6000, 6010, 8, 0, 1, 0, 0, 10, 20),
            (204, 8, 3, NULL, 4, NULL, 6000, 6005, 8, 0, 1, 0, 0, 10, 23),
            (205, 9, 3, NULL, 4, NULL, 5000, 5000, 13, 0, 1, 0, 0, 10, 24);
        INSERT INTO labels VALUES (205, 15);
        INSERT INTO recipients VALUES (10, 200, 1, 0, 0), (11, 202, 1, 0, 0),
            (12, 203, 1, 0, 0), (13, 204, 2, 0, 0), (14, 205, 1, 0, 0);
    ";

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

    fn session() -> (tempfile::TempDir, Session) {
        let (root, store) = fixture_with(SEND_FIXTURE);
        let accounts = vec![
            account(
                "EWS",
                "work@example.edu",
                &[
                    ("inbox", "Inbox"),
                    ("drafts", "Drafts"),
                    ("sent", "Sent Items"),
                ],
            ),
            account(
                "GMAIL",
                "me@gmail.com",
                &[("drafts", "[Gmail]/Drafts"), ("sent", "[Gmail]/Sent Mail")],
            ),
        ];
        let catalog = Catalog::build(accounts, &store.mailboxes().unwrap());
        (root, Session { store, catalog })
    }

    fn pending(session: &Session) -> ActionResult {
        ActionResult {
            id: 200,
            new_id: None,
            subject: "Agenda".into(),
            from_mailbox: None,
            to_mailbox: session.label(8),
            status: ActionStatus::Failed,
            error: None,
        }
    }

    #[test]
    fn send_plans_only_drafts_with_recipients() {
        let (_root, session) = session();
        let plan = |id| session.plan_send(&session.message(id).unwrap());
        assert_eq!(plan(200), Ok((7, 8)));
        // A Gmail draft is stored in All Mail and labelled Drafts.
        assert_eq!(plan(205), Ok((15, 16)));
        assert!(plan(100).unwrap_err().starts_with("not a draft"));
        assert!(plan(201).unwrap_err().contains("no To or Cc"));
    }

    #[test]
    fn matches_messages_by_subject_and_addresses() {
        let (_root, session) = session();
        let draft = session.message(100).unwrap();
        let mut copy = draft.clone();
        copy.to.swap_remove(0);
        copy.cc[0].address = "Me@Example.edu".into();
        copy.cc.push(AddressRecord {
            address: "DEALS@shop.example".into(),
            name: String::new(),
        });
        assert!(same_message(&draft, &copy));
        copy.cc.pop();
        assert!(!same_message(&draft, &copy));
        let mut renamed = draft.clone();
        renamed.subject = "Lease extension".into();
        assert!(!same_message(&draft, &renamed));
    }

    #[test]
    fn send_finds_copies_by_subject_and_recipients() {
        let (_root, session) = session();
        let draft = session.message(200).unwrap();
        assert_eq!(session.drafts_like(&draft, 7).unwrap(), vec![200, 201]);
        assert_eq!(
            session.sent_like(&draft, 8, 0, &[]).unwrap(),
            vec![203, 202]
        );
        assert_eq!(session.sent_like(&draft, 8, 0, &[202]).unwrap(), vec![203]);
        assert_eq!(session.sent_like(&draft, 8, 5000, &[]).unwrap(), vec![203]);
    }

    #[test]
    fn send_dry_run_reports_other_drafts() {
        let (_root, session) = session();
        let Output::Actions(report) = session.send(200, true).unwrap() else {
            panic!("send returns an action report");
        };
        assert_eq!(report.leftovers, Some(vec![201]));
        let [result] = report.results.try_into().unwrap();
        assert_eq!(result.status, ActionStatus::Planned);
        assert_eq!(
            result.from_mailbox.as_deref(),
            Some("work@example.edu/Drafts")
        );
        assert_eq!(
            result.to_mailbox.as_deref(),
            Some("work@example.edu/Sent Items")
        );

        let Output::Actions(report) = session.send(100, true).unwrap() else {
            panic!("send returns an action report");
        };
        assert_eq!(report.results[0].status, ActionStatus::Failed);
        assert_eq!(report.leftovers, None);
    }

    #[test]
    fn confirms_sends_from_the_store() {
        let (_root, session) = session();
        let draft = session.message(200).unwrap();
        let check = |prior: Vec<i64>| SentCheck {
            draft: &draft,
            drafts: 7,
            sent: 8,
            since: 5000,
            prior,
        };
        let confirm = |prior, reply| {
            let mut result = pending(&session);
            let leftovers = session.confirm_send(&check(prior), reply, Duration::ZERO, &mut result);
            (result, leftovers)
        };

        // The sent copy shows; the draft and its autosave are left over.
        let (result, leftovers) = confirm(vec![], Ok(SendResult::default()));
        assert_eq!(result.status, ActionStatus::Done);
        assert_eq!(result.new_id, Some(203));
        assert_eq!(leftovers, Some(vec![200, 201]));

        // Nothing new in Sent: unconfirmed, never failed.
        let replies = [
            (
                Ok(SendResult {
                    sent: true,
                    ..SendResult::default()
                }),
                "has not appeared",
            ),
            (Ok(SendResult::default()), "did not confirm"),
            (Err(anyhow::anyhow!("osascript died")), "osascript died"),
        ];
        for (reply, expected) in replies {
            let (result, leftovers) = confirm(vec![203], reply);
            assert_eq!(result.status, ActionStatus::Unconfirmed);
            assert!(result.error.unwrap().contains(expected));
            assert_eq!(leftovers, None);
        }
    }
}
