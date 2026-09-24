#!/usr/bin/env python3
"""Open the first-party modules fixture in headless Chrome and check each leg.

web-page-smoke.sh serves web/tests/fixtures/std-modules and runs this against
it. The fixture calls into the web half of every module under std/ and writes
each answer into a label; this drives the page over WebDriver, does what only a
visitor or a second tab can do (dispatch an event, post a message, change
local storage from elsewhere), and waits for every label to read what it
should.

WebDriver rather than `--dump-dom`: a dumped page gets a few animation frames
in all, and a WebSocket round trip or an event from another tab arrives after
them. Here the page runs in real time and each check waits for its answer.

It also runs the WebSocket echo server the fixture connects to, on the port
the fixture names, with nothing beyond the standard library.

    web-addons-smoke.py URL

Environment: CHROME_BIN (default google-chrome) and CHROMEDRIVER (default
chromedriver on PATH), which have to be the same major version.
"""

import base64
import hashlib
import json
import os
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

ECHO_PORT = 8798
DRIVER_PORT = 9517
WAIT_SECONDS = 30

# Each label, and what it reads once its leg worked.
EXPECTED = {
    "js-call": "9",
    "js-async": "promise: resolved",
    "js-get": "41",
    "js-import": "allowed: 42",
    "js-denied": "denied",
    "js-event": "ping: 7 true",
    "ws-state": "open",
    "ws-message": "echo: hello over ws",
    "ws-close": "echo: 1000 done",
    "storage-local": "hi",
    "storage-session": "first",
    "storage-event": "from_other_tab=yes",
    "cookie-read": "oat; meal",
    "cookie-gone": "gone",
    "browser-media": "true",
    "browser-message": "there",
    "browser-fullscreen": "fullscreen refused",
    "browser-file": "file refused",
    "svg-count": "1",
    "canvas-size": "40x20",
}


def fail(message):
    print(f"web modules smoke: {message}", file=sys.stderr)
    sys.exit(1)


# --- a WebSocket echo server, text frames only ------------------------------

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def read_exact(conn, count):
    data = b""
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise ConnectionError("closed")
        data += chunk
    return data


def read_frame(conn):
    first, second = read_exact(conn, 2)
    opcode = first & 0x0F
    length = second & 0x7F
    if length == 126:
        (length,) = struct.unpack(">H", read_exact(conn, 2))
    elif length == 127:
        (length,) = struct.unpack(">Q", read_exact(conn, 8))
    mask = read_exact(conn, 4) if second & 0x80 else b"\0\0\0\0"
    payload = bytes(b ^ mask[i % 4] for i, b in enumerate(read_exact(conn, length)))
    return opcode, payload


def write_frame(conn, opcode, payload):
    header = bytes([0x80 | opcode])
    if len(payload) < 126:
        header += bytes([len(payload)])
    elif len(payload) < 1 << 16:
        header += bytes([126]) + struct.pack(">H", len(payload))
    else:
        header += bytes([127]) + struct.pack(">Q", len(payload))
    conn.sendall(header + payload)


def echo(conn):
    with conn:
        request = b""
        while b"\r\n\r\n" not in request:
            chunk = conn.recv(4096)
            if not chunk:
                return
            request += chunk
        key = ""
        for line in request.decode("latin-1").split("\r\n"):
            if line.lower().startswith("sec-websocket-key:"):
                key = line.split(":", 1)[1].strip()
        accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
        conn.sendall(
            (
                "HTTP/1.1 101 Switching Protocols\r\n"
                "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Accept: {accept}\r\n\r\n"
            ).encode()
        )
        try:
            while True:
                opcode, payload = read_frame(conn)
                if opcode == 0x8:
                    # The close handshake: the client's code and reason back.
                    write_frame(conn, 0x8, payload)
                    return
                if opcode == 0x9:
                    write_frame(conn, 0xA, payload)
                elif opcode == 0x1:
                    write_frame(conn, 0x1, payload)
        except ConnectionError:
            return


def serve_echo():
    server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", ECHO_PORT))
    server.listen()

    def accept():
        while True:
            conn, _ = server.accept()
            threading.Thread(target=echo, args=(conn,), daemon=True).start()

    threading.Thread(target=accept, daemon=True).start()


# --- WebDriver ---------------------------------------------------------------


