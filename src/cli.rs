use clap::{ArgGroup, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "mailctl",
    version,
    about = "Read the local Apple Mail store and act on messages through Mail.app"
)]
pub struct Cli {
    /// Print compact JSON instead of human-readable output.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show the build version, commit, and target.
    Version,

    /// Print a shell completion script.
    Completions { shell: Shell },

    /// Check Full Disk Access and Automation for Mail, and request what is
    /// missing.
    Permissions {
        /// Report the current state without opening System Settings or
        /// prompting.
        #[arg(long)]
        check: bool,
    },

    /// List Mail accounts and their special mailboxes.
    Accounts {
        /// Re-read accounts from Mail.app instead of the cache.
        #[arg(long)]
        refresh: bool,
    },

    /// List mailboxes with message and unread counts.
    Mailboxes {
        /// Limit to one account (name, email address, or id).
        #[arg(long)]
        account: Option<String>,
    },

    /// List messages, newest first. Defaults to every account's inbox.
    List(Filters),

    /// Search subject, sender, and snippet text. Defaults to all mailboxes.
    Search {
        query: String,
        #[command(flatten)]
        filters: Filters,
    },

    /// Show one message with its text body.
    Show {
        id: i64,
        /// Print the HTML body instead of the text body.
        #[arg(long, conflicts_with = "raw")]
        html: bool,
        /// Print the raw RFC 822 source.
        #[arg(long)]
        raw: bool,
    },

    /// List every message in the conversation containing a message.
    Thread { id: i64 },

    /// Mark messages as read.
    Read(Targets),

    /// Mark messages as unread.
    Unread(Targets),

    /// Flag messages.
    Flag {
        #[command(flatten)]
        targets: Targets,
        /// Flag color.
        #[arg(long, value_enum, default_value_t = FlagColor::Red)]
        color: FlagColor,
    },

    /// Remove flags from messages.
    Unflag(Targets),

    /// Move messages to another mailbox.
    Move {
        #[command(flatten)]
        targets: Targets,
        /// Destination mailbox: id, `<account>/<path>`, or a special name with --account.
        #[arg(long)]
        to: String,
        /// Account for a special destination name such as `archive`.
        #[arg(long)]
        account: Option<String>,
    },

    /// Archive messages: Gmail removes the inbox label, other accounts move to `Archive`.
    Archive(Targets),

    /// Move messages to their account's trash mailbox.
    Trash(Targets),

    /// Compose a new message and save it as a draft, or send it with --send.
    Draft {
        /// Recipient address; repeat for several.
        #[arg(long, required = true)]
        to: Vec<String>,
        #[arg(long)]
        cc: Vec<String>,
        #[arg(long)]
        bcc: Vec<String>,
        #[arg(long, default_value = "")]
        subject: String,
        #[command(flatten)]
        body: Body,
        /// Sender address; selects the sending account. Defaults to Mail's default account.
        #[arg(long)]
        from: Option<String>,
        /// Send immediately instead of saving a draft.
        #[arg(long)]
        send: bool,
    },

    /// Send a draft from a Drafts mailbox exactly as it is saved, now or at a
    /// later time with Mail's Send Later.
    Send {
        /// Draft id from the `draft`, `reply`, or `forward` result, or from
        /// `list --mailbox drafts`.
        id: i64,
        /// Schedule the send: local `YYYY-MM-DDTHH:MM` or RFC 3339, in the
        /// future. This Mac must be on and online at that time.
        #[arg(long)]
        at: Option<String>,
        /// Print the draft and its Sent mailbox without sending.
        #[arg(long)]
        dry_run: bool,
    },

    /// Reply to a message and save the reply as a draft, or send it with --send.
    Reply {
        id: i64,
        /// Reply to all recipients.
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        body: Body,
        #[arg(long)]
        send: bool,
    },

    /// Forward a message and save it as a draft, or send it with --send.
    Forward {
        id: i64,
        #[arg(long, required = true)]
        to: Vec<String>,
        #[command(flatten)]
        body: Body,
        #[arg(long)]
        send: bool,
    },

