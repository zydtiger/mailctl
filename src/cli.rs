use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
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
    fn flag_colors_follow_mail_flag_index() {
        assert_eq!(FlagColor::Red.index(), 0);
        assert_eq!(FlagColor::Gray.index(), 6);
        assert_eq!(FlagColor::from_index(4), Some(FlagColor::Blue));
        assert_eq!(FlagColor::from_index(-1), None);
    }
}
