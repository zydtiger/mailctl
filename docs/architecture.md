# Architecture

mailctl reads Mail's local store and writes through Mail.app. Writes never
touch the store. Reads touch Mail.app only to fetch account names into the
cache when the store names an account the cache has not seen; account ids
Mail.app does not list are cached as unknown so they do not trigger another
fetch.

| Module | Responsibility |
| --- | --- |
| `src/store.rs` | Read-only queries against `Envelope Index` |
| `src/emlx.rs` | Locating and reading `.emlx` message files |
| `src/catalog.rs` | Accounts (cached from Mail.app) joined with store mailboxes; mailbox selection |
| `src/commands.rs` | Command logic: query building, write planning, move confirmation |
| `src/scripting.rs` | Runs `src/jxa/mail.js` through `osascript` |
| `src/jxa/mail.js` | The Mail.app bridge: one JSON request in, one JSON response out |
| `src/output.rs` | Result types; JSON and human rendering |
| `src/responsibility.rs` | Re-executes mailctl as its own TCC responsible process |
| `src/permissions.rs` | Checks and requests Full Disk Access and Automation |

## Mail Store

Mail keeps its data under the highest-numbered `~/Library/Mail/V<n>`
directory (`V10` on macOS 26).

- `MailData/Envelope Index` is a WAL-mode SQLite database. mailctl opens it
  read-only, so it sees Mail's latest committed writes.
- `mailboxes.url` is `<scheme>://<account id>/<percent-encoded path>`, where
  the scheme is `imap`, `ews`, or `local`. The account id matches Mail.app's
  scripting `id` for the account. `local://` mailboxes are On My Mac.
- `messages.ROWID` is Mail.app's scripting `id` for the message. Mail assigns
  a new row when it moves a message, so ids are not stable across moves.
- Subjects, sender addresses, snippets (`summaries`), and the Message-ID
  header (`message_global_data.message_id_header`) live in side tables.
  `subject_prefix` holds prefixes such as `Re: `.
- `recipients.type` is 0 for To and 1 for Cc.
- The flag color is Mail's flag index in bits 39 to 41 of `messages.flags`.
  The `flag_color` column does not track it.
- Gmail stores each message once, in `[Gmail]/All Mail`. INBOX, Important,
  Sent Mail, and other labels are mailboxes whose `source` is All Mail, and
  the `labels` table links messages to them. Trash and Spam hold their own
  rows.

Message files live at
`<account id>/<mailbox>.mbox/<store uuid>/Data/<digits>/Messages/<id>.emlx`,
where nested mailboxes nest `.mbox` directories and `<digits>` are the digits
of `id / 1000` in reverse order. A `.partial.emlx` file lacks attachments that
Mail has not downloaded. The file can sit under a different mailbox than the
store reports while Mail catches up, so `emlx::find` falls back to every
mailbox of the account. An `.emlx` file is a byte-count line, the RFC 822
message, and an XML property list.

## Mail.app Scripting

- Mailboxes are addressed by their full path, such as `[Gmail]/All Mail`,
  through `account.mailboxes.byName`. A top-level mailbox's `container()` is
  null, or its account when reached through a unified mailbox such as
  `Mail.inbox()`.
- `whose` queries take seconds on large mailboxes, so mailctl looks messages
  up by id within a known mailbox and confirms moves by polling the store for
  the Message-ID instead. Copies that existed before the move are excluded, so
  a pre-existing duplicate is not reported as the moved message. A source in
  Gmail All Mail is never expected to empty, since All Mail keeps labelled
  messages.
- Moving a Gmail message to All Mail, or setting its `mailbox` to All Mail,
  leaves the INBOX label in place, so mailctl refuses to archive Gmail
  messages. Moving a Gmail message out of a label view removes that label.
- `delete` moves a message to its account's trash while the account's "move
  deleted messages to trash" setting is on, which is Mail's default.
- An outgoing message created with `visible: false` has an empty body after
  `reply` or `forward`, so the bridge writes the quoted original itself.
  `save()` stores a draft; `close` and `delete` do not reliably dismiss the
  invisible compose, which Mail discards when it quits.
- Mail's drafts store an empty `text/plain` part beside the HTML body, so
  `show` converts the HTML when the text part is blank.
- `osascript` inherits mailctl's TCC responsibility (see below), so the
  Automation permission names mailctl.

## Permissions

`src/responsibility.rs` re-executes mailctl through `posix_spawn` with
`responsibility_spawnattrs_setdisclaim`, a private libSystem call resolved at
run time. The child is its own responsible process, so Full Disk Access and
Automation are checked against mailctl rather than the app that launched it,
including helper apps that agent hosts run commands through. The parent only
waits and passes on the exit status. When the symbol is missing, mailctl runs
in-process under the launching app's permissions. `Info.plist` is embedded in
the binary's `__TEXT,__info_plist` section to give mailctl a bundle identifier
and the Automation usage string.

`mailctl permissions` checks Full Disk Access by listing `~/Library/Mail` in a
child process, so a grant made while it waits is seen. It checks Automation
with `AEDeterminePermissionToAutomateTarget`, which answers only while Mail
is running and shows the consent dialog when asked to.
