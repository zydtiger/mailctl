//! `mailctl meeting`: creates, reschedules, and cancels meetings in Microsoft
//! Outlook, which sends the invitations, updates, and cancellations from the
//! calendar's own account. Mail.app can only attach an `.ics` file, which
//! Exchange does not treat as a meeting. Outlook has no scripting dictionary,
//! so the embedded bridge `jxa/outlook.js` drives its windows through the
//! Accessibility API.

use crate::cli::{MeetingAction, MeetingRef};
use crate::output::{MeetingReport, MeetingStatus, Output};
use crate::{dates, permissions, scripting};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn run(action: MeetingAction) -> Result<Output> {
    let plan = Plan::new(action)?;
    // A dry run of create needs nothing from Outlook.
    let result = if plan.dry_run && plan.op == "create" {
        BridgeResult::default()
    } else {
        permissions::require_accessibility()?;
        let op = if plan.dry_run { "find" } else { plan.op };
        scripting::outlook(&plan.request(op))?
    };
    Ok(Output::Meeting(plan.report(result)))
}

/// A validated request, plus what a dry run reports.
#[derive(Debug)]
struct Plan {
    op: &'static str,
    dry_run: bool,
    request: Value,
    subject: String,
    start: Option<i64>,
    end: Option<i64>,
    attendees: Vec<String>,
    location: Option<String>,
    teams: Option<bool>,
}

/// What the bridge returns; fields depend on the operation.
#[derive(Debug, Default, Deserialize)]
struct BridgeResult {
    subject: Option<String>,
    calendar: Option<String>,
    start: Option<i64>,
    end: Option<i64>,
    #[serde(default)]
    attendees: Vec<String>,
    location: Option<String>,
    teams: Option<bool>,
    #[serde(default)]
    sent: bool,
    /// `create` left the meeting open in Outlook without sending.
    #[serde(default)]
    open: bool,
    found: Option<String>,
    prompt: Option<String>,
}

impl Plan {
    fn new(action: MeetingAction) -> Result<Self> {
        match action {
            MeetingAction::Create {
                subject,
                start,
                end,
                attendees,
                location,
                teams,
                send,
                dry_run,
            } => {
                ensure!(!subject.trim().is_empty(), "the subject is empty");
                let start = dates::parse_datetime(&start)?;
                let end = dates::parse_datetime(&end)?;
                check_span(start, end)?;
                for address in &attendees {
                    check_address(address)?;
                }
                let location = location.filter(|text| !text.trim().is_empty());
                Ok(Self {
                    op: "create",
                    dry_run,
                    request: json!({
                        "subject": subject, "start": start, "end": end,
                        "attendees": attendees, "location": location, "teams": teams,
                        "send": send,
                    }),
                    subject,
                    start: Some(start),
                    end: Some(end),
                    attendees,
                    location,
                    teams: Some(teams),
                })
            }
            MeetingAction::Update {
                meeting,
                new_start,
                new_end,
                location,
                dry_run,
            } => {
                let (subject, start) = find_by(&meeting)?;
                let new_start = new_start
                    .as_deref()
                    .map(dates::parse_datetime)
                    .transpose()?;
                let new_end = new_end.as_deref().map(dates::parse_datetime).transpose()?;
                if let Some(end) = new_end {
                    check_span(new_start.unwrap_or(start), end)?;
                }
                let location = location.map(|text| text.trim().to_string());
                Ok(Self {
                    op: "update",
                    dry_run,
                    request: json!({
                        "subject": subject, "start": start, "new_start": new_start,
                        "new_end": new_end, "location": location,
                    }),
                    subject,
                    start: Some(new_start.unwrap_or(start)),
                    end: new_end,
                    attendees: Vec::new(),
                    location: location.filter(|text| !text.is_empty()),
                    teams: None,
                })
            }
            MeetingAction::Cancel {
                meeting,
                message,
                dry_run,
            } => {
                let (subject, start) = find_by(&meeting)?;
                Ok(Self {
                    op: "cancel",
                    dry_run,
                    request: json!({ "subject": subject, "start": start, "message": message }),
                    subject,
                    start: Some(start),
                    end: None,
                    attendees: Vec::new(),
                    location: None,
                    teams: None,
                })
            }
        }
    }

