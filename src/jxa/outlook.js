// Microsoft Outlook bridge for `mailctl meeting`. Takes one JSON request in
// argv[0] and returns one JSON response, like mail.js.
//
// The new Outlook for Mac has no scripting dictionary, so this drives its
// windows through the Accessibility API (ApplicationServices through the ObjC
// bridge). Keys go to Outlook's process only (CGEventPostToPid), never to the
// frontmost app. Outlook ignores its Send, Save, and Discard buttons unless it
// is the active app, so `submit` brings it to the front for that press and
// then gives the front back. docs/architecture.md records the behavior this
// relies on.

ObjC.import("ApplicationServices");
ObjC.import("AppKit");
ObjC.import("CoreGraphics");

const BUNDLE = "com.microsoft.Outlook";
const EVENT_WINDOW = " • Calendar • ";
const ATTENDEES = ["Add required people", "Required Attendees"];
const LOCATION = ["Add a location", "Locations"];
const KEY = { a: 0, tab: 48, delete: 51, escape: 53, return: 36 };
const TRUE = $.NSNumber.numberWithBool(true);
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function run(argv) {
  const request = JSON.parse(argv[0]);
  try {
    if (!$.AXIsProcessTrusted()) {
      throw new Error("mailctl needs Accessibility permission to drive Outlook");
    }
    return JSON.stringify({ ok: true, value: handlers[request.op](outlook(), request) });
  } catch (error) {
    return JSON.stringify({ ok: false, error: message(error) });
  }
}

const handlers = {
  create(o, request) {
    const win = openWindow(o, () => menu(o, ["File", "New", "Event"]), "a new event window");
    return leftOpenOnError("the unsent meeting", () => {
      set(field(win, ["Subject"]), "AXValue", $(request.subject));
      setTimes(win, request.start, request.end);
      for (const address of request.attendees) addToken(o, win, ATTENDEES, address);
      if (request.location) addToken(o, win, LOCATION, request.location);
      // Outlook can turn Teams on when the first attendee is added.
      delay(0.5);
      setTeams(win, request.teams);
      const state = editorState(win);
      expect(state.subject === request.subject, `the subject reads "${state.subject}"`);
      expect(sameAddresses(state.attendees, request.attendees), `the attendees read ${state.attendees.join(", ")}`);
      expect(state.location === (request.location || null), `the location reads "${state.location}"`);
      expect(state.teams === request.teams, "the Teams meeting switch did not change");
      // Without send, the meeting stays open in Outlook for the user to review.
      return Object.assign(state, request.send ? submit(o, win, "Send", state.location === null) : { open: true });
    });
  },

  // Dry runs of update and cancel: finds the meeting without opening it.
  find(o, request) {
    return { found: findMeeting(o, request.subject, request.start).description };
  },

  update(o, request) {
    const meeting = findMeeting(o, request.subject, request.start);
    const win = openMeeting(o, meeting, request.subject);
    return leftOpenOnError("the meeting", () => {
      edit(win);
      const before = editorState(win);
      setTimes(win, request.new_start, request.new_end);
      if (request.location !== null) {
        clearTokens(o, win, LOCATION);
        if (request.location) addToken(o, win, LOCATION, request.location);
      }
      const state = editorState(win);
      expect(state.subject === request.subject, `the subject reads "${state.subject}"`);
      expect(sameAddresses(state.attendees, before.attendees), "the attendees changed");
      if (request.location !== null) {
        expect(state.location === (request.location || null), `the location reads "${state.location}"`);
      }
      return Object.assign(state, submit(o, win, "Send", state.location === null));
    });
  },

  cancel(o, request) {
    const meeting = findMeeting(o, request.subject, request.start);
    const win = openMeeting(o, meeting, request.subject);
    return leftOpenOnError("the meeting", () => {
      press(button(win, "Cancel"));
      waitFor(() => button(win, "Send cancellation", false), 5, "Outlook offered no Send cancellation; only meetings with invitees can be cancelled");
      if (request.message) {
        const note = find(win, (el) => role(el) === "AXTextArea" && labelOf(el) === "Optional message to invitees");
        expect(note, "Outlook showed no message box for the cancellation");
        set(note, "AXFocused", TRUE);
        delay(0.3);
        set(note, "AXSelectedText", $(request.message));
      }
      return Object.assign({ subject: request.subject, found: meeting.description }, submit(o, win, "Send cancellation", false));
    });
  },
};

