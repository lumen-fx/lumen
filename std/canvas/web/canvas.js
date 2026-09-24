// The web half of the `lumen-canvas` module: the same `canvas` functions on
// the same <canvas> element, drawn with Canvas 2D.
//
// A canvas is named by its element's id, which every call takes first. Calls
// are recorded per canvas and replayed onto its 2D context in one batch after
// the script's turn, so a frame's drawing reaches the page together. A canvas
// whose element is not in the page yet keeps what was recorded and draws it
// when the element arrives, as the module does.
//
// What a page cannot do the way the module does: `buffer_load_png` and
// `buffer_save_png` name files, and a page has no file system, so they answer
// 0 and false and say so on the console.

// The HTML canvas's default drawing space, which the module uses too.
const UA_SIZE = [300, 150];
// How many calls a canvas with no element keeps, as the module does.
const UNANSWERED_JOURNAL_CAP = 4096;

// The module's caps: the default, the least and the most an app may set.
const CAPS = {
  region_cap: [1048576, 1024, 16777216],
  buffer_pixel_cap: [16777216, 1024, 67108864],
  buffer_count_cap: [256, 1, 4096],
};
const caps = {};

const surfaces = new Map();
const buffers = new Map();
let lastBuffer = 0;
let flushQueued = false;

export function install(_host, config) {
  for (const [key, [fallback, least, most]] of Object.entries(CAPS)) {
    const asked = config ? config[key] : undefined;
    caps[key] =
      typeof asked === "number" && Number.isFinite(asked)
        ? Math.min(Math.max(Math.trunc(asked), least), most)
        : fallback;
  }
}

function warn(message) {
  console.warn(`lumen-canvas: ${message}`);
}

// ---------------------------------------------------------------------------
// Parsing: the same spellings the module accepts, refused the same way.
// ---------------------------------------------------------------------------

const clamp01 = (v) => Math.min(Math.max(Number(v) || 0, 0), 1);

function rgba(r, g, b, a) {
  return [clamp01(r), clamp01(g), clamp01(b), clamp01(a)];
}

function cssOf([r, g, b, a]) {
  const c = (v) => Math.round(v * 255);
  return `rgba(${c(r)}, ${c(g)}, ${c(b)}, ${a})`;
}

const NAMED = {
  black: "000000",
  silver: "c0c0c0",
  gray: "808080",
  grey: "808080",
  white: "ffffff",
  maroon: "800000",
  red: "ff0000",
  purple: "800080",
  fuchsia: "ff00ff",
  magenta: "ff00ff",
  green: "008000",
  lime: "00ff00",
  olive: "808000",
  yellow: "ffff00",
  navy: "000080",
  blue: "0000ff",
  teal: "008080",
  aqua: "00ffff",
  cyan: "00ffff",
  orange: "ffa500",
};

function parseHex(hex) {
  if (!/^[0-9a-fA-F]*$/.test(hex)) return null;
  let parts;
  if (hex.length === 3 || hex.length === 4) {
    parts = [...hex].map((c) => (parseInt(c, 16) * 17) / 255);
  } else if (hex.length === 6 || hex.length === 8) {
    parts = [];
    for (let i = 0; i < hex.length; i += 2) {
      parts.push(parseInt(hex.slice(i, i + 2), 16) / 255);
    }
  } else {
    return null;
  }
  return rgba(parts[0], parts[1], parts[2], parts.length > 3 ? parts[3] : 1);
}

// A number the way Rust's `f64::from_str` reads one, or null.
function number(text) {
  if (!/^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/.test(text) && !/^[+-]?(inf|infinity|nan)$/i.test(text)) {
    return null;
  }
  return Number(text.replace(/^([+-]?)inf(inity)?$/i, "$1Infinity"));
}

function parseRgbCall(inner) {
  const fields = inner
    .split(/[,/ ]/)
    .map((s) => s.trim())
    .filter((s) => s !== "");
  if (fields.length < 3 || fields.length > 4) return null;
  const channel = (s) => {
    if (s.endsWith("%")) {
      const v = number(s.slice(0, -1).trim());
      return v === null ? null : v / 100;
    }
    const v = number(s);
    return v === null ? null : v / 255;
  };
  let alpha = 1;
  if (fields.length === 4) {
    const s = fields[3];
    alpha = s.endsWith("%") ? number(s.slice(0, -1).trim()) : number(s);
    if (alpha === null) return null;
    if (s.endsWith("%")) alpha /= 100;
  }
  const [r, g, b] = fields.slice(0, 3).map(channel);
  if (r === null || g === null || b === null) return null;
  return rgba(r, g, b, alpha);
}

