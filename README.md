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
- Full Disk Access and Automation permissions for the mailctl binary

## Install

Install a tagged release with Cargo:

```sh
cargo install --locked --git https://github.com/zydtiger/mailctl --tag v0.1.0
```

Or, from a checkout:

```sh
cargo install --locked --path .
```

Either installs the release build to Cargo's bin directory, usually
`~/.cargo/bin/mailctl`. Verify the installed build:

```sh
mailctl version
```

## Permissions

mailctl re-executes itself with TCC responsibility disclaimed, so macOS
checks permissions for mailctl itself rather than for the terminal, editor, or
agent app that launched it. Grant them once and mailctl works from any of
those.

Reads need **Full Disk Access** to open `~/Library/Mail`. Writes, drafts,
and account discovery need **Automation** permission to control Mail. Run:

```sh
mailctl permissions
```

1. Without Full Disk Access, it opens System Settings > Privacy & Security >
   Full Disk Access and reveals the mailctl binary in Finder. Drag the binary
   into the list and turn it on; macOS has no prompt for this permission.
2. It then starts Mail if needed and asks for Automation permission. Allow
   mailctl to control Mail in the dialog. If you refused before, it opens the
   Automation pane instead.

It waits for each grant and prints both states. `mailctl permissions --check`
only reports them.

The grants follow the binary's code signature. An ad-hoc signed rebuild is a
new binary to macOS: run `mailctl permissions` again, and remove the old Full
Disk Access entry.

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
