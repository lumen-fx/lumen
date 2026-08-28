//! Running another program from a Lumen app, as a self-contained module.
//!
//! The engine has no process code; this crate is the whole capability.
//! Install [`ProcessPlugin`] and the app gains one function, in every host:
//! `process::start(cmd, args, tag)` in Rhai and candela, `process.start(..)`
//! in Lua. It starts `cmd` in the app directory and answers whether the
//! program is running.
//!
//! Without the module none of that exists: a script calling `process::start`
//! gets its host's ordinary unknown-function error.
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
//! # What this version does not do
//!
//! There is no way to write to a child's stdin, no way to end a child from a
//! script, and no per-child environment or working directory. Children are
//! not ended when the app exits: a program still running outlives the app
//! that started it.

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