function outlook() {
  const apps = $.NSRunningApplication.runningApplicationsWithBundleIdentifier(BUNDLE);
  if (apps.count === 0) throw new Error("Microsoft Outlook is not running; open it and try again");
  const pid = apps.objectAtIndex(0).processIdentifier;
  const app = $.AXUIElementCreateApplication(pid);
  $.AXUIElementSetMessagingTimeout(app, 5);
  return { pid, app };
}

// --- Accessibility helpers ---------------------------------------------------

function get(el, name) {
  const ref = Ref();
  return $.AXUIElementCopyAttributeValue(el, $(name), ref) === 0 ? ObjC.castRefToObject(ref[0]) : null;
}

function text(el, name) {
  const value = get(el, name);
  return value === null ? "" : value.description.js;
}

function array(value) {
  const out = [];
  for (let i = 0; value !== null && i < value.count; i++) out.push(value.objectAtIndex(i));
  return out;
}

function children(el) {
  return array(get(el, "AXChildren"));
}

function role(el) {
  return text(el, "AXRole");
}

// Outlook labels controls with AXDescription, a few with AXTitle or a
// placeholder.
function labelOf(el) {
  return text(el, "AXDescription") || text(el, "AXTitle") || text(el, "AXPlaceholderValue");
}

function set(el, name, value) {
  const error = $.AXUIElementSetAttributeValue(el, $(name), value);
  if (error !== 0) throw new Error(`Outlook refused to set ${name} (AXError ${error})`);
}

// Some Outlook buttons act but still return an error, so callers check the
// effect instead of this result.
function press(el) {
  return $.AXUIElementPerformAction(el, $("AXPress"));
}

function findAll(root, test, depth = 12) {
  const hits = [];
  const walk = (el, level) => {
    for (const child of children(el)) {
      if (test(child)) hits.push(child);
      if (level < depth) walk(child, level + 1);
    }
  };
  walk(root, 0);
  return hits;
}

function find(root, test, depth = 12) {
  let hit = null;
  const walk = (el, level) => {
    for (const child of children(el)) {
      if (hit) return;
      if (test(child)) hit = child;
      else if (level < depth) walk(child, level + 1);
    }
  };
  walk(root, 0);
  return hit;
}

function key(o, code, flags = 0) {
  for (const down of [true, false]) {
    const event = $.CGEventCreateKeyboardEvent($(), code, down);
    if (flags) $.CGEventSetFlags(event, flags);
    $.CGEventPostToPid(o.pid, event);
    delay(0.05);
  }
}

function waitFor(test, seconds, failure) {
  for (let elapsed = 0; elapsed <= seconds; elapsed += 0.25) {
    const value = test();
    if (value) return value;
    delay(0.25);
  }
  throw new Error(failure);
}

function expect(condition, failure) {
  if (!condition) throw new Error("Outlook did not take the change: " + failure);
}

function message(error) {
  return error && error.message ? error.message : String(error);
}

function leftOpenOnError(what, body) {
  try {
    return body();
  } catch (error) {
    throw new Error(`${message(error)}; ${what} is left open in Outlook`);
  }
}

// --- Outlook windows and menus -----------------------------------------------

function windows(o) {
  return array(get(o.app, "AXWindows"));
}

function isOpen(o, win) {
  return windows(o).some((other) => other.isEqual(win));
}

function openWindow(o, action, what) {
  const before = windows(o);
  action();
  return waitFor(
    () => windows(o).find((win) => !before.some((old) => old.isEqual(win)) && text(win, "AXTitle").includes(EVENT_WINDOW)),
    10,
    `Outlook did not open ${what}`,
  );
}

