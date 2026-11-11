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
| `src/scripting.rs` | Runs the JXA bridges through `osascript`, each prefixed with `src/jxa/ax.js` |
| `src/jxa/ax.js` | Accessibility helpers shared by both bridges |
| `src/jxa/mail.js` | The Mail.app bridge: one JSON request in, one JSON response out |
| `src/meeting.rs` | `meeting` commands: validation, bridge requests, reports |
| `src/jxa/outlook.js` | The Outlook bridge: drives Outlook's windows through the Accessibility API |
| `src/output.rs` | Result types; JSON and human rendering |
| `src/responsibility.rs` | Re-executes mailctl as its own TCC responsible process |
| `src/permissions.rs` | Checks and requests Full Disk Access and Automation; reports Accessibility |

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
  a pre-existing duplicate is not reported as the moved message. Copies that
  share a Message-ID and move together, such as the Sent and Inbox copies of
  a message to oneself, each get their own destination copy, preferring the
  one with the same received date, which a move keeps. A source in
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
- `send` accepts only an outgoing message, and a saved draft is a plain
  message. Mail keeps every compose created through scripting, sent or not,
  in `outgoing messages` until it quits. Opening a draft whose compose Mail
  still holds creates no new compose; it only re-saves the draft under a new
  id. `mailctl send` therefore first looks for a held compose with the
  draft's subject, sender address, To and Cc addresses, and text (ignoring
  surrounding whitespace), and sends it when exactly one matches. Sending
  that compose filed the message in Sent Items within about three seconds and
  removed the draft, with no autosaved copies. Without a held compose,
  `send` opens the draft, as a double-click does, and sends the new compose;
  that path has not been checked against a live send.
- `reply --send` once returned success but sent the wrong body: Mail had
  held, since an earlier `reply` on the same thread two days before, a
  compose whose draft the user had then sent from Mail, and the new send
  carried that old reply on top with the requested text quoted below it.
  How the old text got there is unconfirmed. Its "Nov 8, 2026 at 3:22 PM"
  attributions are mailctl's own: on macOS 26, `formatDate`
  (`toLocaleString` with medium date and short time) writes "at".
  `draft`, `reply`, and `forward` with `--send` therefore compose in one
  bridge request and send in a second. The first returns the new outgoing
  message's id, its content read back by that id, and the subject, To, and
  Cc of every other outgoing message, without sending. mailctl refuses when
  another one has the same subject and To and Cc addresses, when one cannot
  be read, or when the content does not begin with the requested body as
  whole lines, after normalizing line endings, non-breaking spaces, and
  trailing whitespace. Otherwise the second request sends the outgoing
  message by that id. The content check cannot see what Mail adds only while
  sending.
- Checked live on Exchange on 2026-11-10, self-addressed:
  - Outgoing message ids stayed stable across separate `osascript` runs, and
    sending by id worked for `draft`, `reply`, and `forward`.
  - A saved, unsent `draft` or `reply` compose stays in `outgoing messages`,
    so a later `--send` with its subject and recipients is refused. A
    compose created by `make` stays there after it is sent; a `reply` or
    `forward` compose does not, so a sent reply never blocks the next one.
  - Reading `content` of a new reply compose before setting it makes Mail
    ignore the assignment: `content` stays empty, also when read later.
    The bridge never reads it first; the content check refuses that case.
  - Mail autosaves an invisible compose that was neither saved nor sent,
    including a refused one, to Drafts within seconds, with or without its
    recipients.
  - Mail puts every scripted body, from `make`, `reply`, or `forward`,
    into a share template: an empty `Apple-Mail-URLShareUserContentTopClass`
    `div`, then the content inside a `blockquote type="cite"` styled without
    a quote bar. Saved drafts carry it too. The HTML part reads as unquoted
    text, but the sent `text/plain` part prefixes every line with `> `.
    Assigning `content` after creation and making a `paragraph` give the
    same result; making a paragraph at `paragraphs.end` fails with "Invalid
    key form". This matches Apple feedback FB11734014, reported since
    Ventura, and mails sent with mailctl 0.3.0 have it as well.
  - A reply opened with `openingWindow: true` gets Mail's native body
    instead: a top line break and the quoted original in
    `AppleOriginalContents`, with Mail's attribution. Setting
    `AXSelectedText` on its focused `AXWebArea` is accepted but changes
    nothing, since the web area exposes no selected text range. Putting the
    text on the pasteboard and pressing Edit > Paste inserts it at the top,
    outside any quote. `content` of such a window compose reads as empty.
    mailctl does not use this path, which would need Accessibility, a
    visible window, and the user's pasteboard for every compose.
