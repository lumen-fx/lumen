// The `lumen-storage` add-on: local and session storage.
//
// Another tab of the same site changing local storage arrives as
// `on_storage_change(key, new_value, old_value)`. A value that was removed or
// never there is null, and a `clear()` in the other tab arrives with an empty
// key.

let host;

export function install(given) {
  host = given;
  window.addEventListener("storage", (event) => {
    if (event.storageArea !== area(false)) return;
    host.emit("on_storage_change", event.key ?? "", event.newValue, event.oldValue);
  });
}

// The storage area, or null where the browser denies it (a sandboxed frame,
// storage switched off).
function area(session) {
  try {
    return session ? window.sessionStorage : window.localStorage;
  } catch {
    return null;
  }
}

function read(session, key) {
  const store = area(session);
  return store ? store.getItem(key) : null;
}

function write(session, key, value) {
  const store = area(session);
  if (!store) return false;
  try {
    store.setItem(key, value);
    return true;
  } catch {
    return false;
  }
}

function forget(session, key) {
  area(session)?.removeItem(key);
}

function list(session) {
  const store = area(session);
  if (!store) return [];
  const out = [];
  for (let i = 0; i < store.length; i += 1) {
    out.push(store.key(i));
  }
  return out;
}

function empty(session) {
  area(session)?.clear();
}

export function get_item(key) {
  return read(false, key);
}

export function set_item(key, value) {
  return write(false, key, value);
}

export function remove_item(key) {
  forget(false, key);
}

export function keys() {
  return list(false);
}

export function clear() {
  empty(false);
}

export function session_get_item(key) {
  return read(true, key);
}

export function session_set_item(key, value) {
  return write(true, key, value);
}

export function session_remove_item(key) {
  forget(true, key);
}

export function session_keys() {
  return list(true);
}

export function session_clear() {
  empty(true);
}