    fn request(&self, op: &str) -> Value {
        let mut request = self.request.clone();
        request["op"] = json!(op);
        request
    }

    /// A dry run reports the plan; otherwise what Outlook showed before
    /// sending, which cancel does not read.
    fn report(self, result: BridgeResult) -> MeetingReport {
        let status = if self.dry_run {
            MeetingStatus::Planned
        } else if result.sent {
            MeetingStatus::Sent
        } else if result.open {
            MeetingStatus::Open
        } else {
            MeetingStatus::NotSent
        };
        let from_outlook = !self.dry_run && self.op != "cancel";
        let (start, end, attendees, location, teams) = if from_outlook {
            (
                result.start,
                result.end,
                result.attendees,
                result.location,
                result.teams,
            )
        } else {
            (
                self.start,
                self.end,
                self.attendees,
                self.location,
                self.teams,
            )
        };
        MeetingReport {
            action: self.op.to_string(),
            status,
            subject: result.subject.unwrap_or(self.subject),
            calendar: result.calendar,
            start: start.and_then(dates::rfc3339),
            end: end.and_then(dates::rfc3339),
            attendees,
            location,
            teams,
            found: result.found,
            prompt: result.prompt,
            when: when(start, end),
        }
    }
}

fn find_by(meeting: &MeetingRef) -> Result<(String, i64)> {
    ensure!(!meeting.subject.trim().is_empty(), "the subject is empty");
    Ok((
        meeting.subject.clone(),
        dates::parse_datetime(&meeting.start)?,
    ))
}

/// Outlook's editor has one end-time picker on the start's day.
fn check_span(start: i64, end: i64) -> Result<()> {
    ensure!(end > start, "the meeting must end after it starts");
    ensure!(
        dates::local_date(start) == dates::local_date(end),
        "the meeting must start and end on the same day"
    );
    Ok(())
}

fn check_address(address: &str) -> Result<()> {
    let valid = address.contains('@')
        && !address
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, ',' | ';' | '<' | '>'));
    ensure!(valid, "`{address}` is not a single email address");
    Ok(())
}

