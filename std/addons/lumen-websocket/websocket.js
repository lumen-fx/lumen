// The `lumen-websocket` add-on: WebSocket connections a script names by key.
//
// Every event arrives with the key first: `on_ws_open(key)`,
// `on_ws_message(key, text)`, `on_ws_close(key, code, reason, clean)` and
// `on_ws_error(key, message)`.

let host;
const sockets = new Map();
const STATES = ["connecting", "open", "closing", "closed"];

export function install(given) {
  host = given;
}

export function open(key, url) {
  const current = sockets.get(key);
  if (current && current.readyState <= WebSocket.OPEN) {
    return false;
  }
  // A relative URL means the site's own server, reached over the socket
  // scheme that matches the page's.
  const resolved = new URL(url, location.href);
  if (resolved.protocol === "http:") {
    resolved.protocol = "ws:";
  } else if (resolved.protocol === "https:") {
    resolved.protocol = "wss:";
  }
  const socket = new WebSocket(resolved.href);
  socket.binaryType = "arraybuffer";
  sockets.set(key, socket);
  // A socket replaced under the same key says nothing more.
  const live = () => sockets.get(key) === socket;
  socket.addEventListener("open", () => {
    if (live()) host.emit("on_ws_open", key);
  });
  socket.addEventListener("message", (event) => {
    if (!live()) return;
    if (typeof event.data === "string") {
      host.emit("on_ws_message", key, event.data);
    } else {
      host.emit(
        "on_ws_error",
        key,
        "a binary frame arrived; this add-on delivers text frames only",
      );
    }
  });
  socket.addEventListener("error", () => {
    if (live()) host.emit("on_ws_error", key, "the connection failed");
  });
  socket.addEventListener("close", (event) => {
    if (!live()) return;
    sockets.delete(key);
    host.emit("on_ws_close", key, event.code, event.reason, event.wasClean);
  });
  return true;
}

export function send(key, text) {
  const socket = sockets.get(key);
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    return false;
  }
  socket.send(text);
  return true;
}

export function close(key, code, reason) {
  const socket = sockets.get(key);
  if (!socket || socket.readyState >= WebSocket.CLOSING) {
    return false;
  }
  socket.close(code, reason);
  return true;
}

export function state(key) {
  const socket = sockets.get(key);
  return socket ? STATES[socket.readyState] : "closed";
}
