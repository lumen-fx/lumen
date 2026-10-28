#!/usr/bin/env python3
"""Drag dock tabs across real windows with the desktop's own input.

Starts the dock-drag fixture (fixtures/dock-drag) with the lumenc binary
given, moves the pointer and presses keys through the desktop's own input
injection, and checks the arrangement the app prints after each step:

  reorder   a tab dragged to the end of its strip
  split     a tab dropped on the right edge of its stack
  escape    a drag Escape cancels, which changes nothing
  tear-off  a tab dragged past the window's edge, into a window of its own
  dock-back that window's tab dragged over the main window, which docks it

Each step leaves screenshots of the app's windows in the output directory,
taken by the app itself through `lumenc screenshot`.

usage: drive.py LUMENC OUT_DIR --desktop x11|windows|macos|wayland

`wayland` drives a wlroots compositor (sway) through wlr-virtual-pointer, a
KWin through its fake-input protocol, or a mutter through its remote-desktop
D-Bus API; the helper programs for the first two are given with --pointer.

Where a window is on the desktop is found by asking the app: the fixture
prints every pointer move it sees in its window's own coordinates, so a
pointer put at a known desktop point says where the window is. That works the
same on a Wayland desktop, where no client may learn its window's place.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

MCP_PORT = 47941
STEP = 0.03


# ---- input ------------------------------------------------------------------


class X11:
    def __init__(self):
        from Xlib import X, XK, display
        from Xlib.ext import xtest

        self.X, self.xtest = X, xtest
        self.d = display.Display()
        self.esc = self.d.keysym_to_keycode(XK.string_to_keysym("Escape"))

    def move(self, x, y):
        self.xtest.fake_input(self.d, self.X.MotionNotify, x=int(x), y=int(y))
        self.d.sync()

    def button(self, down):
        self.xtest.fake_input(self.d, self.X.ButtonPress if down else self.X.ButtonRelease, 1)
        self.d.sync()

    def escape(self):
        # With no window manager nothing gives a window the keyboard: give it
        # to the window under the pointer, as a click-to-focus desktop does.
        under = self.d.screen().root.query_pointer().child
        if under:
            self.d.set_input_focus(under, self.X.RevertToParent, self.X.CurrentTime)
            self.d.sync()
            time.sleep(0.1)
        for kind in (self.X.KeyPress, self.X.KeyRelease):
            self.xtest.fake_input(self.d, kind, self.esc)
            self.d.sync()

    def screen(self):
        s = self.d.screen()
        return s.width_in_pixels, s.height_in_pixels

    def windows(self, pid):
        root = self.d.screen().root
        out = {}

        def walk(w, depth):
            for k in w.query_tree().children:
                try:
                    name = k.get_wm_name()
                    attrs = k.get_attributes()
                except Exception:
                    continue
                if name and attrs.map_state == self.X.IsViewable:
                    g = k.get_geometry()
                    at = k.translate_coords(root, 0, 0)
                    out[str(name)] = (-at.x, -at.y, g.width, g.height)
                if depth < 2:
                    walk(k, depth + 1)

        walk(root, 0)
        return out


class Windows:
    def __init__(self):
        import ctypes
        from ctypes import wintypes

        self.c, self.w = ctypes, wintypes
        self.u = ctypes.windll.user32
        self.u.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))

    def move(self, x, y):
        self.u.SetCursorPos(int(x), int(y))

    def button(self, down):
        self.u.mouse_event(0x0002 if down else 0x0004, 0, 0, 0, 0)

    def escape(self):
        self.u.keybd_event(0x1B, 0, 0, 0)
        self.u.keybd_event(0x1B, 0, 2, 0)

    def screen(self):
        return self.u.GetSystemMetrics(0), self.u.GetSystemMetrics(1)

    def windows(self, pid):
        c, w, u = self.c, self.w, self.u
        out = {}

        @c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
        def each(hwnd, _):
            owner = w.DWORD()
            u.GetWindowThreadProcessId(hwnd, c.byref(owner))
            if owner.value != pid or not u.IsWindowVisible(hwnd):
                return True
            buf = c.create_unicode_buffer(256)
            u.GetWindowTextW(hwnd, buf, 256)
            p = w.POINT(0, 0)
            u.ClientToScreen(hwnd, c.byref(p))
            r = w.RECT()
            u.GetClientRect(hwnd, c.byref(r))
            if buf.value:
                out[buf.value] = (p.x, p.y, r.right, r.bottom)
            return True

        u.EnumWindows(each, 0)
        return out


class Macos:
    def __init__(self):
        import Quartz

        self.q = Quartz
        self.down = False
        self.at = (0, 0)

    def post(self, kind, x, y):
        q = self.q
        e = q.CGEventCreateMouseEvent(None, kind, (x, y), q.kCGMouseButtonLeft)
        q.CGEventPost(q.kCGHIDEventTap, e)

    def move(self, x, y):
        q = self.q
        self.at = (x, y)
        self.post(q.kCGEventLeftMouseDragged if self.down else q.kCGEventMouseMoved, x, y)

    def button(self, down):
        q = self.q
        self.down = down
        self.post(q.kCGEventLeftMouseDown if down else q.kCGEventLeftMouseUp, *self.at)

    def escape(self):
        q = self.q
        for down in (True, False):
            q.CGEventPost(q.kCGHIDEventTap, q.CGEventCreateKeyboardEvent(None, 53, down))

    def screen(self):
        b = self.q.CGDisplayBounds(self.q.CGMainDisplayID())
        return int(b.size.width), int(b.size.height)

    def windows(self, pid):
        q = self.q
        info = q.CGWindowListCopyWindowInfo(
            q.kCGWindowListOptionOnScreenOnly | q.kCGWindowListExcludeDesktopElements,
            q.kCGNullWindowID,
        )
        out = {}
        for i, w in enumerate(info or []):
            if w.get("kCGWindowOwnerPID") != pid:
                continue
            b = w["kCGWindowBounds"]
            name = w.get("kCGWindowName") or f"window-{w.get('kCGWindowNumber', i)}"
            out[str(name)] = (b["X"], b["Y"], b["Width"], b["Height"])
        return out


class Wayland:
    """A helper process that reads `m X Y`, `p`, `r`, `k` lines."""

    def __init__(self, helper, compositor, size):
        self.helper = subprocess.Popen(helper, stdin=subprocess.PIPE, text=True)
        self.compositor = compositor
        self.size = size

    def send(self, line):
        self.helper.stdin.write(line + "\n")
        self.helper.stdin.flush()

    def move(self, x, y):
        self.send(f"m {int(x)} {int(y)}")

    def button(self, down):
        self.send("p" if down else "r")

    def escape(self):
        self.send("k")

    def screen(self):
        return self.size

    def windows(self, pid):
        if self.compositor != "sway":
            return {}
        tree = json.loads(subprocess.check_output(["swaymsg", "-t", "get_tree", "-r"]))
        out = {}

        def walk(n):
            if n.get("pid") and n.get("name"):
                r, wr = n["rect"], n.get("window_rect", {"x": 0, "y": 0})
                out[n["name"]] = (r["x"] + wr["x"], r["y"] + wr["y"], r["width"], r["height"])
            for k in n.get("nodes", []) + n.get("floating_nodes", []):
                walk(k)

        walk(tree)
        return out


# ---- the app ----------------------------------------------------------------


class App:
    def __init__(self, lumenc, out, env):
        self.lumenc, self.out = lumenc, out
        self.dir = Path(tempfile.mkdtemp(prefix="dock-drag-"))
        fixture = Path(__file__).resolve().parents[3] / "fixtures" / "dock-drag"
        shutil.copytree(fixture, self.dir / "app")
        toml = (self.dir / "app" / "lumen.toml").read_text()
        (self.dir / "app" / "lumen.toml").write_text(toml.replace("port = 0", f"port = {MCP_PORT}"))
        self.log = out / "app.log"
        env = dict(os.environ, **env)
        env.setdefault("XDG_DATA_HOME", str(self.dir / "data"))
        self.proc = subprocess.Popen(
            [lumenc, "run", str(self.dir / "app")],
            stdout=open(self.log, "w"),
            stderr=subprocess.STDOUT,
            env=env,
        )

    def lines(self):
        try:
            return self.log.read_text(errors="replace").splitlines()
        except FileNotFoundError:
            return []

    def wait_line(self, text, secs=20, since=0):
        end = time.time() + secs
        while time.time() < end:
            if any(text in l for l in self.lines()[since:]):
                return True
            time.sleep(0.1)
        return False

    def layout(self):
        last = [l for l in self.lines() if l.startswith("dock-drag layout ")]
        return json.loads(last[-1][len("dock-drag layout "):]) if last else None

    def pointer_line(self, since):
        for l in reversed(self.lines()[since:]):
            if l.startswith("dock-drag pointer "):
                _, _, x, y = l.split()
                return float(x), float(y)
        return None

    def shot(self, name, window="primary"):
        path = self.out / f"{name}.png"
        subprocess.run(
            [self.lumenc, "screenshot", str(path), "--port", str(MCP_PORT), "--window", window],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=30,
        )

    def stop(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()


def desktop_shot(path):
    """The whole screen, where Pillow can grab it (Windows, macOS): what
    the app's own screenshots cannot show, its windows' places."""
    try:
        from PIL import ImageGrab

        ImageGrab.grab().save(path)
    except Exception as e:
        print(f"no desktop picture: {e}", flush=True)


def png_size(path):
    try:
        head = path.read_bytes()[16:24]
    except FileNotFoundError:
        return None
    return int.from_bytes(head[:4], "big"), int.from_bytes(head[4:], "big")


def stacks(layout):
    """Every stack of the layout as (container, panels): `dock` or a window id."""
    out = []

    def walk(node, where):
        if "stack" in node:
            out.append((where, node.get("panels", [])))
        for c in node.get("children", []):
            walk(c["node"], where)

    for dock in layout.get("docks", {}).values():
        walk(dock["root"], "dock")
        for f in dock.get("floats", []):
            walk(f["root"], "float")
    for w in layout.get("windows", []):
        walk(w["root"], w["id"])
    return out


def stack_of(layout, panel):
    return next((s for s in stacks(layout) if panel in s[1]), None)


# ---- the steps --------------------------------------------------------------


class Run:
    def __init__(self, inp, app):
        self.inp, self.app = inp, app
        self.failures = []

    def settle(self, done, secs=10):
        """Wait until `done(layout)` holds or `secs` pass, and return the
        layout last read: a debug build paints a frame in a good part of a
        second, and a drop reaches the printed arrangement some frames
        after the release."""
        end = time.time() + secs
        while True:
            lay = self.app.layout()
            if (lay and done(lay)) or time.time() > end:
                return lay
            time.sleep(0.1)

    def check(self, step, ok, detail):
        print(f"{'ok  ' if ok else 'FAIL'} {step}: {detail}", flush=True)
        if not ok:
            self.failures.append(f"{step}: {detail}")

    def locate(self, guesses):
        """Where a window's client area starts on the desktop: put the pointer
        at each guess until the app says where it is in the window."""
        for gx, gy in guesses:
            # Off the spot first, so the move to it is a move.
            self.inp.move(gx - 3, gy - 3)
            time.sleep(0.2)
            since = len(self.app.lines())
            self.inp.move(gx, gy)
            time.sleep(0.6)
            p = self.app.pointer_line(since)
            if p:
                return (gx - p[0], gy - p[1])
        return None

    def drag(self, start, end, before_release=None, steps=30):
        self.inp.move(*start)
        time.sleep(0.25)
        self.inp.button(True)
        time.sleep(0.2)
        for k in range(1, steps + 1):
            t = k / steps
            self.inp.move(start[0] + (end[0] - start[0]) * t, start[1] + (end[1] - start[1]) * t)
            time.sleep(STEP)
        time.sleep(0.4)
        if before_release:
            before_release()
        self.inp.button(False)
        time.sleep(0.8)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("lumenc")
    ap.add_argument("out")
    ap.add_argument("--desktop", required=True, choices=["x11", "windows", "macos", "wayland"])
    ap.add_argument("--pointer", help="the Wayland input helper, and its arguments", nargs="*")
    ap.add_argument("--compositor", default="sway")
    ap.add_argument("--screen", default="1280x1024")
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)

    if a.desktop == "x11":
        inp = X11()
    elif a.desktop == "windows":
        inp = Windows()
    elif a.desktop == "macos":
        inp = Macos()
    else:
        inp = Wayland(a.pointer, a.compositor, tuple(int(v) for v in a.screen.split("x")))
    sw, sh = inp.screen()
    print(f"desktop {a.desktop} {a.compositor if a.desktop == 'wayland' else ''} screen {sw}x{sh}", flush=True)

    app = App(a.lumenc, out, {})
    run = Run(inp, app)
    try:
        if not app.wait_line("dock-drag ready", 90):
            run.check("start", False, "the app did not come up:\n" + "\n".join(app.lines()[-20:]))
            return report(run)
        time.sleep(2.0)
        grid = [(sw * x / 8, sh * y / 8) for y in range(1, 8) for x in range(1, 8)]
        origin = run.locate([(sw / 2, sh / 2)] + grid)
        run.check("locate", origin is not None, f"the main window's client area starts at {origin}")
        if origin is None:
            return report(run)
        ox, oy = origin
        app.shot("0-start")
        # The steps place the pointer in an 800x500 window with room around
        # it; a window still maximized would put every drop elsewhere.
        size = png_size(out / "0-start.png")
        run.check("size", size == (800, 500), f"the main window is {size}")
        if size != (800, 500):
            return report(run)
        at = lambda x, y: (ox + x, oy + y)
        first = app.layout()
        center = stack_of(first, "scene")
        run.check("start", center and center[1] == ["scene", "game"], f"center stack {center}")

        # Reorder: the center's first tab to the end of its strip.
        run.drag(at(280, 15), at(770, 15), lambda: app.shot("1-reorder-preview"))
        lay = run.settle(lambda l: (stack_of(l, "scene") or ("", []))[1] == ["game", "scene"])
        app.shot("1-reorder")
        s = stack_of(lay, "scene")
        run.check("reorder", s and s[1] == ["game", "scene"], f"center stack {s}")

        # Split: the first tab dropped on the stack's right edge.
        run.drag(at(280, 15), at(700, 260), lambda: app.shot("2-split-preview"))
        lay = run.settle(lambda l: (stack_of(l, "game") or ("", []))[1] == ["game"])
        app.shot("2-split")
        g, sc = stack_of(lay, "game"), stack_of(lay, "scene")
        run.check("split", g and sc and g[1] == ["game"] and sc[1] == ["scene"], f"game {g}, scene {sc}")

        # Escape: a drag to the middle, cancelled.
        before = app.layout()
        since = len(app.lines())

        def cancel():
            app.shot("3-escape-preview")
            inp.escape()
            # The release must come after the app took the key.
            time.sleep(1.5)

        run.drag(at(36, 15), at(400, 260), cancel)
        # Nothing should change: give a wrong drop the time a right one
        # would take to show.
        time.sleep(3.0)
        app.shot("3-escape")
        moved = [l for l in app.lines()[since:] if l.startswith("dock-drag files ")]
        same = stacks(app.layout()) == stacks(before)
        run.check("escape", same and not moved, f"layout kept: {same}, moves: {moved}")

        # Tear-off: Files past the window's edge.
        since = len(app.lines())
        below = oy + 500 + 120 < sh - 4
        end = at(60, 620) if below else (min(ox + 920, sw - 4), oy + 60)
        run.drag(at(36, 15), end)
        torn = app.wait_line("dock-drag files window", 15, since)
        app.shot("4-tear-off")
        app.shot("4-tear-off-window", "files")
        run.check("tear-off", torn, "files is in a window of its own")
        if not torn:
            return report(run)

        # Dock back: the pop-out's tab over the main window.
        time.sleep(1.0)
        wins = inp.windows(app.proc.pid)
        desktop_shot(out / "4-tear-off-desktop.png")
        files = wins.get("Files")
        guesses = [end]
        if files:
            # Down its left side from the top, where its tab is, as far as
            # the screen shows it: a window opened near the screen's edge
            # runs past it.
            x, y, _, h = files
            guesses = [(x + 40, y + dy) for dy in range(8, int(min(h, sh - 2 - y)), 12)] + guesses
        pop = run.locate(guesses + grid)
        run.check("locate pop-out", pop is not None, f"the pop-out starts at {pop}, desktop says {files}")
        if pop is None:
            return report(run)
        since = len(app.lines())
        run.drag(
            (pop[0] + 36, pop[1] + 15),
            at(560, 300),
            lambda: (
                time.sleep(0.4),
                app.shot("5-dock-back-preview"),
                desktop_shot(out / "5-dock-back-desktop.png"),
            ),
        )
        back = app.wait_line("dock-drag files dock", 15, since)
        lay = run.settle(lambda l: (stack_of(l, "files") or ("", []))[0] == "dock")
        app.shot("5-dock-back")
        where = stack_of(lay, "files")
        run.check("dock-back", back and where and where[0] == "dock", f"files is in {where}")
    finally:
        app.stop()
    return report(run)


def report(run):
    if run.failures:
        print("\n".join(["failed:"] + run.failures), flush=True)
        sys.exit(1)
    print("every step passed", flush=True)


if __name__ == "__main__":
    main()