- Every draft save, including Mail's autosaves, stores a new message with a
  new Message-ID, and the sent copy gets another. `send` therefore confirms by
  subject and To and Cc addresses among Sent messages that were not there
  before, and reports other Drafts messages with the same subject as
  leftovers. Exchange has taken up to about ten seconds to file a sent
  message in Sent Items.
- One scripted compose and `save()` can leave more than one Drafts message.
  On Exchange, one `draft` call left two messages stored in the same second
  with the same subject, To, and body, different Message-IDs, and
  consecutive ids, and a draft first listed as id N was later found only as
  N+1. On Gmail, a copy without To recipients appeared beside the draft and
  disappeared a few seconds later. Mail autosaving the held compose besides
  `save()` would explain this, but no live trace has confirmed it.
  `draft`, `reply`, and `forward` without `--send` therefore record the ids
  already in every Drafts mailbox before composing, then poll the store for
  messages stored since then with the compose's subject. The newest one
  with the compose's sender address and To and Cc addresses is the draft,
  and the rest are reported as copies, whatever their sender, since Mail
  may store the compose before mailctl sets its sender. mailctl moves none
  of them. The id is confirmed once that set has stayed unchanged, with
  every store read succeeding, for ten seconds, Exchange's filing time,
  within a thirty-second limit. Nothing ties a stored draft to the compose
  beyond these fields, so a matching draft saved elsewhere during the call
  can be taken for it. Composing still saves when the store cannot be read,
  and then reports no id.
- Mail's drafts store an empty `text/plain` part beside the HTML body, so
  `show` converts the HTML when the text part is blank.
- `osascript` inherits mailctl's TCC responsibility (see below), so the
  Automation permission names mailctl.

## Send Later

Mail's scripting dictionary has only an immediate `send`, so `send --at`
drives the Send Later UI through the Accessibility API, as the Outlook bridge
does. Observed with Mail on macOS 26.6, English UI:

- Message > Send Later is a submenu of "Send 8:00 AM Tomorrow", "Send 8:00 AM
  Monday" (U+202F before AM), and "Send Later…" (U+2026). They are enabled only
  for a compose window with a recipient, and only after Mail has rebuilt the
  menu for a newly key window, so the bridge waits for the item.
- "Send Later…" shows an `AXSheet` on the compose window with `AXDateTimeArea`
  pickers identified `Mail.datePicker.stepper`, `Mail.timePicker.stepper`,
  `Mail.datePicker.calendar`, and `Mail.timePicker.clock`, and Cancel and
  Schedule buttons. Writing an `NSDate` to the two stepper pickers' `AXValue`
  sets all four. The sheet says the message "will be delivered by this
  device", so Mail sends it locally.
- Schedule closes the compose window and files the message in
  `On My Mac/SendLater` within a few seconds, with the draft removed and
  `date_received` set to the time it was scheduled.
- A compose window carries the subject as its title, and its fields are
  identified `Mail.toField`, `Mail.ccField`, and `Mail.subjectField`.
- Setting `visible` on a held compose shows an empty window rather than its
  message. Opening a draft whose compose Mail holds shows a read-only viewer;
  Message > Send Again there opens a compose window with the draft's content,
  which leaves the draft in Drafts.
- Closing a changed compose window asks "Save this message as a draft?" with
  Save, Don't Save, and Cancel.

## Outlook Meetings

A meeting request needs an iTIP `text/calendar; method=REQUEST` part that the
organizer's server tracks. Mail.app can only attach an `.ics` file, EventKit's
attendees are read-only, and Calendar.app's Exchange accounts stop syncing
when Exchange Online retires EWS. The new Outlook for Mac sends meetings
through its own Microsoft 365 connection, but it has no scripting
dictionary, so `src/jxa/outlook.js` drives its windows through the
Accessibility API, called through the JXA ObjC bridge. It never uses System
Events, so it needs Accessibility but no Automation permission. Observed with
Outlook 16.113 on macOS 26.6, English UI:

