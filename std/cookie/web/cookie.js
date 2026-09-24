// The web half of the `lumen-cookie` module: `document.cookie`, read and
// written one cookie at a time.
//
// Names and values are percent-encoded on the way in and decoded on the way
// out, so any text survives the trip. A cookie the server marked HttpOnly is
// not in `document.cookie` at all, so nothing here can read or remove it.

const OPTIONS = ["max_age", "path", "domain", "same_site", "secure", "partitioned"];
const SAME_SITE = { lax: "Lax", strict: "Strict", none: "None" };

// Every cookie the page can see, as [name, value] pairs.
function jar() {
  if (document.cookie === "") return [];
  return document.cookie.split("; ").map((pair) => {
    const at = pair.indexOf("=");
    const name = at < 0 ? "" : pair.slice(0, at);
    const value = at < 0 ? pair : pair.slice(at + 1);
    return [decode(name), decode(value)];
  });
}

function decode(text) {
  try {
    return decodeURIComponent(text);
  } catch {
    return text;
  }
}

// An option given as a flag: `true`, or the text "true", since a script's map
// literal may hold only one type of value.
function flag(value) {
  return value === true || value === "true";
}

// The attributes after `name=value`, checked, so a misspelled option is an
// error rather than a cookie that quietly lacks it.
function attributes(options) {
  const given = options ?? {};
  for (const key of Object.keys(given)) {
    if (!OPTIONS.includes(key)) {
      throw new Error(`unknown cookie option \`${key}\`; the options are ${OPTIONS.join(", ")}`);
    }
  }
  let out = `; path=${given.path ?? "/"}`;
  if (given.domain) out += `; domain=${given.domain}`;
  if (given.max_age !== undefined && given.max_age !== null) {
    const seconds = Number(given.max_age);
    if (!Number.isFinite(seconds)) {
      throw new Error(`max_age \`${given.max_age}\` is not a number of seconds`);
    }
    out += `; max-age=${Math.trunc(seconds)}`;
  }
  if (given.same_site) {
    const spelled = SAME_SITE[String(given.same_site).toLowerCase()];
    if (!spelled) {
      throw new Error(`same_site \`${given.same_site}\` is not lax, strict or none`);
    }
    out += `; samesite=${spelled}`;
  }
  if (flag(given.secure)) out += "; secure";
  if (flag(given.partitioned)) out += "; partitioned";
  return out;
}

export function get(name) {
  const found = jar().find(([key]) => key === name);
  return found ? found[1] : null;
}

export function set(name, value, options) {
  document.cookie = `${encodeURIComponent(name)}=${encodeURIComponent(value)}${attributes(options)}`;
  // A cookie set to expire at once is kept by being gone.
  const expiring = options && options.max_age !== undefined && Number(options.max_age) <= 0;
  return expiring ? get(name) === null : get(name) === value;
}

export function remove(name, options) {
  const rest = { ...(options ?? {}) };
  delete rest.max_age;
  document.cookie = `${encodeURIComponent(name)}=${attributes({ ...rest, max_age: 0 })}`;
}

export function keys() {
  return jar().map(([name]) => name);
}
