// The module behind the `echo` test add-on.

let host;

export function install(given) {
  host = given;
  // Before the app's first tick: an event the module raises on its own, and a
  // signal it writes.
  host.emit("on_echo_hello", "install", "hello");
  host.setSignal("from_js", "set by echo");
}

export function shout(text) {
  return text.toUpperCase() + "!";
}

export function later(text, ms) {
  return new Promise((resolve) => setTimeout(() => resolve(text + " later"), ms));
}

export function fail(why) {
  return Promise.reject(new Error(why));
}

export const elements = {
  "echo-view": {
    mount(element) {
      element.textContent = "mounted by echo";
      element.setAttribute("data-echo", "mounted");
    },
    update(element, name, value) {
      element.setAttribute("data-echo-" + name, value);
    },
  },
};
