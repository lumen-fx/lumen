#!/usr/bin/env python3
"""Pointer and Escape for mutter through its remote-desktop D-Bus API, from
stdin: `m X Y` moves to a desktop point, `p` and `r` press and release the
left button, `k` presses Escape. The pointer is first pushed into the top-left
corner, so relative moves from there land on desktop points."""

import sys

import dbus

BUS = "org.gnome.Mutter.RemoteDesktop"
bus = dbus.SessionBus()
rd = dbus.Interface(bus.get_object(BUS, "/org/gnome/Mutter/RemoteDesktop"), BUS)
path = rd.CreateSession()
session = dbus.Interface(bus.get_object(BUS, path), BUS + ".Session")
session.Start()
session.NotifyPointerMotionRelative(-100000.0, -100000.0)
x, y = 0.0, 0.0
for line in sys.stdin:
    parts = line.split()
    if not parts:
        continue
    if parts[0] == "m":
        nx, ny = float(parts[1]), float(parts[2])
        session.NotifyPointerMotionRelative(nx - x, ny - y)
        x, y = nx, ny
    elif parts[0] in ("p", "r"):
        session.NotifyPointerButton(272, parts[0] == "p")
    elif parts[0] == "k":
        session.NotifyKeyboardKeycode(1, True)
        session.NotifyKeyboardKeycode(1, False)