function menu(o, path) {
  let item = get(o.app, "AXMenuBar");
  for (const name of path) {
    const items = children(item).flatMap((child) => (role(child) === "AXMenu" ? children(child) : [child]));
    item = items.find((child) => text(child, "AXTitle") === name);
    if (!item) throw new Error(`Outlook has no ${path.join(" > ")} menu item`);
  }
  if (text(item, "AXEnabled") !== "1") throw new Error(`Outlook's ${path.join(" > ")} menu item is unavailable`);
  press(item);
}

function button(win, label, required = true) {
  const hit = find(win, (el) => role(el) === "AXButton" && labelOf(el).replace(/, pressed$/, "") === label);
  if (!hit && required) throw new Error(`Outlook shows no ${label} button`);
  return hit;
}

function field(win, labels) {
  const hit = find(win, (el) => role(el) === "AXTextField" && labels.includes(text(el, "AXDescription")));
  if (!hit) throw new Error(`Outlook shows no ${labels[0]} field`);
  return hit;
}

// --- Event editor ------------------------------------------------------------

function picker(win, title) {
  const hit = find(win, (el) => role(el) === "AXDateTimeArea" && text(el, "AXTitle") === title);
  if (!hit) throw new Error(`Outlook shows no ${title} picker`);
  return hit;
}

function dateOf(win, title) {
  const value = get(picker(win, title), "AXValue");
  return value === null ? null : Math.round(value.timeIntervalSince1970);
}

// Writing an NSDate through the raw AX API updates Outlook's model; System
// Events cannot convert dates, and typing needs the keyboard.
function setTimes(win, start, end) {
  if (start !== null) {
    for (const title of ["Start date", "Start time"]) {
      set(picker(win, title), "AXValue", $.NSDate.dateWithTimeIntervalSince1970(start));
      delay(0.3);
    }
    expect(dateOf(win, "Start time") === start, "the start time");
  }
  if (end !== null) {
    set(picker(win, "End time"), "AXValue", $.NSDate.dateWithTimeIntervalSince1970(end));
    delay(0.3);
    expect(dateOf(win, "End time") === end, "the end time");
  }
}

// Recipient and location fields turn text into tokens. Return would pick the
// first directory suggestion, such as a room for a location, so Escape closes
// the suggestions and Tab turns the typed text itself into the token.
function addToken(o, win, labels, value) {
  const before = tokens(win, labels);
  set(field(win, labels), "AXFocused", TRUE);
  delay(0.3);
  set(field(win, labels), "AXSelectedText", $(value));
  delay(0.5);
  key(o, KEY.escape);
  delay(0.3);
  key(o, KEY.tab);
  const after = waitFor(() => {
    const now = tokens(win, labels);
    return now.length > before.length ? now : null;
  }, 5, `Outlook did not accept "${value}" in ${labels[0]}`);
  const added = after.slice(before.length);
  if (added.length !== 1 || added[0].toLowerCase() !== value.toLowerCase()) {
    throw new Error(`Outlook turned "${value}" into "${added.join(", ")}" in ${labels[0]}`);
  }
}

function clearTokens(o, win, labels) {
  if (tokens(win, labels).length === 0) return;
  set(field(win, labels), "AXFocused", TRUE);
  delay(0.3);
  key(o, KEY.a, $.kCGEventFlagMaskCommand);
  key(o, KEY.delete);
  waitFor(() => tokens(win, labels).length === 0, 5, `Outlook did not clear ${labels[0]}`);
}

// A token field's tokens are the static texts next to it.
function tokens(win, labels) {
  const parent = get(field(win, labels), "AXParent");
  return findAll(parent, (el) => role(el) === "AXStaticText", 4)
    .map((el) => text(el, "AXValue"))
    .filter((value) => value !== "");
}

function teamsSwitch(win) {
  return find(win, (el) => role(el) === "AXCheckBox" && text(el, "AXTitle") === "Teams meeting");
}

