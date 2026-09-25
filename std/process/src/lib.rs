//! Running another program from a Lumen app, as a self-contained module.
//!
//! The engine has no process code; this crate is the whole capability.
//! Install [`ProcessPlugin`] and the app gains two functions, in every host:
//!
//! - `process::start(cmd, args, tag, opts)` (`process.start(..)` in Lua)
//!   starts `cmd` and answers whether the program is running;
//! - `process::stop(tag)` ends the program running under `tag` and answers
//!   whether there was one.
//!
//! Without the module none of that exists: a script calling `process::start`
//! gets its host's ordinary unknown-function error.
//!
//! # Options
//!
//! `opts` is required and every field has a default:
//!
//! | Field | Type | Default | Meaning |
//! | --- | --- | --- | --- |
//! | `cwd` | string | `""` | The child's directory, relative to the app directory; empty is the app directory. |
//! | `env` | map of strings | empty | Variables laid over the inherited environment. |
//! | `end_at_exit` | bool | `false` | End the child when the app exits. |
//!
//! candela takes them as the `process::StartOptions` struct the module
//! declares, built with `..Default::default()` or passed as
//! `Default::default()`; Rhai and Lua pass a map and leave out what they do
//! not set.
//!
//! The function is `start` rather than `spawn` because Rhai reserves `spawn`
//! as a keyword: a script naming it fails to lex, so no host could see it.
//!
//! One implementation, two link shapes:
//!
//! - **Runtime module.** The `cdylib` target is the bundled `lumen-process`
//!   module; an app opts in from `lumen.toml`:
//!
//!   ```toml
//!   [dependencies]
//!   lumen-process = { bundled = true }
//!   ```
//!
//! - **Compiled in.** A statically linked app (or a test) adds this crate as
//!   an ordinary dependency and installs [`ProcessPlugin`] itself.
//!
//! # What a child reports
//!
//! Everything after the start arrives as an event keyed by the tag the script
//! chose, so one handler serves several children:
//!
//! | Event | Fallback handler |
//! | --- | --- |
//! | `process_stdout` | `on_process_stdout(tag, line)` |
//! | `process_stderr` | `on_process_stderr(tag, line)` |
//! | `process_exit` | `on_process_exit(tag, code)` |
//!
//! `process_exit` is always the last event for a tag, and its code is the
//! program's own, or 128 plus the signal that killed it. A start that failed
//! answers false and reports on stderr; it produces no event at all, because
//! the tag never named a running program.
//!
//! Output arrives a line at a time, one handler call each, so a chatty child
//! calls the handler a lot. Bytes that are not utf-8 are replaced, and a line
//! past [`child::LINE_CAP`] arrives in pieces.
//!
//! # Threads, not tasks
//!
//! A child is supervised for as long as it chooses to live, which is not the
//! bounded blocking work the app's spawn service exists for; a pool sized for
//! reads would be held by the first program that waits for input. So each
//! child gets a supervisor thread of its own, owning one reader thread per
//! output pipe. The supervisor joins both readers before it waits on the
//! child, so a child's output is complete before its exit is reported, and
//! the wait is what keeps a finished child from lingering as a zombie.
//!
//! # Ending a child
//!
//! `process::stop` sends `SIGTERM` on Unix and kills the child if it is still
//! running after [`child::GRACE`]; on Windows it ends the child at once. The
//! exit event still arrives, last as always. A tag several children share
//! stops all of them. A child started with `end_at_exit` is ended the same
//! way when the app's world is dropped, and the drop waits for it; an app
//! killed outright never drops its world, so its children keep running.
//!
//! There is no way to write to a child's stdin.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod child;

mod plugin;

pub use plugin::ProcessPlugin;

// The module entry: the loader constructs the shipping plugin from the app's
// `config` table, whether it opened this crate's library or found it linked
// in.
lumen_module::lumen_module!("lumen-process", |config: lumen_module::ModuleConfig| {
    ProcessPlugin::new(config)
});