def request(method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{DRIVER_PORT}{path}",
        data=data,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            return json.loads(response.read() or b"{}").get("value")
    except urllib.error.HTTPError as error:
        fail(f"WebDriver {method} {path}: {error.read().decode(errors='replace')}")


def main():
    if len(sys.argv) != 2:
        fail("usage: web-addons-smoke.py URL")
    url = sys.argv[1]
    chrome = os.environ.get("CHROME_BIN", "google-chrome")
    driver_bin = os.environ.get("CHROMEDRIVER", "chromedriver")

    serve_echo()
    driver = subprocess.Popen(
        [driver_bin, f"--port={DRIVER_PORT}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(100):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{DRIVER_PORT}/status", timeout=1)
                break
            except OSError:
                time.sleep(0.1)
        else:
            fail("chromedriver did not come up")
        run(url, chrome)
    finally:
        driver.terminate()
        driver.wait()


def run(url, chrome):
    session = request(
        "POST",
        "/session",
        {
            "capabilities": {
                "alwaysMatch": {
                    "browserName": "chrome",
                    "goog:loggingPrefs": {"browser": "ALL"},
                    "goog:chromeOptions": {
                        "binary": chrome,
                        "args": ["--headless=new", "--no-sandbox", "--disable-gpu"],
                    },
                }
            }
        },
    )["sessionId"]
    base = f"/session/{session}"

    def script(source, *args):
        return request("POST", f"{base}/execute/sync", {"script": source, "args": list(args)})

    def labels():
        return script(
            "const out = {};"
            "for (const id of arguments[0]) {"
            "  const el = document.getElementById(id);"
            "  out[id] = el ? el.textContent : null;"
            "}"
            "return out;",
            list(EXPECTED),
        )

    try:
        request("POST", f"{base}/url", {"url": url})
        main_window = request("GET", f"{base}/window")

        # Wait for the app to be up: every leg it starts on its own has
        # answered when the synchronous labels have.
        deadline = time.time() + WAIT_SECONDS
        while time.time() < deadline and labels().get("js-call") != "9":
            time.sleep(0.2)

        # What only the page's surroundings can do.
        # Twice: the fixture stops listening after the first, so the second
        # would show as a different detail if it were heard.
        script("window.dispatchEvent(new CustomEvent('lumen-ping', { detail: 7 }));")
        time.sleep(0.5)
        script("window.dispatchEvent(new CustomEvent('lumen-ping', { detail: 8 }));")
        script("window.postMessage({ hello: 'there' }, '*');")
        other = request("POST", f"{base}/window/new", {"type": "tab"})["handle"]
        request("POST", f"{base}/window", {"handle": other})
        # Any page of the same origin that is not the app, so nothing on it
        # writes storage of its own.
        request("POST", f"{base}/url", {"url": url.rstrip("/") + "/lumen.web.json"})
        script("localStorage.setItem('from_other_tab', 'yes');")
        request("DELETE", f"{base}/window")
        request("POST", f"{base}/window", {"handle": main_window})

        seen = {}
        while time.time() < deadline:
            seen = labels()
            if all(seen.get(k) == v for k, v in EXPECTED.items()):
                break
            time.sleep(0.2)
        # Everything the page logged but the runtime's own boot report, which
        # is a failure on its own and the first clue to any other.
        logs = request("POST", f"{base}/se/log", {"type": "browser"}) or []
        # The browser asking for a favicon the fixture does not have is the
        # browser's request, not the page's.
        noise = [
            entry["message"]
            for entry in logs
            if "lumen: hydrated" not in entry["message"] and "favicon.ico" not in entry["message"]
        ]
        for line in noise:
            print(f"  console: {line}", file=sys.stderr)

        wrong = {k: seen.get(k) for k, v in EXPECTED.items() if seen.get(k) != v}
        for label, got in wrong.items():
            print(f"  {label}: read {got!r}, wanted {EXPECTED[label]!r}", file=sys.stderr)
        if wrong:
            fail(f"{len(wrong)} module legs never answered as they should")

        # The drawing: what the canvas holds, read back from its pixels.
        pixels = script(
            "const c = document.getElementById('paint');"
            "const ctx = c.getContext('2d');"
            "const sx = c.width / 40, sy = c.height / 20;"
            "const at = (x, y) => Array.from(ctx.getImageData(Math.floor(x * sx), Math.floor(y * sy), 1, 1).data);"
            "return { tag: c.tagName, red: at(5, 5), blue: at(30, 5), green: at(39, 19) };"
        )
        if pixels["tag"] != "CANVAS":
            fail(f"the canvas element is a {pixels['tag']}")
        for name, want in (
            ("red", [255, 0, 0, 255]),
            ("blue", [0, 0, 255, 255]),
            ("green", [0, 255, 0, 255]),
        ):
            if pixels[name] != want:
                fail(f"the canvas's {name} area reads {pixels[name]}, wanted {want}")

        # The SVG: the attribute the script set, and nothing that runs code.
        drawing = script(
            "const view = document.getElementById('art');"
            "const dot = view.querySelector('#dot');"
            "const urls = [...view.querySelectorAll('*')].some((e) =>"
            "  [...e.attributes].some((a) => /java\\s*script:/i.test(a.value)));"
            "return { fill: dot && dot.getAttribute('fill'),"
            "  onclick: dot && dot.hasAttribute('onclick'),"
            "  scripts: view.querySelectorAll('script').length, urls };"
        )
        if drawing != {"fill": "blue", "onclick": False, "scripts": 0, "urls": False}:
            fail(f"the svg-view holds {drawing}")

        if noise:
            fail("the page logged something")

    finally:
        request("DELETE", base)

    print("web modules smoke: every first-party module's web half answered")


if __name__ == "__main__":
    main()