function parseCss(text) {
  const trimmed = String(text).trim();
  if (trimmed.startsWith("#")) return parseHex(trimmed.slice(1));
  for (const prefix of ["rgba(", "rgb("]) {
    if (trimmed.startsWith(prefix) && trimmed.endsWith(")")) {
      return parseRgbCall(trimmed.slice(prefix.length, -1));
    }
  }
  const name = trimmed.toLowerCase();
  if (name === "transparent") return rgba(0, 0, 0, 0);
  return NAMED[name] ? parseHex(NAMED[name]) : null;
}

function parseFont(text) {
  let weight = 400;
  let size = null;
  const family = [];
  for (const word of String(text).split(/\s+/).filter((w) => w !== "")) {
    if (word.endsWith("px")) {
      const v = number(word.slice(0, -2));
      if (v !== null && v > 0) {
        size = v;
        continue;
      }
    }
    if (size === null) {
      const lower = word.toLowerCase();
      if (lower === "bold") {
        weight = 700;
        continue;
      }
      if (lower === "normal") {
        weight = 400;
        continue;
      }
      if (/^\d+$/.test(word) && Number(word) >= 100 && Number(word) <= 900) {
        weight = Number(word);
        continue;
      }
    }
    family.push(word);
  }
  if (size === null) return null;
  const name = family.join(" ").replace(/^["']+|["']+$/g, "");
  return `${weight} ${size}px ${name === "" ? "sans-serif" : name}`;
}

// ---------------------------------------------------------------------------
// Surfaces: one per id, with the calls recorded against it.
// ---------------------------------------------------------------------------

function surface(id) {
  let s = surfaces.get(id);
  if (!s) {
    s = { element: null, ctx: null, logical: [...UA_SIZE], pending: [], reported: false };
    surfaces.set(id, s);
  }
  return s;
}

// The drawing space the element's markup declares. Its `width` and `height`
// reach the page as CSS, so this is the size the element's style gives it on
// each axis. An axis whose size only follows the bitmap, which is what a
// canvas with no size in its CSS does, takes the default.
function declaredSize(element) {
  const style = getComputedStyle(element);
  const axis = (value, bitmap, fallback) => {
    if (value === `${bitmap}px`) return fallback;
    const px = /^(\d+(\.\d+)?)px$/.exec(value);
    const v = px ? Number(px[1]) : NaN;
    return v > 0 ? v : fallback;
  };
  return [
    axis(style.width, element.width, UA_SIZE[0]),
    axis(style.height, element.height, UA_SIZE[1]),
  ];
}

// True when an axis of the element's box is the bitmap's size, which is what
// it is with nothing in the app's CSS sizing it.
function followsBitmap(element) {
  const style = getComputedStyle(element);
  return style.width === `${element.width}px` && style.height === `${element.height}px`;
}

// Size the element's bitmap for its drawing space at the screen's density,
// which also empties it and resets its state, as a resize does in the module.
function applyBitmap(s) {
  const ratio = window.devicePixelRatio || 1;
  const [w, h] = s.logical;
  s.element.width = Math.max(1, Math.round(w * ratio));
  s.element.height = Math.max(1, Math.round(h * ratio));
  s.ratio = ratio;
  s.ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  if (s.pinned) {
    s.element.style.width = `${w}px`;
    s.element.style.height = `${h}px`;
  }
}

function adopt(element) {
  const id = element.id;
  const s = surface(id);
  if (s.element === element) return s;
  s.element = element;
  s.ctx = element.getContext("2d");
  s.logical = declaredSize(element);
  // A canvas nothing in the app's CSS sizes would take the bitmap's size as
  // its box, and on a dense screen the bitmap is larger than the drawing
  // space. Its box is the drawing space instead, as it is in the module.
  s.pinned = followsBitmap(element);
  applyBitmap(s);
  return s;
}

// The element for `id`, found in the page when its mount has not said so yet.
function element(id) {
  const s = surfaces.get(id);
  if (s && s.element && s.element.isConnected) return s;
  const found = document.getElementById(id);
  if (found instanceof HTMLCanvasElement && found.dataset.lmForeign === "canvas") {
    return adopt(found);
  }
  return null;
}

function record(id, op) {
  surface(id).pending.push(op);
  if (!flushQueued) {
    flushQueued = true;
    queueMicrotask(flush);
  }
}

function flush() {
  flushQueued = false;
  for (const [id, s] of surfaces) {
    if (s.pending.length === 0) continue;
    if (!element(id)) {
      if (s.pending.length > UNANSWERED_JOURNAL_CAP) {
        s.pending.splice(0, s.pending.length - UNANSWERED_JOURNAL_CAP);
      }
      reportLater(id, s);
      continue;
    }
    const ops = s.pending;
    s.pending = [];
    for (const op of ops) {
      replay(s, op);
    }
  }
}

// A canvas drawn on with no element is usually a typo in the id. The element
// may still be on its way, so this waits a frame before saying so, once.
function reportLater(id, s) {
  if (s.reported) return;
  s.reported = true;
  requestAnimationFrame(() => {
    if (!element(id) && s.pending.length > 0) {
      warn(`no <canvas> element has id="${id}", so its drawing is not shown`);
    } else {
      s.reported = false;
    }
  });
}

function replay(s, [name, ...args]) {
  const ctx = s.ctx;
  switch (name) {
    case "resize":
      s.logical = args;
      applyBitmap(s);
      break;
    case "clear":
      applyBitmap(s);
      break;
    case "set_transform": {
      const r = s.ratio;
      const [a, b, c, d, e, f] = args;
      ctx.setTransform(r * a, r * b, r * c, r * d, r * e, r * f);
      break;
    }
    case "reset_transform":
      ctx.setTransform(s.ratio, 0, 0, s.ratio, 0, 0);
      break;
    case "fill_style":
      ctx.fillStyle = cssOf(args[0]);
      break;
    case "stroke_style":
      ctx.strokeStyle = cssOf(args[0]);
      break;
    case "line_width":
      ctx.lineWidth = args[0];
      break;
    case "line_cap":
      ctx.lineCap = args[0];
      break;
    case "line_join":
      ctx.lineJoin = args[0];
      break;
    case "global_alpha":
      ctx.globalAlpha = clamp01(args[0]);
      break;
    case "font":
      ctx.font = args[0];
      break;
    case "draw_buffer": {
      const [handle, x, y, width, height] = args;
      const image = bitmapOf(handle);
      if (!image) break;
      if (width === undefined) {
        ctx.drawImage(image, x, y);
      } else {
        ctx.drawImage(image, x, y, width, height);
      }
      break;
    }
    default:
      // The rest are Canvas 2D calls of the same shape.
      ctx[name](...args);
  }
}

// A buffer's pixels on a canvas of their own, which is what drawImage takes
// through the transform and the alpha. Null for a handle naming nothing.
function bitmapOf(handle) {
  const buffer = buffers.get(handle);
  if (!buffer) return null;
  const staging = document.createElement("canvas");
  staging.width = buffer.width;
  staging.height = buffer.height;
  staging.getContext("2d").putImageData(new ImageData(buffer.data, buffer.width, buffer.height), 0, 0);
  return staging;
}

// ---------------------------------------------------------------------------
// The script surface, in the module's order.
// ---------------------------------------------------------------------------

// The drawing space a script sees: the last pending resize, or the current.
function pendingSize(id) {
  element(id);
  const s = surface(id);
  for (let i = s.pending.length - 1; i >= 0; i -= 1) {
    if (s.pending[i][0] === "resize") return s.pending[i].slice(1);
  }
  return s.logical;
}

const f = Number;

export function width(id) {
  return Math.trunc(pendingSize(id)[0]);
}

export function height(id) {
  return Math.trunc(pendingSize(id)[1]);
}

export function resize(id, w, h) {
  record(id, ["resize", Math.max(0, f(w)), Math.max(0, f(h))]);
}

export function clear(id) {
  record(id, ["clear"]);
}

export function begin_path(id) {
  record(id, ["beginPath"]);
}

export function move_to(id, x, y) {
  record(id, ["moveTo", f(x), f(y)]);
}

export function line_to(id, x, y) {
  record(id, ["lineTo", f(x), f(y)]);
}

export function quad_to(id, cx, cy, x, y) {
  record(id, ["quadraticCurveTo", f(cx), f(cy), f(x), f(y)]);
}

export function bezier_to(id, c1x, c1y, c2x, c2y, x, y) {
  record(id, ["bezierCurveTo", f(c1x), f(c1y), f(c2x), f(c2y), f(x), f(y)]);
}

export function arc(id, x, y, radius, start, end) {
  record(id, ["arc", f(x), f(y), Math.max(0, f(radius)), f(start), f(end)]);
}

export function rect(id, x, y, w, h) {
  record(id, ["rect", f(x), f(y), f(w), f(h)]);
}

export function close_path(id) {
  record(id, ["closePath"]);
}

export function fill(id) {
  record(id, ["fill"]);
}

export function stroke(id) {
  record(id, ["stroke"]);
}

export function fill_rect(id, x, y, w, h) {
  record(id, ["fillRect", f(x), f(y), f(w), f(h)]);
}

export function stroke_rect(id, x, y, w, h) {
  record(id, ["strokeRect", f(x), f(y), f(w), f(h)]);
}

export function set_fill_rgba(id, r, g, b, a) {
  record(id, ["fill_style", rgba(r, g, b, a)]);
}

// A color from CSS text, or false after saying why it was refused.
function styleFrom(id, text, op) {
  const color = parseCss(text);
  if (!color) {
    warn(`'${text}' is not a color this module understands; use a hex, rgb(), or rgba() value`);
    return false;
  }
  record(id, [op, color]);
  return true;
}

export function set_fill_style(id, color) {
  return styleFrom(id, color, "fill_style");
}

export function set_stroke_rgba(id, r, g, b, a) {
  record(id, ["stroke_style", rgba(r, g, b, a)]);
}

export function set_stroke_style(id, color) {
  return styleFrom(id, color, "stroke_style");
}

export function set_line_width(id, w) {
  record(id, ["line_width", f(w)]);
}

// One of `allowed`, or false after saying what `what` takes.
function oneOf(id, text, allowed, op, what) {
  const word = String(text).trim().toLowerCase();
  if (!allowed.includes(word)) {
    warn(`'${text}' is not a ${what}; use ${allowed.slice(0, -1).join(", ")}, or ${allowed[allowed.length - 1]}`);
    return false;
  }
  record(id, [op, word]);
  return true;
}

export function set_line_cap(id, cap) {
  return oneOf(id, cap, ["butt", "round", "square"], "line_cap", "line cap");
}

export function set_line_join(id, join) {
  return oneOf(id, join, ["miter", "round", "bevel"], "line_join", "line join");
}

export function set_global_alpha(id, alpha) {
  record(id, ["global_alpha", f(alpha)]);
}

export function save(id) {
  record(id, ["save"]);
}

export function restore(id) {
  record(id, ["restore"]);
}

export function translate(id, x, y) {
  record(id, ["translate", f(x), f(y)]);
}

export function rotate(id, radians) {
  record(id, ["rotate", f(radians)]);
}

export function scale(id, x, y) {
  record(id, ["scale", f(x), f(y)]);
}

export function reset_transform(id) {
  record(id, ["reset_transform"]);
}

export function set_transform(id, a, b, c, d, e, g) {
  record(id, ["set_transform", f(a), f(b), f(c), f(d), f(e), f(g)]);
}

export function set_font(id, font) {
  const spec = parseFont(font);
  if (!spec) {
    warn(`'${font}' is not a font; it needs a size, as in '16px'`);
    return false;
  }
  record(id, ["font", spec]);
  return true;
}

export function fill_text(id, text, x, y) {
  record(id, ["fillText", String(text), f(x), f(y)]);
}

// ---------------------------------------------------------------------------
// Pixel buffers: straight RGBA, one 0xRRGGBBAA integer per pixel.
// ---------------------------------------------------------------------------

const u32 = (v) => (Math.trunc(Number(v) || 0) % 0x100000000 + 0x100000000) % 0x100000000;
const dim = (v) => {
  const n = Math.trunc(Number(v) || 0);
  return n >= 0 && n <= 0xffffffff ? n : 0;
};

function admit(w, h) {
  if (w === 0 || h === 0) return `a ${w}x${h} buffer holds no pixels`;
  const pixels = w * h;
  if (pixels > caps.buffer_pixel_cap) {
    return `a ${w}x${h} buffer is ${pixels} pixels, over the ${caps.buffer_pixel_cap} the app allows`;
  }
  if (buffers.size >= caps.buffer_count_cap) {
    return `${caps.buffer_count_cap} buffers already exist, which is the cap; free one first`;
  }
  return null;
}

function regionAllowed(w, h) {
  const pixels = w * h;
  if (pixels > caps.region_cap) {
    warn(`a ${w}x${h} region is ${pixels} pixels, over the ${caps.region_cap} the app allows`);
    return false;
  }
  return true;
}

function pixelAt(buffer, x, y) {
  if (x < 0 || y < 0 || x >= buffer.width || y >= buffer.height) return -1;
  return (y * buffer.width + x) * 4;
}

function readPixel(buffer, x, y) {
  const i = pixelAt(buffer, x, y);
  if (i < 0) return 0;
  const d = buffer.data;
  return ((d[i] << 24) >>> 0) + (d[i + 1] << 16) + (d[i + 2] << 8) + d[i + 3];
}

function writePixel(buffer, x, y, value) {
  const i = pixelAt(buffer, x, y);
  if (i < 0) return;
  const d = buffer.data;
  d[i] = (value >>> 24) & 0xff;
  d[i + 1] = (value >>> 16) & 0xff;
  d[i + 2] = (value >>> 8) & 0xff;
  d[i + 3] = value & 0xff;
}

export function buffer_new(w, h) {
  const [bw, bh] = [dim(w), dim(h)];
  const refusal = admit(bw, bh);
  if (refusal) {
    warn(refusal);
    return 0;
  }
  lastBuffer += 1;
  buffers.set(lastBuffer, { width: bw, height: bh, data: new Uint8ClampedArray(bw * bh * 4) });
  return lastBuffer;
}

export function buffer_free(handle) {
  return buffers.delete(handle);
}

export function buffer_width(handle) {
  return buffers.get(handle)?.width ?? 0;
}

export function buffer_height(handle) {
  return buffers.get(handle)?.height ?? 0;
}

export function buffer_get_pixel(handle, x, y) {
  const buffer = buffers.get(handle);
  return buffer ? readPixel(buffer, x, y) : 0;
}

export function buffer_set_pixel(handle, x, y, value) {
  const buffer = buffers.get(handle);
  if (buffer) writePixel(buffer, x, y, u32(value));
}

export function buffer_get_region(handle, x, y, w, h) {
  const [rw, rh] = [dim(w), dim(h)];
  if (!regionAllowed(rw, rh)) return [];
  const buffer = buffers.get(handle);
  if (!buffer) return [];
  const out = [];
  for (let row = 0; row < rh; row += 1) {
    for (let col = 0; col < rw; col += 1) {
      out.push(readPixel(buffer, x + col, y + row));
    }
  }
  return out;
}

export function buffer_put_region(handle, x, y, w, h, pixels) {
  const [rw, rh] = [dim(w), dim(h)];
  if (!regionAllowed(rw, rh)) return;
  const buffer = buffers.get(handle);
  if (!buffer || !Array.isArray(pixels)) return;
  for (let row = 0; row < rh; row += 1) {
    for (let col = 0; col < rw; col += 1) {
      const at = row * rw + col;
      if (at >= pixels.length) return;
      writePixel(buffer, x + col, y + row, u32(pixels[at]));
    }
  }
}

export function buffer_fill_rect(handle, x, y, w, h, value) {
  const buffer = buffers.get(handle);
  if (!buffer) return;
  const left = Math.max(x, 0);
  const top = Math.max(y, 0);
  const right = Math.min(x + dim(w), buffer.width);
  const bottom = Math.min(y + dim(h), buffer.height);
  const pixel = u32(value);
  for (let row = top; row < bottom; row += 1) {
    for (let col = left; col < right; col += 1) {
      writePixel(buffer, col, row, pixel);
    }
  }
}

export function buffer_load_png(path) {
  warn(`buffer_load_png('${path}'): a page has no files to read; load images with the js module instead`);
  return 0;
}

export function buffer_save_png(handle, path) {
  warn(`buffer_save_png(${handle}, '${path}'): a page has no files to write`);
  return false;
}

export function draw_buffer(id, handle, x, y) {
  record(id, ["draw_buffer", handle, f(x), f(y)]);
}

export function draw_buffer_scaled(id, handle, x, y, w, h) {
  record(id, ["draw_buffer", handle, f(x), f(y), f(w), f(h)]);
}

export const elements = {
  canvas: {
    mount(el) {
      if (el.id === "") return;
      adopt(el);
      flush();
    },
    unmount(el) {
      const s = surfaces.get(el.id);
      if (s && s.element === el) {
        surfaces.delete(el.id);
      }
    },
  },
};
