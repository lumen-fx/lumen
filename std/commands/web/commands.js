// The web half of the `lumen-commands` module: commands with keyboard
// shortcuts, menu items that run them, a palette that finds them, and the
// user's key bindings kept in local storage.
//
// A command runs as a `command` CustomEvent on the document, `detail.id`
// naming it, and reaches the app's script as `on_command(id)`. A binding
// that changes (bind, reset, a finished or cancelled record) is a
// `keyschange` event, and `on_keyschange(id, keys)`.

let host;
let persist = true;
const STORE_KEY = "lumen-commands:keymap";
const PALETTE = "commands.palette";
const MAC = /Mac|iPhone|iPad/.test(
  (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "",
);

// Every command, in the order it was declared.
const commands = [];
// The user's bindings, by id: arrays of chords.
let overrides = {};
let recording = null;
let palette = null;

// ---- Chords -------------------------------------------------------------

const NAMED = {
  tab: "Tab",
  enter: "Enter",
  return: "Enter",
  escape: "Escape",
  esc: "Escape",
  backspace: "Backspace",
  space: " ",
  arrowup: "ArrowUp",
  up: "ArrowUp",
  arrowdown: "ArrowDown",
  down: "ArrowDown",
  arrowleft: "ArrowLeft",
  left: "ArrowLeft",
  arrowright: "ArrowRight",
  right: "ArrowRight",
  home: "Home",
  end: "End",
  delete: "Delete",
  del: "Delete",
  plus: "+",
  comma: ",",
  pageup: "PageUp",
  pagedown: "PageDown",
  insert: "Insert",
};
const SHOWN = { " ": "Space", ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right", "+": "Plus" };

function keyNamed(name) {
  if (!name) return null;
  const lower = name.toLowerCase();
  if (NAMED[lower]) return NAMED[lower];
  if (/^f\d+$/.test(lower)) return "F" + lower.slice(1);
  if ([...name].length === 1) return name.toLowerCase();
  return name;
}

// A chord as an author writes one, or null.
export function parseChord(text) {
  text = String(text).trim();
  if (!text) return null;
  const c = { key: null, ctrl: false, alt: false, shift: false, meta: false };
  let mods;
  let key;
  if (text === "+") [mods, key] = ["", "+"];
  else if (text.endsWith("++")) [mods, key] = [text.slice(0, -2), "+"];
  else {
    const at = text.lastIndexOf("+");
    [mods, key] = at < 0 ? ["", text] : [text.slice(0, at), text.slice(at + 1)];
  }
  c.key = keyNamed(key.trim());
  if (!c.key) return null;
  if (mods.trim()) {
    for (const part of mods.split("+")) {
      switch (part.trim().toLowerCase()) {
        case "commandorcontrol":
        case "cmdorctrl":
        case "cmdorcontrol":
        case "commandorctrl":
          c[MAC ? "meta" : "ctrl"] = true;
          break;
        case "control":
        case "ctrl":
          c.ctrl = true;
          break;
        case "shift":
          c.shift = true;
          break;
        case "alt":
        case "option":
          c.alt = true;
          break;
        case "super":
        case "cmd":
        case "command":
        case "meta":
          c.meta = true;
          break;
        default:
          return null;
      }
    }
  }
  return c;
}

// A comma-separated chord list; a comma right after `+` is the comma key.
export function parseList(text) {
  const out = [];
  let current = "";
  for (const ch of String(text)) {
    if (ch === "," && current.trim() && !current.trimEnd().endsWith("+")) {
      out.push(current.trim());
      current = "";
    } else current += ch;
  }
  if (current.trim()) out.push(current.trim());
  const chords = out.map(parseChord);
  return chords.includes(null) ? null : chords;
}

function keyShown(key) {
  if (SHOWN[key]) return SHOWN[key];
  return [...key].length === 1 ? key.toUpperCase() : key;
}

// The chord as text that reads back as the same chord.
export function chordText(c) {
  const parts = [];
  if (c.ctrl) parts.push("Ctrl");
  if (c.alt) parts.push("Alt");
  if (c.shift) parts.push("Shift");
  if (c.meta) parts.push(MAC ? "Cmd" : "Super");
  parts.push(keyShown(c.key));
  return parts.join("+");
}

// The chord as the platform's menus write it.
export function chordLabel(c) {
  if (!MAC) return chordText(c).replace(/\bCmd\b/, "Super");
  let out = "";
  if (c.ctrl) out += "\u2303";
  if (c.alt) out += "\u2325";
  if (c.shift) out += "\u21e7";
  if (c.meta) out += "\u2318";
  return out + keyShown(c.key);
}

function sameChord(a, b) {
  return a.key === b.key && a.ctrl === b.ctrl && a.alt === b.alt && a.shift === b.shift && a.meta === b.meta;
}

// Whether a keydown is the chord.
export function matches(c, e) {
  if (e.ctrlKey !== c.ctrl || e.altKey !== c.alt || e.shiftKey !== c.shift || e.metaKey !== c.meta) {
    return false;
  }
  const got = e.key;
  if (got.toLowerCase() === c.key.toLowerCase()) return true;
  // A Latin chord on a layout whose letters are not: the key where the US
  // layout has it.
  if (/^[a-z0-9]$/.test(c.key) && /[^\x00-\x7f]/.test(got)) {
    const code = /[a-z]/.test(c.key) ? "Key" + c.key.toUpperCase() : "Digit" + c.key;
    return e.code === code;
  }
  return false;
}

// ---- The text-field rule ------------------------------------------------

const NOT_TEXT = ["checkbox", "radio", "button", "submit", "reset", "range", "color", "file", "image"];
const EDITING_KEYS = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End", "Backspace", "Delete"];

// Whether a focused text field keeps the key: plain and Shift-only keys and
// its editing chords. The desktop's `text_field_keeps` keeps the same.
export function fieldKeeps(e) {
  const t = e.target;
  const text =
    t &&
    (t.isContentEditable ||
      t.tagName === "TEXTAREA" ||
      (t.tagName === "INPUT" && !NOT_TEXT.includes(t.type)));
  if (!text) return false;
  if (e.key === "Escape") return false;
  if (EDITING_KEYS.includes(e.key)) return true;
  if (e.ctrlKey || e.metaKey) return ["a", "c", "x", "v", "z", "y"].includes(e.key.toLowerCase());
  return [...e.key].length === 1 || e.key === "Enter" || e.key === "Tab";
}

// ---- The registry -------------------------------------------------------

function find(id) {
  return commands.find((c) => c.id === id);
}

function keysOf(id) {
  if (Object.prototype.hasOwnProperty.call(overrides, id)) return overrides[id];
  const c = find(id);
  return c ? c.defaults : [];
}

function add(id, label, defaults, enabled, element) {
  const command = { id, label: label || id, defaults, enabled, element };
  const at = commands.findIndex((c) => c.id === id);
  if (at >= 0) commands[at] = command;
  else commands.push(command);
  changed();
}

function changed() {
  syncMenuItems();
  if (palette && palette.open) fill();
}

function save() {
  if (!persist) return;
  const keys = {};
  for (const [id, chords] of Object.entries(overrides)) keys[id] = chords.map(chordText);
  try {
    window.localStorage.setItem(STORE_KEY, JSON.stringify({ "lumen-commands": 1, keys }));
  } catch {
    // Storage switched off or full: the binding lasts this visit.
  }
}

function load() {
  if (!persist) return;
  let text = null;
  try {
    text = window.localStorage.getItem(STORE_KEY);
  } catch {
    return;
  }
  if (!text) return;
  try {
    const file = JSON.parse(text);
    if (file["lumen-commands"] !== 1) return;
    for (const [id, list] of Object.entries(file.keys || {})) {
      overrides[id] = (list || []).map(parseChord).filter(Boolean);
    }
  } catch {
    // Not a keymap: left alone.
  }
}

function rebound(id) {
  const keys = keys_(id);
  document.dispatchEvent(new CustomEvent("keyschange", { detail: { id, keys } }));
  host.emit("on_keyschange", id, keys);
}

function bindChords(id, chords) {
  for (const c of commands) {
    if (c.id === id) continue;
    const theirs = keysOf(c.id);
    if (theirs.some((t) => chords.some((x) => sameChord(t, x)))) {
      overrides[c.id] = theirs.filter((t) => !chords.some((x) => sameChord(t, x)));
    }
  }
  overrides[id] = chords;
  save();
  changed();
  rebound(id);
}

function fire(id) {
  if (id === PALETTE) {
    if (palette && palette.open) closePalette();
    else open_palette();
  }
  document.dispatchEvent(new CustomEvent("command", { detail: { id } }));
  host.emit("on_command", id);
}

// ---- Keys ---------------------------------------------------------------

function onKey(e) {
  if (e.repeat || ["Shift", "Control", "Alt", "Meta", "AltGraph", "CapsLock"].includes(e.key)) return;
  if (recording) {
    const id = recording;
    recording = null;
    e.preventDefault();
    if (e.key === "Escape") rebound(id);
    else
      bindChords(id, [
        {
          key: keyNamed(e.key === " " ? "Space" : e.key),
          ctrl: e.ctrlKey,
          alt: e.altKey,
          shift: e.shiftKey,
          meta: e.metaKey,
        },
      ]);
    return;
  }
  if (palette && palette.open && paletteKey(e)) return;
  if (fieldKeeps(e)) return;
  const hit = commands.find((c) => c.enabled && keysOf(c.id).some((k) => matches(k, e)));
  if (hit) {
    e.preventDefault();
    fire(hit.id);
  }
}

// ---- Menu items ---------------------------------------------------------

function syncMenuItems() {
  for (const item of document.querySelectorAll("[data-command]")) {
    const c = find(item.dataset.command);
    if (!c) continue;
    const chord = keysOf(c.id)[0];
    let accel = item.querySelector(":scope > .menu-accel");
    for (const node of [...item.childNodes]) {
      if (node.nodeType === Node.TEXT_NODE) node.remove();
    }
    item.insertBefore(document.createTextNode(c.label), item.firstChild);
    if (chord) {
      if (!accel) {
        accel = document.createElement("span");
        accel.className = "menu-accel";
        item.appendChild(accel);
      }
      accel.textContent = chordLabel(chord);
      item.setAttribute("aria-keyshortcuts", chordText(chord));
    } else {
      if (accel) accel.remove();
      item.removeAttribute("aria-keyshortcuts");
    }
    item.toggleAttribute("disabled", !c.enabled);
    item.setAttribute("aria-disabled", String(!c.enabled));
  }
}

function onClick(e) {
  const item = e.target && e.target.closest && e.target.closest("[data-command]");
  if (!item) return;
  const c = find(item.dataset.command);
  if (c && c.enabled) fire(c.id);
}

// ---- The palette --------------------------------------------------------

function buildPalette(at) {
  const panel = document.createElement("div");
  panel.className = "lm-command-palette";
  panel.setAttribute("popover", "auto");
  const input = document.createElement("input");
  input.className = "lm-command-palette-input";
  input.placeholder = "Type a command";
  const list = document.createElement("div");
  list.className = "lm-command-palette-list";
  list.setAttribute("role", "listbox");
  panel.append(input, list);
  at.appendChild(panel);
  input.addEventListener("input", fill);
  panel.addEventListener("toggle", (e) => {
    if (e.newState === "closed" && palette) {
      palette.open = false;
      if (palette.back && palette.back.focus) palette.back.focus();
    }
  });
  list.addEventListener("click", (e) => {
    const row = e.target.closest("[data-id]");
    if (!row) return;
    closePalette();
    run(row.dataset.id);
  });
  palette = { panel, input, list, rows: [], at: 0, open: false, back: null };
}

// The enabled commands whose label holds every word of `query`.
export function matching(query) {
  const words = String(query).toLowerCase().split(/\s+/).filter(Boolean);
  return commands.filter(
    (c) => c.enabled && c.id !== PALETTE && words.every((w) => c.label.toLowerCase().includes(w)),
  );
}

function fill() {
  const { list, input } = palette;
  list.replaceChildren();
  palette.rows = matching(input.value);
  palette.at = 0;
  if (palette.rows.length === 0) {
    const empty = document.createElement("div");
    empty.className = "lm-command-palette-empty";
    empty.textContent = "No commands";
    list.appendChild(empty);
  }
  for (const c of palette.rows) {
    const row = document.createElement("div");
    row.className = "lm-command-palette-item";
    row.setAttribute("role", "option");
    row.dataset.id = c.id;
    const label = document.createElement("span");
    label.className = "lm-command-palette-label";
    label.textContent = c.label;
    const keys = document.createElement("span");
    keys.className = "lm-command-palette-keys";
    const chord = keysOf(c.id)[0];
    keys.textContent = chord ? chordLabel(chord) : "";
    row.append(label, keys);
    list.appendChild(row);
  }
  mark();
}

function mark() {
  [...palette.list.children].forEach((row, i) => {
    row.classList.toggle("highlighted", i === palette.at);
    row.setAttribute("aria-selected", String(i === palette.at));
  });
}

function paletteKey(e) {
  const n = palette.rows.length;
  switch (e.key) {
    case "ArrowDown":
    case "ArrowUp":
      if (n) palette.at = (palette.at + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
      mark();
      break;
    case "Enter": {
      const c = palette.rows[palette.at];
      closePalette();
      if (c) run(c.id);
      break;
    }
    case "Escape":
      closePalette();
      break;
    default:
      return false;
  }
  e.preventDefault();
  return true;
}

function closePalette() {
  if (palette && palette.open) palette.panel.hidePopover();
}

// ---- The module ---------------------------------------------------------

function readElement(el) {
  const id = el.id || el.getAttribute("data-id") || "";
  if (!id) return;
  const chords = parseList(el.getAttribute("data-keys") || "") || [];
  const enabled = !["false", "0", "no"].includes((el.getAttribute("data-enabled") || "").trim());
  add(id, el.getAttribute("data-label") || id, chords, enabled, el);
}

export function install(given, config) {
  host = given;
  persist = !(config && config.persist === "none");
  load();
  add(PALETTE, "Show all commands", parseList("CommandOrControl+Shift+P"), true, null);
  document.addEventListener("keydown", onKey);
  document.addEventListener("click", onClick, true);
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", () => readAll(document));
  } else {
    readAll(document);
  }
  new MutationObserver(() => syncMenuItems()).observe(document.documentElement, {
    childList: true,
    subtree: true,
  });
}

// Every `<command>` in or under `root`.
function readAll(root) {
  for (const el of root.querySelectorAll('[data-lm-foreign="command"]')) readElement(el);
}

export const elements = {
  commands: {
    mount: readAll,
  },
  command: {
    mount: readElement,
    update(el) {
      readElement(el);
    },
    unmount(el) {
      const id = el.id || el.getAttribute("data-id");
      const at = commands.findIndex((c) => c.id === id && c.element === el);
      if (at >= 0) {
        commands.splice(at, 1);
        changed();
      }
    },
  },
  "command-palette": {
    mount(el) {
      if (!palette) buildPalette(el);
    },
  },
};

export function register(id, label, keys) {
  const chords = parseList(keys || "");
  if (!id || !chords) return false;
  add(id, label, chords, true, null);
  return true;
}

export function unregister(id) {
  const at = commands.findIndex((c) => c.id === id);
  if (at < 0) return false;
  commands.splice(at, 1);
  changed();
  return true;
}

export function run(id) {
  const c = find(id);
  if (!c || !c.enabled) return false;
  fire(id);
  return true;
}

export function set_enabled(id, enabled) {
  const c = find(id);
  if (!c) return false;
  c.enabled = !!enabled;
  changed();
  return true;
}

function keys_(id) {
  return keysOf(id).map(chordText).join(", ");
}
export { keys_ as keys };

export function bind(id, keys) {
  const chords = parseList(keys || "");
  if (!find(id) || !chords) return false;
  bindChords(id, chords);
  return true;
}

export function reset(id) {
  if (!Object.prototype.hasOwnProperty.call(overrides, id)) return false;
  delete overrides[id];
  save();
  changed();
  rebound(id);
  return true;
}

export function record(id) {
  if (!find(id)) return false;
  recording = id;
  return true;
}

export function open_palette() {
  if (!palette) buildPalette(document.body);
  if (palette.open) return;
  palette.back = document.activeElement;
  palette.input.value = "";
  palette.open = true;
  palette.panel.showPopover();
  fill();
  palette.input.focus();
}