function setTeams(win, on) {
  const control = teamsSwitch(win);
  if (!control) {
    if (on) throw new Error("Outlook shows no Teams meeting switch for this calendar");
    return;
  }
  if ((text(control, "AXValue") === "1") !== on) press(control);
  waitFor(() => (text(teamsSwitch(win), "AXValue") === "1") === on, 3, "Outlook did not change the Teams meeting switch");
}

function editorState(win) {
  const calendar = find(win, (el) => role(el) === "AXPopUpButton" && /^Calendar \(/.test(text(el, "AXDescription")));
  // "Calendar (someone@example.edu), From"
  const owner = calendar ? /^Calendar \(([^)]*)\)/.exec(text(calendar, "AXDescription")) : null;
  const location = tokens(win, LOCATION);
  const teams = teamsSwitch(win);
  return {
    subject: text(field(win, ["Subject"]), "AXValue"),
    calendar: owner ? owner[1] : null,
    start: dateOf(win, "Start time"),
    end: dateOf(win, "End time"),
    attendees: tokens(win, ATTENDEES),
    location: location.length ? location.join("; ") : null,
    teams: teams ? text(teams, "AXValue") === "1" : false,
  };
}

function sameAddresses(a, b) {
  const norm = (list) => list.map((value) => value.toLowerCase()).sort().join("\n");
  return norm(a) === norm(b);
}

// A meeting opened from the calendar shows read-only; Edit switches the same
// window to the editor. Edit acts but reports an AX error.
function edit(win) {
  press(button(win, "Edit"));
  waitFor(() => find(win, (el) => role(el) === "AXDateTimeArea"), 5, "Outlook did not open the meeting for editing");
}

// --- Finding an existing meeting ---------------------------------------------

// Outlook's event search lists each match as a row whose cell describes it:
// "<subject>, starts on Nov 27, 2026 at 10:00 AM to 10:30 AM, you are the
// organizer, show as Busy".
function findMeeting(o, subject, start) {
  menu(o, ["View", "Go To", "Calendar"]);
  delay(0.5);
  const main = windows(o).find((win) => !text(win, "AXTitle").includes(EVENT_WINDOW));
  if (!main) throw new Error("Outlook has no main window");
  const search = find(main, (el) => text(el, "AXSubrole") === "AXSearchField", 4);
  if (!search) throw new Error("Outlook shows no search field");
  // The search field takes a written AXValue, which replaces any earlier
  // query; Command-A does not reliably select it. A search with no results
  // keeps the previous results listed, so the old search is ended first.
  set(search, "AXFocused", TRUE);
  delay(0.3);
  set(search, "AXValue", $(""));
  key(o, KEY.escape);
  delay(1);
  set(search, "AXFocused", TRUE);
  delay(0.3);
  set(search, "AXValue", $(subject));
  delay(0.3);
  key(o, KEY.return);

  // Results arrive per calendar, so an invitation in another calendar can be
  // listed before the user's own meeting: wait for the organizer's row.
  const prefix = `${subject}, starts on ${outlookTime(start)} `;
  const organizer = (match) => match.description.includes("you are the organizer");
  let matches = [];
  const found = () => {
    const table = find(main, (el) => role(el) === "AXTable" && text(el, "AXDescription") === "Calendar Events");
    matches = table ? listed(table) : [];
    return matches.some(organizer);
  };
  try {
    waitFor(found, 10, "");
  } catch (error) {
    if (matches.length === 0) throw new Error(`Outlook found no meeting "${subject}" starting ${outlookTime(start)}`);
    throw new Error(`you are not the organizer of "${subject}" at ${outlookTime(start)}`);
  }
  const organized = matches.filter(organizer);
  if (organized.length > 1) throw new Error(`Outlook lists ${organized.length} meetings "${subject}" at ${outlookTime(start)}`);
  return organized[0];

  function listed(table) {
    return children(table)
      .filter((row) => role(row) === "AXRow")
      // Outlook puts U+202F before AM/PM.
      .map((row) => ({
        row,
        description: children(row)
          .map((cell) => text(cell, "AXDescription"))
          .join(" ")
          .replace(/[  ]/g, " "),
      }))
      .filter((match) => match.description.startsWith(prefix));
  }
}

