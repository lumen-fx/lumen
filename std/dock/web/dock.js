// The dock on a page: each stack shows its first panel (or the one its
// `active` attribute names) behind a strip of tabs a visitor switches
// between, each split lays out along its `axis`, and each child takes the
// size its `basis` and `grow` attributes declare. Rearranging needs the runtime to move panels, which a page does
// not do yet, so the script functions say so and change nothing.

function truthy(value) {
  return value !== null && !["false", "no", "0"].includes(value.trim());
}

function strip(stack) {
  const panels = Array.from(stack.children).filter(
    (c) => c.classList.contains("lm-dock-panel") && !c.hasAttribute("hidden"),
  );
  if (panels.length === 0) return;
  const wanted = stack.getAttribute("active");
  let shown = panels.find((p) => p.id === wanted) || panels[0];
  const bar = document.createElement("div");
  bar.className = "lm-dock-tabs";
  bar.setAttribute("role", "tablist");
  const show = (panel) => {
    shown = panel;
    for (const p of panels) p.toggleAttribute("data-dock-inactive", p !== panel);
    for (const b of bar.children) {
      b.setAttribute("aria-selected", String(b.dataset.panel === panel.id));
    }
  };
  for (const panel of panels) {
    const b = document.createElement("button");
    b.type = "button";
    b.setAttribute("role", "tab");
    b.dataset.panel = panel.id;
    b.textContent = panel.getAttribute("title") || panel.id;
    b.addEventListener("click", () => show(panel));
    bar.appendChild(b);
  }
  stack.insertBefore(bar, stack.firstChild);
  show(shown);
}

function sizes(root) {
  for (const el of root.querySelectorAll(".lm-dock-split, .lm-dock-stack")) {
    const basis = el.getAttribute("basis");
    const grow = el.getAttribute("grow");
    if (basis) {
      el.style.flex = `${grow || 0} 1 ${parseFloat(basis)}px`;
    } else if (grow) {
      el.style.flexGrow = grow;
    }
    if (el.classList.contains("lm-dock-stack") && truthy(el.getAttribute("collapsed") || "false")) {
      el.toggleAttribute("data-dock-collapsed", true);
    }
  }
}

function layout(dock) {
  if (dock.dataset.dockLaidOut) return;
  dock.dataset.dockLaidOut = "1";
  sizes(dock);
  for (const s of dock.querySelectorAll(".lm-dock-stack")) strip(s);
}

function refuse(name) {
  return () => {
    console.warn(`dock::${name} changes the layout on the desktop only; the page keeps its layout`);
    return false;
  };
}

export function install() {
  // A panel outside any dock is a window the app opens by showing it, which
  // a page does not do yet: it stays out of sight.
  for (const panel of document.querySelectorAll(".lm-dock-panel")) {
    if (!panel.closest(".lm-dock")) panel.hidden = true;
  }
}

export const elements = {
  dock: {
    mount(element) {
      layout(element);
    },
  },
};

export const show = refuse("show");
export const hide = refuse("hide");
export const toggle = refuse("toggle");
export const focus = refuse("focus");
export const activate = refuse("activate");
export const close = refuse("close");
export const move = refuse("move");
export const detach = refuse("detach");
export const detach_group = refuse("detach_group");
export const attach = refuse("attach");
export const collapse = refuse("collapse");
export const maximize = refuse("maximize");
export const restore_maximized = refuse("restore_maximized");
export const restore = refuse("restore");
export const save_layout = refuse("save_layout");
export const apply_layout = refuse("apply_layout");
export const delete_layout = refuse("delete_layout");
export const reset = refuse("reset");
export const forget = refuse("forget");
export function window_of() {
  return 0;
}
export function windows() {
  return [];
}
export function panels() {
  return Array.from(document.querySelectorAll(".lm-dock-panel")).map((p) => p.id);
}
export function is_shown(id) {
  const p = document.getElementById(id);
  return !!p && !p.hidden && !!p.closest(".lm-dock-stack");
}
export function save() {
  return "";
}
export function layouts() {
  return [];
}
