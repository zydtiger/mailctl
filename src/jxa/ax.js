// Accessibility helpers shared by the bridges. src/scripting.rs prepends this
// file to mail.js and outlook.js. They call the AX API through the JXA ObjC
// bridge; `axApp` returns the `{ name, pid, app }` handle the rest take.

ObjC.import("ApplicationServices");
ObjC.import("AppKit");
ObjC.import("CoreGraphics");

const TRUE = $.NSNumber.numberWithBool(true);

function axApp(bundle, name) {
  if (!$.AXIsProcessTrusted()) {
    throw new Error(`mailctl needs Accessibility permission to drive ${name}`);
  }
  const apps = $.NSRunningApplication.runningApplicationsWithBundleIdentifier(bundle);
  if (apps.count === 0) throw new Error(`${name} is not running; open it and try again`);
  const pid = apps.objectAtIndex(0).processIdentifier;
  const app = $.AXUIElementCreateApplication(pid);
  $.AXUIElementSetMessagingTimeout(app, 5);
  return { name, pid, app };
}

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

// Apps label controls with AXDescription, some with AXTitle or a placeholder.
function labelOf(el) {
  return text(el, "AXDescription") || text(el, "AXTitle") || text(el, "AXPlaceholderValue");
}

function set(el, name, value) {
  const error = $.AXUIElementSetAttributeValue(el, $(name), value);
  if (error !== 0) throw new Error(`the app refused to set ${name} (AXError ${error})`);
}

// Some buttons act but still return an error, so callers check the effect
// instead of this result.
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

// Posts a key press to the app's process only, never to the frontmost app.
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

function message(error) {
  return error && error.message ? error.message : String(error);
}

function windows(o) {
  return array(get(o.app, "AXWindows"));
}

function isOpen(o, win) {
  return windows(o).some((other) => other.isEqual(win));
}

// Presses a menu bar item by its path of titles, waiting up to `seconds` for
// it to appear and be enabled: apps rebuild some menus when a window becomes
// key.
function menu(o, path, seconds = 0) {
  const lookup = () => {
    let item = get(o.app, "AXMenuBar");
    for (const name of path) {
      const items = children(item).flatMap((child) => (role(child) === "AXMenu" ? children(child) : [child]));
      item = items.find((child) => text(child, "AXTitle") === name);
      if (!item) return null;
    }
    return text(item, "AXEnabled") === "1" ? item : null;
  };
  press(waitFor(lookup, seconds, `${o.name}'s ${path.join(" > ")} menu item is missing or unavailable`));
}

function button(win, label, required = true) {
  const hit = find(win, (el) => role(el) === "AXButton" && labelOf(el).replace(/, pressed$/, "") === label);
  if (!hit && required) throw new Error(`no ${label} button is showing`);
  return hit;
}

// Runs body with the app active and win raised, then gives the front back to
// the app that had it. Apps ignore some buttons and menu items unless active,
// and the front must be taken in the same run as the press: between runs the
// launching app can take it back.
function inFront(o, win, body) {
  const previous = frontmostPid();
  try {
    set(o.app, "AXFrontmost", TRUE);
    $.AXUIElementPerformAction(win, $("AXRaise"));
    $.AXUIElementSetAttributeValue(win, $("AXMain"), TRUE);
    waitFor(() => text(o.app, "AXFrontmost") === "1", 3, `${o.name} did not come to the front`);
    delay(0.3);
    return body();
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
