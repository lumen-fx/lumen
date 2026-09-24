// The `lumen-svg` add-on: an <svg-view> element whose drawing a script sets
// as markup and then edits by selector.
//
// Markup is parsed as an SVG document, never as HTML, and cleaned before it
// reaches the page: <script> and <foreignObject> elements, `on...` event
// attributes, and attributes naming a `javascript:` URL are removed. A
// call naming an element that is not in the page yet is kept, and applied
// when the element arrives.

const SVG_NS = "http://www.w3.org/2000/svg";
const views = new Map();
const pending = new Map();

// Element names and attributes that can run code.
const DROPPED_ELEMENTS = new Set(["script", "foreignobject"]);

// An event attribute, or any value naming a `javascript:` URL: an `href`,
// and equally an <animate> or <set> whose `to` or `values` would put one in
// an `href` later. Whitespace and control characters inside the scheme are
// ignored, as a browser ignores them.
function unsafeAttribute(name, value) {
  if (name.toLowerCase().startsWith("on")) return true;
  return /javascript:/i.test(String(value).replace(/[\s\u0000-\u001f]/g, ""));
}

function clean(node) {
  for (const child of [...node.children]) {
    if (DROPPED_ELEMENTS.has(child.localName.toLowerCase())) {
      child.remove();
      continue;
    }
    clean(child);
  }
  for (const attribute of [...node.attributes]) {
    if (unsafeAttribute(attribute.name, attribute.value)) {
      node.removeAttribute(attribute.name);
    }
  }
}

// The <svg> root `markup` parses to, cleaned, or an error saying why not.
function parse(markup) {
  const doc = new DOMParser().parseFromString(markup, "image/svg+xml");
  const error = doc.querySelector("parsererror");
  if (error) {
    throw new Error(`the markup is not well-formed SVG: ${error.textContent.trim()}`);
  }
  const root = doc.documentElement;
  if (root.namespaceURI !== SVG_NS || root.localName !== "svg") {
    throw new Error("the markup's root element is not <svg>");
  }
  clean(root);
  return document.importNode(root, true);
}

function view(id) {
  const known = views.get(id);
  if (known && known.isConnected) return known;
  const found = document.getElementById(id);
  if (found && found.dataset.lmForeign === "svg-view") {
    views.set(id, found);
    return found;
  }
  return null;
}

function queue(id, apply) {
  const list = pending.get(id) ?? [];
  list.push(apply);
  pending.set(id, list);
}

function applyMarkup(element, root) {
  element.replaceChildren(root);
}

function applyAttribute(element, selector, name, value) {
  const matched = element.querySelectorAll(selector);
  for (const node of matched) {
    node.setAttribute(name, value);
  }
  return matched.length;
}

export function set_markup(id, markup) {
  const root = parse(markup);
  const element = view(id);
  if (element) {
    applyMarkup(element, root);
  } else {
    queue(id, (target) => applyMarkup(target, root));
  }
}

export function set_attribute(id, selector, name, value) {
  if (unsafeAttribute(name, value)) {
    throw new Error(`\`${name}\` can run code, so svg::set_attribute does not set it`);
  }
  const element = view(id);
  if (element) {
    return applyAttribute(element, selector, name, value);
  }
  queue(id, (target) => applyAttribute(target, selector, name, value));
  return 0;
}

export const elements = {
  "svg-view": {
    mount(element) {
      if (element.id === "") return;
      views.set(element.id, element);
      const waiting = pending.get(element.id);
      pending.delete(element.id);
      for (const apply of waiting ?? []) {
        apply(element);
      }
    },
    unmount(element) {
      if (views.get(element.id) === element) {
        views.delete(element.id);
      }
    },
  },
};
