// The web half of the `lumen-browser` module: what the browser offers a page
// beyond its own document.
//
// Two events arrive without a call behind them:
// `on_browser_message(origin, data, source)` for a message another window
// posted to this one, where `source` is `opener`, `parent`, a popup's name or
// an empty string, and `on_browser_fullscreen_change(id, on)` when fullscreen
// starts or ends. A media query `match_media` asked about reports its changes
// as `on_browser_media_change(query, matches)`.

let host;
const popups = new Map();
const watched = new Map();
let unloadGuard = null;

export function install(given) {
  host = given;
  window.addEventListener("message", (event) => {
    // A message whose data JSON cannot write, such as one another script on
    // the page sent with a transferable in it, is not the app's.
    let data;
    try {
      data = JSON.parse(JSON.stringify(event.data ?? null));
    } catch {
      return;
    }
    host.emit("on_browser_message", event.origin, data, sourceName(event.source));
  });
  document.addEventListener("fullscreenchange", () => {
    const element = document.fullscreenElement;
    host.emit("on_browser_fullscreen_change", element?.id ?? "", element !== null);
  });
}

// What a script calls the window a message came from.
function sourceName(source) {
  if (source === null) return "";
  if (source === window.opener) return "opener";
  if (source === window.parent && window.parent !== window) return "parent";
  for (const [name, popup] of popups) {
    if (popup === source) return name;
  }
  return "";
}

// The window a script names: `opener`, `parent`, or a popup it opened.
function windowNamed(target) {
  if (target === "opener") return window.opener;
  if (target === "parent") return window.parent !== window ? window.parent : null;
  const popup = popups.get(target);
  return popup && !popup.closed ? popup : null;
}

export function open_popup(url, name, features) {
  const popup = window.open(new URL(url, document.baseURI).href, name, features);
  if (!popup) return false;
  popups.set(name, popup);
  return true;
}

export function close_popup(name) {
  const popup = popups.get(name);
  popups.delete(name);
  if (!popup || popup.closed) return false;
  popup.close();
  return true;
}

export function post_message(target, data, origin) {
  const to = windowNamed(target);
  if (!to) return false;
  to.postMessage(data, origin);
  return true;
}

export function close_window() {
  window.close();
}

export async function share(data) {
  if (typeof navigator.share !== "function") {
    throw new Error("this browser cannot share");
  }
  needsGesture("sharing");
  await navigator.share(data ?? {});
  return true;
}

export function can_share(data) {
  return typeof navigator.canShare === "function" && navigator.canShare(data ?? {});
}

export function download_text(filename, mime, text) {
  const url = URL.createObjectURL(new Blob([text], { type: mime || "text/plain" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  link.style.display = "none";
  document.body.append(link);
  link.click();
  link.remove();
  // The download has its own reference by the time the click returns; the
  // URL only has to outlive the task that started it.
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

export function guard_unload(on) {
  if (on && !unloadGuard) {
    unloadGuard = (event) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", unloadGuard);
  } else if (!on && unloadGuard) {
    window.removeEventListener("beforeunload", unloadGuard);
    unloadGuard = null;
  }
}

export function match_media(query) {
  let list = watched.get(query);
  if (!list) {
    list = window.matchMedia(query);
    list.addEventListener("change", (event) => {
      host.emit("on_browser_media_change", query, event.matches);
    });
    watched.set(query, list);
  }
  return list.matches;
}

// The browser opens a picker, a fullscreen view or a share sheet only right
// after the visitor did something. Asking without that is refused here with a
// reason, where the browser would ignore it and never answer.
function needsGesture(what) {
  if (navigator.userActivation && !navigator.userActivation.isActive) {
    throw new Error(`${what} needs a click or key press just before it`);
  }
}

export function pick_file(accept) {
  needsGesture("picking a file");
  return new Promise((resolve, reject) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.addEventListener("change", () => {
      const file = input.files && input.files[0];
      if (!file) {
        reject(new Error("no file was picked"));
        return;
      }
      file.text().then(
        (text) => resolve({ name: file.name, type: file.type, size: file.size, text }),
        reject,
      );
    });
    input.addEventListener("cancel", () => reject(new Error("no file was picked")));
    input.click();
  });
}

export async function request_fullscreen(id) {
  needsGesture("going fullscreen");
  const element = id === "" ? document.documentElement : document.getElementById(id);
  if (!element) {
    throw new Error(`no element has id="${id}"`);
  }
  await element.requestFullscreen();
  return true;
}

export function exit_fullscreen() {
  if (document.fullscreenElement) {
    document.exitFullscreen().catch(() => {});
  }
}

export function is_fullscreen() {
  return document.fullscreenElement !== null;
}