fn when(start: Option<i64>, end: Option<i64>) -> String {
    match (start, end) {
        (Some(start), Some(end)) => format!(
            "{} - {}",
            dates::short(start),
            dates::short(end).get(11..).unwrap_or_default()
        ),
        (Some(start), None) => dates::short(start),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn action(args: &[&str]) -> MeetingAction {
        let argv = ["mailctl", "meeting"].iter().chain(args);
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Meeting { action } => action,
            other => panic!("parsed {other:?}"),
        }
    }

    fn create(extra: &[&str]) -> Result<Plan> {
        let mut args = vec![
            "create",
            "--subject",
            "Sync",
            "--start",
            "2026-11-20T15:00",
            "--end",
            "2026-11-20T15:30",
            "--attendee",
            "a@example.edu",
        ];
        args.extend_from_slice(extra);
        Plan::new(action(&args))
    }

    #[test]
    fn create_builds_the_bridge_request() {
        let plan = create(&["--attendee", "b@example.edu", "--location", "Room 1"]).unwrap();
        let request = plan.request("create");
        assert_eq!(request["op"], "create");
        assert_eq!(request["subject"], "Sync");
        assert_eq!(
            request["end"].as_i64().unwrap() - request["start"].as_i64().unwrap(),
            1800
        );
        assert_eq!(
            request["attendees"],
            json!(["a@example.edu", "b@example.edu"])
        );
        assert_eq!(request["location"], "Room 1");
        assert_eq!(request["teams"], false);
        assert_eq!(request["send"], false);
        assert_eq!(create(&["--send"]).unwrap().request("create")["send"], true);
    }

    #[test]
    fn create_rejects_bad_spans_and_addresses() {
        let span = |start: &str, end: &str| {
            Plan::new(action(&[
                "create",
                "--subject",
                "Sync",
                "--start",
                start,
                "--end",
                end,
                "--attendee",
                "a@example.edu",
            ]))
        };
        assert!(span("2026-11-20T15:00", "2026-11-20T14:00").is_err());
        assert!(span("2026-11-20T23:00", "2026-11-21T00:30").is_err());
        assert!(create(&["--attendee", "Ann <a@example.edu>"]).is_err());
        assert!(create(&["--attendee", "a@example.edu, b@example.edu"]).is_err());
        assert!(create(&["--attendee", "nobody"]).is_err());
    }

    #[test]
    fn create_dry_run_reports_the_plan_without_outlook() {
        let Output::Meeting(report) = run(action(&[
            "create",
            "--subject",
            "Sync",
            "--start",
            "2026-11-20T15:00",
            "--end",
            "2026-11-20T15:30",
            "--attendee",
            "a@example.edu",
            "--dry-run",
        ]))
        .unwrap() else {
            panic!("expected a meeting report");
        };
        assert_eq!(report.status, MeetingStatus::Planned);
        assert_eq!(report.attendees, ["a@example.edu"]);
        assert_eq!(report.teams, Some(false));
        assert_eq!(report.when, "2026-11-20 15:00 - 15:30");
    }

    #[test]
    fn update_sends_only_requested_changes() {
        let plan = Plan::new(action(&[
            "update",
            "--subject",
            "Sync",
            "--start",
            "2026-11-20T15:00",
            "--new-start",
            "2026-11-20T16:00",
        ]))
        .unwrap();
        let request = plan.request("update");
        assert_eq!(
            request["new_start"].as_i64().unwrap() - request["start"].as_i64().unwrap(),
            3600
        );
        assert_eq!(request["new_end"], Value::Null);
        assert_eq!(request["location"], Value::Null);

        let clear = Plan::new(action(&[
            "update",
            "--subject",
            "Sync",
            "--start",
            "2026-11-20T15:00",
            "--location",
            "",
        ]))
        .unwrap();
        assert_eq!(clear.request("update")["location"], "");
        assert_eq!(clear.location, None);
    }

    #[test]
    fn update_checks_the_new_end_against_the_start() {
        let plan = |end: &str| {
            Plan::new(action(&[
                "update",
                "--subject",
                "Sync",
                "--start",
                "2026-11-20T15:00",
                "--new-end",
                end,
            ]))
        };
        assert!(plan("2026-11-20T15:45").is_ok());
        assert!(plan("2026-11-20T14:45").is_err());
    }

    #[test]
    fn report_status_follows_the_bridge() {
        let status = |result: BridgeResult| create(&[]).unwrap().report(result).status;
        let open = BridgeResult {
            open: true,
            ..BridgeResult::default()
        };
        let refused = BridgeResult {
            prompt: Some("You have not specified a location".into()),
            ..BridgeResult::default()
        };
        assert_eq!(status(open), MeetingStatus::Open);
        assert_eq!(status(refused), MeetingStatus::NotSent);
    }

    #[test]
    fn report_uses_outlook_state_after_sending() {
        let plan = create(&["--send"]).unwrap();
        let report = plan.report(BridgeResult {
            calendar: Some("me@example.edu".into()),
            start: Some(100 * 60),
            end: Some(130 * 60),
            attendees: vec!["a@example.edu".into()],
            teams: Some(false),
            sent: true,
            ..BridgeResult::default()
        });
        assert_eq!(report.status, MeetingStatus::Sent);
        assert_eq!(report.calendar.as_deref(), Some("me@example.edu"));
        assert_eq!(report.start, dates::rfc3339(6000));
        assert_eq!(report.location, None);
    }
}
