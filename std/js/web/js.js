// The web half of the `lumen-js` module: a script's way into the page's own
// JavaScript.
//
// A path is a dotted walk from the global object, so `Math.max` is
// `globalThis.Math.max` and a function found there is called with the object
// it was found on as `this`. Anything a call cannot hand back as a value, such
// as a module, stays here and the script holds a number that names it.

let host;
// URL prefixes `load` accepts beyond the site's own origin, from the `allow`
// list in the dependency's `config` table.
let allow = [];

const held = new Map();
let lastHandle = 0;
const listening = new Map();

export function install(given, config) {
  host = given;
  const listed = config && Array.isArray(config.allow) ? config.allow : [];
  allow = listed.filter((prefix) => typeof prefix === "string" && prefix !== "");
}

// The object a dotted path ends on, and the last name in it.
function walk(path) {
  const names = String(path).split(".");
  if (names.some((name) => name === "")) {
    throw new Error(`\`${path}\` is not a dotted path`);
  }
  let owner = globalThis;
  for (let i = 0; i < names.length - 1; i += 1) {
    owner = owner[names[i]];
    if (owner === null || owner === undefined) {
      throw new Error(`\`${names.slice(0, i + 1).join(".")}\` is ${owner}`);
    }
  }
  return { owner, name: names[names.length - 1] };
}

function isPromise(value) {
  return value !== null && typeof value === "object" && typeof value.then === "function";
}

// Call `owner[name]` with `args`, as the function it has to be.
function apply(owner, name, args, what) {
  const fn = owner[name];
  if (typeof fn !== "function") {
    throw new Error(`${what} is not a function`);
  }
  return fn.apply(owner, Array.isArray(args) ? args : []);
}

// What a handle holds, or an error naming the handle.
function holding(handle) {
  if (!held.has(handle)) {
    throw new Error(`handle ${handle} holds nothing`);
  }
  return held.get(handle);
}

export function call(path, args) {
  const { owner, name } = walk(path);
  const value = apply(owner, name, args, `\`${path}\``);
  if (isPromise(value)) {
    throw new Error(`\`${path}\` returned a promise; call it with js::call_async`);
  }
  return value;
}

export function call_async(path, args) {
  const { owner, name } = walk(path);
  return apply(owner, name, args, `\`${path}\``);
}

export function get(path) {
  const { owner, name } = walk(path);
  return owner[name];
}

export function set(path, value) {
  const { owner, name } = walk(path);
  owner[name] = value;
}

export async function load(url) {
  // Against the document, not this module: a relative URL means the same
  // thing to a script as it does to the page's own links.
  const resolved = new URL(url, document.baseURI);
  const allowed =
    resolved.origin === location.origin ||
    allow.some((prefix) => resolved.href.startsWith(prefix));
  if (!allowed) {
    throw new Error(
      `${resolved.href} is not on this site and no \`allow\` prefix in the lumen-js config matches it`,
    );
  }
  const module = await import(resolved.href);
  lastHandle += 1;
  held.set(lastHandle, module);
  return lastHandle;
}

export function invoke(handle, method, args) {
  const value = apply(holding(handle), method, args, `\`${method}\` on handle ${handle}`);
  if (isPromise(value)) {
    throw new Error(`\`${method}\` returned a promise; call it with js::invoke_async`);
  }
  return value;
}

export function invoke_async(handle, method, args) {
  return apply(holding(handle), method, args, `\`${method}\` on handle ${handle}`);
}

export function release(handle) {
  return held.delete(handle);
}

// What a script is told about a DOM event: its type, and whichever of the
// usual fields it has. A `detail` JSON cannot write is left out.
function describe(event) {
  const out = { type: event.type };
  if ("detail" in event && event.detail !== undefined && event.detail !== null) {
    try {
      out.detail = JSON.parse(JSON.stringify(event.detail));
    } catch {
      // Left out: a value that contains itself, or a BigInt.
    }
  }
  for (const field of ["key", "code", "button", "clientX", "clientY", "deltaX", "deltaY"]) {
    if (field in event) {
      out[field] = event[field];
    }
  }
  const target = event.target;
  if (target && typeof target.value === "string") {
    out.value = target.value;
  }
  if (target && typeof target.id === "string" && target.id !== "") {
    out.target = target.id;
  }
  return out;
}

export function listen(target, event, key) {
  let node;
  if (target === "window") {
    node = window;
  } else if (target === "document") {
    node = document;
  } else {
    node = document.getElementById(target);
  }
  if (!node) {
    return false;
  }
  unlisten(key);
  const listener = (e) => host.emit("on_js_event", key, describe(e));
  node.addEventListener(event, listener);
  listening.set(key, { node, event, listener });
  return true;
}

export function unlisten(key) {
  const entry = listening.get(key);
  if (!entry) {
    return false;
  }
  entry.node.removeEventListener(entry.event, entry.listener);
  listening.delete(key);
  return true;
}