- Outlook's elements are labelled by `AXDescription`: the editor's `Subject`,
  `Add required people` (`Required Attendees` once filled), and
  `Add a location` (`Locations`) text fields; `AXDateTimeArea` pickers titled
  `Start date`, `Start time`, and `End time`; an `AXSwitch` titled
  `Teams meeting`; and a `Calendar (<account>), From` pop-up. Event windows
  are titled `<subject> • Calendar • <account>`.
- Writing an `NSDate` to a picker's `AXValue` through
  `AXUIElementSetAttributeValue` updates Outlook's model. System Events
  reports the same write as done but cannot convert the date, so nothing
  changes. Setting the start date or time moves the end with it.
- Writing `AXValue` to the location or attendee field shows the text without
  updating Outlook's model, and Outlook then asks about a missing location.
  Writing `AXSelectedText` after focusing the field does update it. Return
  turns the text into the first directory suggestion, which for a location
  can be a room mailbox, so the bridge presses Escape and then Tab, which
  keeps the typed text as the token. Each token appears as an
  `AXStaticText` beside the field, which the bridge reads back. Outlook reads
  `Name (detail)` as a room name and address.
- Keys go to Outlook's process with `CGEventPostToPid`, never to the
  frontmost app: an earlier System Events `keystroke` typed into whatever
  app was in front.
- Outlook ignores the editor's Send, Save, and Discard buttons, and the
  cancellation's Send cancellation, unless it is the active app: `AXPress`
  only marks them `, pressed`. Activating it through the application's
  `AXFrontmost` attribute and pressing within the same `osascript` run
  works; between separate runs the launching app can take the front back.
  The bridge restores the previously focused app afterwards.
- Sending a meeting without a location shows an `AXSheet` asking for one,
  with `Send invitation` and `Continue editing` buttons. Sheets are children
  of the window and do not appear in System Events' `UI elements`.
- After the first attendee is added, Outlook may turn on Teams. The bridge
  sets the switch afterwards.
- Event search (the main window's `AXSearchField` while the Calendar view is
  shown) lists matches in an `AXTable` described `Calendar Events`. Each row's
  cell is described `<subject>, starts on Nov 27, 2026 at 10:00 AM to
  10:30 AM, you are the organizer, …`, with U+202F before AM and PM.
  Results arrive per calendar, so an invitation copy in another account's
  calendar can be listed before the organizer's row. A search with no results
  leaves the previous results listed; writing an empty `AXValue` and pressing
  Escape ends the search first. The search field takes a written `AXValue`,
  while Command-A does not reliably select its text.
  File > Open > Current Item often does nothing after a search; focusing the
  table, selecting the row, and pressing Return opens it.
- An opened event is read-only and shows `Cancel` only for a meeting with
  invitees (a plain event shows `Delete`). While it loads, the window briefly
  shows other buttons, including a `Cancel`, so the bridge waits for `Edit`.
  `Edit` switches the same window to the editor but returns an AX error.
  `Cancel` adds a panel with an `Optional message to invitees` text area
  and `Send cancellation`.
- Opening an event that already has a window only brings that window
  forward, so `update` and `cancel` refuse when one is open.
- Outlook sends an update after Send in the editor, and attendees' calendars
  follow it. A cancelled meeting moves to the organizer's Deleted Items.
- Edits to an existing meeting, and its cancellation panel, take effect only
  on Send; closing the window drops them. So only `create` has an unsent
  state: without `--send`, the bridge leaves the filled-in editor open, which
  Outlook does not save anywhere.

## Outlook Compose Spike

Issue #11 asked whether `draft`, `reply`, and `forward` could compose in the
new Outlook through the Accessibility API while reads stay on Mail's store.
A spike on 2026-11-10 tried it with throwaway JXA scripts built on `ax.js`,
against Outlook 16.113.3 with the English UI on macOS 26.6 and the Exchange
account, all messages self-addressed. No mailctl code drives this path yet.

Composing works and gives a clean body:

- File > New > Mail opens a window titled `Untitled • <account>` in 2 to
  3 seconds; the title follows the subject. Its fields carry stable
  `AXIdentifier`s: `accountPicker` (value `Name (address)`), `toTextField`,
  `ccShowButton`, `subjectTextField`. The body is the `AXTextArea` inside the
  web area of the group described `Editor`.
