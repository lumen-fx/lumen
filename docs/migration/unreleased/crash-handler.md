# Apps handle their own crashes and start a lumen-crash helper

This affects every windowed Lumen app: markup apps run by `lumenc`, Rust apps
on `lumenui`, and C++ and Python programs that call `lumen_app_run`.

A windowed launch now installs crash handling before the window opens. It
sets a panic hook in front of the one already set, installs handlers for
native faults (signal handlers for `SIGSEGV`, `SIGBUS`, `SIGABRT`, `SIGILL`,
`SIGFPE` and `SIGTRAP` on Linux; the task's exception ports on macOS; the
unhandled-exception filter on Windows), and starts the `lumen-crash` helper
process beside the app when it finds one. A crash leaves a report in `crashes/` under the app's data
directory and shows a message box. Headless runs are unchanged.

What you may notice:

- One more process, `lumen-crash`, runs for as long as the app does.
- A program that installs its own fault handlers before `lumen_app_run`
  has them replaced for the faults listed above. A panic hook set before is
  kept and still runs.

To keep the old behaviour, turn crash handling off in `lumen.toml`:

```toml
[crash]
enabled = false
```

A Rust app that wants its own panic hook sets that and installs the hook
itself.
