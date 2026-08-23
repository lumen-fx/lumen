# Philosophy

What Lumen values, in the order it trades them off. When a change forces a
choice, this file says which side wins.

## Fast

The app is fast by default. Startup, first frame, input latency, and
animation are engine responsibilities; an author writing ordinary markup and
CSS gets a responsive app without tuning anything. A feature that makes the
default path slower pays for itself somewhere else or does not land.

## Minimal

Ship only what an app uses. No bundled runtimes, no always-on services, no
dependencies carried for one corner case. The runtime is deconstructable:
capabilities are modules, and an app that does not use one does not carry it.
When in doubt, leave it out; adding later is cheap, removing later is not.

## Parallel

The engine is built on an ECS (bevy_ecs) so work parallelizes by structure,
not by heroics. Layout, styling, rendering, and app logic are systems over
data, and independent work runs on all cores. New engine work follows the
same shape: data in components, logic in systems, no hidden globals that
serialize the frame.

## Simple, without a ceiling

Easy for newbies to pick up, infinitely flexible for power users. The first
app is markup, CSS, and a short script; no build configuration, no framework
concepts to learn before the first window opens. Power grows by adding, not
by relearning: plugins, custom backends, the C ABI, and the SDKs are there
when needed and invisible until then. A feature that makes the simple case
harder to reach is designed wrong.

## Capable

Simple does not mean small. Real apps need menus, trays, dialogs, drag and
drop, accessibility, i18n, animations, audio, multiple pages, and packaging;
Lumen covers them in the box. The measure of the framework is whether a
production desktop app can ship on it without escaping to another toolkit.

## Modular

One capability, one seam. Capabilities are trait crates, backends implement
them, and users can swap their own in. Nothing is hardwired: not the
renderer, not the script host, not the visual metrics. This is what keeps
minimal and capable compatible; you take the modules you need.

## Productive

The edit loop is the product. Compiles are fast, `lumenc run` hot-reloads
markup, CSS, and scripts while the window stays open, and errors point at
the line in the file the author wrote. Iteration speed is a feature with the
same priority as runtime speed; a change that slows the edit-to-result loop
is a regression.
