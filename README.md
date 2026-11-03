# mailctl

`mailctl` is a macOS command-line interface for Apple Mail. It reads the local
Mail store directly and makes changes through Mail.app's scripting interface,
so it works with every account already signed in to Mail.app, including
accounts that use OAuth. It never asks for mail server credentials.

Human-readable output is the default; pass `--json` to any command for compact
JSON.

## Status

`mailctl` currently supports:

- listing accounts, mailboxes, and messages across every account
- searching by subject, sender, recipient, snippet, date, and read or flag
  state, including Gmail label views such as INBOX
- reading message bodies, headers, attachment lists, raw source, and threads
- marking messages read or unread, and flagging them with a color
- moving messages, archiving them (except Gmail), and moving them to trash,
  with dry-run planning and per-message results
- saving new messages, replies, and forwards as drafts, or sending them

It does not search full message bodies, save attachments, or attach files to
outgoing mail. Forwards do not carry the original attachments.

## Requirements

- macOS on Apple Silicon, with Mail.app signed in to the accounts you want
- Rust toolchain for local builds (`rust-toolchain.toml` pins the version)
- Full Disk Access and Automation permissions for the app that runs mailctl

## Install

From the project root:

```sh
cargo install --path .
```

This installs the release build to Cargo's bin directory, usually
`~/.cargo/bin/mailctl`. Verify the installed build:

```sh
mailctl version
```

## Permissions

mailctl runs with the permissions of the app that launches it, such as your
terminal.

1. Grant that app **Full Disk Access** in System Settings > Privacy & Security
   > Full Disk Access, then restart it. Reads need this to open
   `~/Library/Mail`.
2. Run `mailctl accounts --refresh`. macOS asks whether the app may control
   Mail; allow it. Writes and account discovery need this **Automation**
   permission. To change it later, use System Settings > Privacy & Security >
   Automation.

## Usage

```sh
mailctl accounts
mailctl mailboxes
mailctl list --unread
mailctl search "invoice" --since 30d
mailctl show 76037
mailctl thread 76037
mailctl flag 76037 --color blue
mailctl move 76037 --to archive --dry-run
mailctl trash 76037
mailctl draft --to a@example.com --subject "Agenda" --body "Draft text"
mailctl reply 76037 --body "Thanks, Friday works."
```

Run `mailctl help <command>` for every option. [SKILL.md](SKILL.md) documents
mailbox selection, JSON fields, and the rules an agent follows before writing.

## Agent Skill

[SKILL.md](SKILL.md) is a self-contained agent skill for mailctl. Install it by
copying or linking it into your agent's skills directory, for example:

```sh
mkdir -p ~/.agents/skills/mailctl
ln -s "$PWD/SKILL.md" ~/.agents/skills/mailctl/SKILL.md
```

## Shell Completions

```sh
mailctl completions zsh > ~/.zfunc/_mailctl
```

`completions` also accepts `bash`, `fish`, `elvish`, and `powershell`.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Set `MAILCTL_MAIL_DIR` to a directory laid out like `~/Library/Mail/V10` to
run reads against a copy of the store, and `MAILCTL_CACHE_DIR` to keep the
account cache elsewhere. [docs/architecture.md](docs/architecture.md)
describes the store layout and Mail.app behavior mailctl depends on.
