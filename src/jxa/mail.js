// Mail.app bridge for mailctl. Takes one JSON request in argv[0] and returns
// one JSON response. Messages are addressed by Envelope Index ROWID, which is
// Mail's scripting `id`, inside the mailbox the store says they live in.

function run(argv) {
  const Mail = Application("Mail");
  const request = JSON.parse(argv[0]);
  try {
    return JSON.stringify({ ok: true, value: handlers[request.op](Mail, request) });
  } catch (error) {
    return JSON.stringify({ ok: false, error: String(error) });
  }
}

const SPECIAL = {
  inbox: "inbox",
  sent: "sentMailbox",
  drafts: "draftsMailbox",
  trash: "trashMailbox",
  junk: "junkMailbox",
};

const handlers = {
  accounts(Mail) {
    const special = {};
    for (const [kind, property] of Object.entries(SPECIAL)) {
      for (const mailbox of Mail[property]().mailboxes()) {
        const account = mailbox.account();
        if (account === null) continue;
        const id = account.id();
        (special[id] = special[id] || {})[kind] = mailboxPath(mailbox);
      }
    }
    return Mail.accounts().map((account) => ({
      id: account.id(),
      name: account.name(),
      type: String(account.accountType()),
      emails: account.emailAddresses(),
      enabled: account.enabled(),
      special: special[account.id()] || {},
    }));
  },

  messages(Mail, request) {
    const destination = request.destination ? mailboxRef(Mail, request.destination) : null;
    return request.targets.map((target) => {
      try {
        const message = messageRef(Mail, target);
        message.id(); // Fails here when the message is gone.
        applyAction(Mail, message, request, destination);
        return { id: target.id, ok: true };
      } catch (error) {
        return { id: target.id, ok: false, error: String(error) };
      }
    });
  },

  compose(Mail, request) {
    const message = Mail.OutgoingMessage({
      subject: request.subject,
      content: request.body,
      visible: false,
    });
    Mail.outgoingMessages.push(message);
    if (request.sender) message.sender = request.sender;
    addRecipients(Mail, message, request);
    return finish(message, request.send);
  },

  // Without a compose window Mail leaves reply and forward bodies empty, so
  // the quoted original is written here as plain text.
  reply(Mail, request) {
    const original = messageRef(Mail, request.target);
    const message = original.reply({ openingWindow: false, replyToAll: request.all });
    const quoted = original
      .content()
      .split("\n")
      .map((line) => "> " + line)
      .join("\n");
    const attribution = `On ${formatDate(original.dateSent())}, ${original.sender()} wrote:`;
    message.content = join(request.body, attribution + "\n" + quoted);
    return finish(message, request.send);
  },

  forward(Mail, request) {
    const original = messageRef(Mail, request.target);
    const message = original.forward({ openingWindow: false });
    addRecipients(Mail, message, request);
    const header = [
      "---------- Forwarded message ----------",
      "From: " + original.sender(),
      "Date: " + formatDate(original.dateSent()),
      "Subject: " + original.subject(),
      "To: " + original.toRecipients().map((recipient) => recipient.address()).join(", "),
    ].join("\n");
    message.content = join(request.body, header + "\n\n" + original.content());
    return finish(message, request.send);
  },
};

function applyAction(Mail, message, request, destination) {
  switch (request.action) {
    case "read":
      message.readStatus = true;
      break;
    case "unread":
      message.readStatus = false;
      break;
    case "flag":
      message.flagIndex = request.color;
      break;
    case "unflag":
      message.flaggedStatus = false;
      break;
    case "move":
      Mail.move(message, { to: destination });
      break;
    case "trash":
      Mail.delete(message);
      break;
    default:
      throw new Error("unknown action " + request.action);
  }
}

// A null account means an On My Mac mailbox.
function mailboxRef(Mail, location) {
  const parent = location.account === null ? Mail : Mail.accounts.byId(location.account);
  return parent.mailboxes.byName(location.mailbox);
}

function messageRef(Mail, target) {
  return mailboxRef(Mail, target).messages.byId(target.id);
}

// A top-level mailbox's container is null or, when reached through a unified
// mailbox such as `inbox`, its account.
function mailboxPath(mailbox) {
  const parts = [];
  for (let current = mailbox; current !== null && !isAccount(current); current = current.container()) {
    parts.unshift(current.name());
  }
  return parts.join("/");
}

function isAccount(item) {
  try {
    item.accountType();
    return true;
  } catch (error) {
    return false;
  }
}

function addRecipients(Mail, message, request) {
  const kinds = [
    ["to", "toRecipients", "ToRecipient"],
    ["cc", "ccRecipients", "CcRecipient"],
    ["bcc", "bccRecipients", "BccRecipient"],
  ];
  for (const [field, collection, constructor] of kinds) {
    for (const address of request[field] || []) {
      message[collection].push(Mail[constructor]({ address }));
    }
  }
}

function join(body, quoted) {
  return body ? body + "\n\n" + quoted : quoted;
}

function formatDate(date) {
  return date.toLocaleString("en-US", { dateStyle: "medium", timeStyle: "short" });
}

function finish(message, send) {
  const summary = {
    subject: message.subject(),
    sender: message.sender(),
    to: message.toRecipients().map((recipient) => recipient.address()),
  };
  if (send) {
    // Never report a plain failure here: Mail may have sent it anyway.
    if (!message.send()) {
      throw new Error("Mail did not confirm the send; check the Sent mailbox before trying again");
    }
    return Object.assign(summary, { sent: true });
  }
  // Closing an invisible compose neither saves nor removes it reliably;
  // `save` stores the draft, and Mail drops the compose when it quits.
  message.save();
  return Object.assign(summary, { sent: false });
}
