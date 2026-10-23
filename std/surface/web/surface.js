// The web half of the `lumen-surface` module: the <surface> placeholder.
//
// A page cannot show another process's GPU frames, so the element is marked
// unsupported and every function answers as a surface with no producer does.
// The element keeps the class and attribute a desktop surface carries in the
// same state, so one stylesheet styles both.

export function endpoint(_id) {
  return "";
}

export function state(id) {
  return document.getElementById(id) ? "unsupported" : "";
}

export function frame_info(_id) {
  return {};
}

export function frame_meta(_id) {
  return [];
}

export function request_frame(_id) {}

export function send(_id, _message) {
  return false;
}

export function disconnect(_id) {}

export const elements = {
  surface: {
    mount(element) {
      element.setAttribute("state", "unsupported");
      element.classList.add("surface-unsupported");
    },
  },
};