- To takes the token method the meeting bridge uses (`AXSelectedText`,
  Escape, Tab). Tokens read back as `AXStaticText` such as
  `Tiger Ding, zding27@jh.edu, Away`, with the presence status appended.
  Subject takes a written `AXValue`.
- In the body, `AXSelectedText` reports success and changes nothing, with or
  without setting `AXSelectedTextRange` first, as in Mail. Writing `AXValue`
  reaches Outlook's model in an empty body, newlines included, but would
  replace the quoted original in a reply. Unicode key events
  (`CGEventKeyboardSetUnicodeString`, bound through `ObjC.bindFunction`
  with a `void *` string argument, posted with `CGEventPostToPid`) type at
  the caret, which is at the top of a new reply or forward, above Outlook's
  quote, without Outlook in front and without the pasteboard. Return keys
  make the line breaks.
- File > Save, with Outlook in front, saves to the account's Drafts;
  `Draft saved just now` appears in the compose.
- The sent new message, reply, and forward each had the typed text unquoted
  in both parts: no `blockquote type="cite"` and no `> ` lines. Outlook
  quotes the original below a `From:`/`Date:`/`To:`/`Subject:` block inside
  `div#mail-editor-reference-message-container`. Reply subjects get `Re:`,
  forwards `FW:`. Both carry `In-Reply-To` and `References` with the
  original's Message-ID. The sent copy keeps the draft's Message-ID.

Replying to a given message:

- Outlook shows no Message-ID anywhere found. The reading pane's
  `HeaderMainContainer` gives the subject, `From:` with address, `Sent on:`
  to the minute, and recipients; result rows give sender, subject,
  minute-resolution time, folder, unread state, and a body preview.
- Search covers the selected folder (`searchScopeButton` reads
  `Current Folder`; its pop-up does not open through AX). Selecting the
  folder's row in the navigation `AXOutline` and then writing the
  `AXSearchField` value and pressing the `Perform Search` button searches;
  Return posted to the process does not. Matching is fuzzy, so a search for
  one subject lists similar subjects too. Results come back in about six
  seconds.
- Rows are grouped by conversation (`13 messages, …`). Selecting the row and
  posting Right Arrow expands it into one row per message. In the test
  thread two replies shared sender, subject, and minute and differed only in
  preview, and Mail's Inbox and Sent copies of one message are two rows with
  identical fields. Matching a store record therefore rests on folder,
  sender, subject, minute, and preview text; a tie has to be refused, and
  any copy of the same message is an acceptable target.
- The header's `Reply`, `Reply all`, and `Forward` buttons act only with
  Outlook in front and open an inline compose in the reading pane within
  about 2.5 seconds, with the same identifiers as the window plus
  `fullMessageComposeButton`. Opening a message that is itself a draft shows
  the inline editor instead of the header.
- The menu bar follows the key window: with a compose key, `Message` is
  replaced by `Draft`, so the bridge uses the header buttons rather than
  Message > Reply.

What failed or stays open:

- Read-back from Mail's store is unreliable for Outlook drafts. Mail showed
  no Exchange draft for over nine minutes after Outlook saved the new
  message, then showed it. The reply and forward drafts appeared within
  about two minutes but with the body of Outlook's first autosave, made when
  the compose opened, and Mail did not update them in the next five minutes
  although Outlook's Drafts preview showed the typed text. Exchange updates a
  draft in place under the same Message-ID. `check for new mail` and
  `synchronize` did not speed this up. Sent copies are new items and did
  appear in Mail's Sent Items, and Mail dropped the drafts after the send.
  A pre-send check has to read Outlook's own editor, not the store.
- Send needs Outlook in front. The forward was read back through AX with the
  expected body, then sent with the text `Hat ` in front of it; Outlook's
  Drafts preview just before had not shown it. The likeliest source is
  typing in another app, which Outlook received while it was frontmost for
  the press. Rechecking the body after activation narrows this window but
  cannot close it.
- New messages start from Outlook's default account, whatever folder is
  selected. `accountPicker` exposes no actions, and focusing it and pressing
  Space opens nothing, so the sending account could not be chosen. Replies
  and forwards use the account holding the original.
- Discard asks `Discard draft message?` in a sheet and moves the last saved
  draft to Deleted Items.

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
is running and shows the consent dialog when asked to. It reports
Accessibility with `AXIsProcessTrusted`; `osascript` inherits the
responsibility, so the Outlook bridge runs under mailctl's grant.