// Opens the meeting read-only. Outlook offers Cancel only on a meeting with
// invitees; a plain event shows Delete instead and is closed unchanged.
function openMeeting(o, meeting, subject) {
  // Outlook would only bring an open window forward, and it may hold edits.
  if (windows(o).some((win) => text(win, "AXTitle").startsWith(subject + EVENT_WINDOW))) {
    throw new Error(`a window for "${subject}" is already open in Outlook; close it and try again`);
  }
  // File > Open > Current Item often does nothing after a search; Return on
  // the focused results table opens the selected row.
  set(get(meeting.row, "AXParent"), "AXFocused", TRUE);
  delay(0.3);
  set(meeting.row, "AXSelected", TRUE);
  delay(0.5);
  const win = openWindow(o, () => key(o, KEY.return), "the meeting");
  if (!text(win, "AXTitle").startsWith(subject + EVENT_WINDOW)) {
    throw new Error(`Outlook opened "${text(win, "AXTitle")}" instead of "${subject}"; that window is left open`);
  }
  // While it loads, the window shows other buttons, including a Cancel.
  waitFor(() => button(win, "Edit", false), 10, `Outlook did not finish opening "${subject}"; that window is left open`);
  delay(0.5);
  if (!button(win, "Cancel", false)) {
    const close = get(win, "AXCloseButton");
    if (close !== null) press(close);
    throw new Error(`"${subject}" has no invitees, so Outlook sends nothing for it`);
  }
  return win;
}

function outlookTime(seconds) {
  const date = new Date(seconds * 1000);
  const hour = date.getHours();
  const minute = String(date.getMinutes()).padStart(2, "0");
  return `${MONTHS[date.getMonth()]} ${date.getDate()}, ${date.getFullYear()} at ${hour % 12 || 12}:${minute} ${hour < 12 ? "AM" : "PM"}`;
}

// --- Sending -----------------------------------------------------------------

// Presses the submit button with Outlook in front, then waits for the window
// to close. Outlook asks before sending a meeting without a location; that
// prompt is answered only when no location was requested. Any other prompt
// is reported and left for the user.
function submit(o, win, label, confirmNoLocation) {
  const previous = frontmostPid();
  try {
    set(o.app, "AXFrontmost", TRUE);
    $.AXUIElementPerformAction(win, $("AXRaise"));
    waitFor(() => text(o.app, "AXFrontmost") === "1", 3, "Outlook did not come to the front");
    press(button(win, label));
    for (let elapsed = 0; elapsed < 20; elapsed += 0.25) {
      delay(0.25);
      if (!isOpen(o, win)) return { sent: true };
      const sheet = children(win).find((el) => role(el) === "AXSheet");
      if (!sheet) continue;
      const prompt = findAll(sheet, (el) => role(el) === "AXStaticText", 4).map((el) => text(el, "AXValue")).join(" ");
      const confirm = button(sheet, "Send invitation", false);
      if (!(confirmNoLocation && confirm && /location/i.test(prompt))) return { sent: false, prompt };
      press(confirm);
      confirmNoLocation = false;
    }
    return { sent: false, prompt: "Outlook did not close the meeting window; check Outlook before trying again" };
  } finally {
    if (previous !== null && previous !== o.pid) {
      $.AXUIElementSetAttributeValue($.AXUIElementCreateApplication(previous), $("AXFrontmost"), TRUE);
    }
  }
}

function frontmostPid() {
  const app = get($.AXUIElementCreateSystemWide(), "AXFocusedApplication");
  if (app === null) return null;
  const pid = Ref();
  return $.AXUIElementGetPid(app, pid) === 0 ? pid[0] : null;
}