    /// Create, reschedule, or cancel meetings through Microsoft Outlook, which
    /// sends the invitations from the calendar's own account.
    Meeting {
        #[command(subcommand)]
        action: MeetingAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum MeetingAction {
    /// Create a meeting in Outlook's default calendar and leave it open there
    /// unsent, or send the invitations with --send.
    Create {
        #[arg(long)]
        subject: String,
        /// Start: local `YYYY-MM-DDTHH:MM` or RFC 3339.
        #[arg(long)]
        start: String,
        /// End on the same day; same formats as --start.
        #[arg(long)]
        end: String,
        /// Required attendee address; repeat for several.
        #[arg(long = "attendee", required = true)]
        attendees: Vec<String>,
        /// Location text, kept as typed rather than matched to a room.
        #[arg(long)]
        location: Option<String>,
        /// Keep the Teams meeting Outlook can add; off by default.
        #[arg(long)]
        teams: bool,
        /// Send the invitations instead of leaving the meeting open in Outlook
        /// for review.
        #[arg(long)]
        send: bool,
        /// Print the meeting without opening Outlook.
        #[arg(long, conflicts_with = "send")]
        dry_run: bool,
    },

    /// Reschedule or relocate a meeting you organize and send the update.
    #[command(group(ArgGroup::new("change").required(true).multiple(true).args(["new_start", "new_end", "location"])))]
    Update {
        #[command(flatten)]
        meeting: MeetingRef,
        /// New start; Outlook keeps the duration unless --new-end is given.
        #[arg(long)]
        new_start: Option<String>,
        /// New end on the same day as the start.
        #[arg(long)]
        new_end: Option<String>,
        /// New location; an empty value removes it.
        #[arg(long)]
        location: Option<String>,
        /// Find the meeting without opening or changing it.
        #[arg(long)]
        dry_run: bool,
    },

    /// Cancel a meeting you organize and send the cancellation.
    Cancel {
        #[command(flatten)]
        meeting: MeetingRef,
        /// Note to the invitees.
        #[arg(long)]
        message: Option<String>,
        /// Find the meeting without opening or changing it.
        #[arg(long)]
        dry_run: bool,
    },
}

/// An existing meeting, found by Outlook's event search.
#[derive(Debug, Clone, Args)]
pub struct MeetingRef {
    /// Subject, exactly as Outlook shows it.
    #[arg(long)]
    pub subject: String,
    /// Current start: local `YYYY-MM-DDTHH:MM` or RFC 3339.
    #[arg(long)]
    pub start: String,
}

#[derive(Debug, Clone, Args)]
pub struct Filters {
    /// Mailbox: id, `<account>/<path>`, or inbox|sent|drafts|trash|junk|archive|all. Repeatable.
    #[arg(long)]
    pub mailbox: Vec<String>,
    /// Limit to one account (name, email address, or id).
    #[arg(long)]
    pub account: Option<String>,
    /// Only unread messages.
    #[arg(long)]
    pub unread: bool,
    /// Only flagged messages.
    #[arg(long)]
    pub flagged: bool,
    /// Sender address or name contains this text.
    #[arg(long)]
    pub from: Option<String>,
    /// A To or Cc recipient address or name contains this text.
    #[arg(long)]
    pub to: Option<String>,
    /// Subject contains this text.
    #[arg(long)]
    pub subject: Option<String>,
    /// Received at or after: YYYY-MM-DD, RFC 3339, today, yesterday, or a duration ago such as 3h, 2d, 1w.
    #[arg(long)]
    pub since: Option<String>,
    /// Received before: same formats as --since.
    #[arg(long)]
    pub before: Option<String>,
    /// Maximum number of messages.
    #[arg(long, default_value_t = 25)]
    pub limit: u32,
}

#[derive(Debug, Clone, Args)]
pub struct Targets {
    /// Message ids from `list`, `search`, or `show`.
    #[arg(required = true)]
    pub ids: Vec<i64>,
    /// Print the resolved targets without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Args)]
#[group(required = false, multiple = false)]
pub struct Body {
    /// Message body text.
    #[arg(long)]
    pub body: Option<String>,
    /// Read the body text from a file, or `-` for stdin.
    #[arg(long)]
    pub body_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FlagColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    Gray,
}

impl FlagColor {
    /// Mail's `flag index`, which is also the Envelope Index `flag_color` value.
    pub fn index(self) -> i64 {
        self as i64
    }

    pub fn from_index(index: i64) -> Option<Self> {
        Self::value_variants()
            .get(usize::try_from(index).ok()?)
            .copied()
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Gray => "gray",
        }
    }
}

pub fn print_completions(shell: Shell) {
    clap_complete::generate(
        shell,
        &mut Cli::command(),
        "mailctl",
        &mut std::io::stdout(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn meeting_update_needs_a_change() {
        let base = [
            "mailctl",
            "meeting",
            "update",
            "--subject",
            "Sync",
            "--start",
            "2026-11-20T15:00",
        ];
        assert!(Cli::try_parse_from(base).is_err());
        assert!(Cli::try_parse_from(base.iter().chain(&["--location", ""])).is_ok());
    }

    #[test]
    fn flag_colors_follow_mail_flag_index() {
        assert_eq!(FlagColor::Red.index(), 0);
        assert_eq!(FlagColor::Gray.index(), 6);
        assert_eq!(FlagColor::from_index(4), Some(FlagColor::Blue));
        assert_eq!(FlagColor::from_index(-1), None);
    }
}
