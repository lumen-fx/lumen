//! Serves a Lumen site whose pages are rendered for the request that asks.
//!
//! `lumenc web --render ssr` writes a site directory: `lumen.site.json`, the
//! compiled app, and the files a page loads. This crate serves that
//! directory, and serves any other site `lumenc web` builds as its files.
//! Files come straight from disk, every page is a render of the app through
//! [`lumen_ssr`], and the process holds the limits a server facing the
//! public needs: a bounded number of connections, a bounded render queue
//! that turns a request away with a 503 when it is full, deadlines on every
//! read and write, health endpoints, an access log, a graceful stop, and
//! worker processes that restart themselves.
//!
//! The crate is the `lumen-server` binary; its library is the command's
//! entry point and nothing more. `lumenc web --serve` starts the same binary
//! with `--dev`, which turns on development defaults.
//!
//! A process renders one page at a time, because the buses an app reads its
//! state through belong to the process. On unix, `--workers N` runs N worker
//! processes sharing one listening socket; everywhere, more processes behind
//! a balancer is how a site answers more at once.

pub mod cli;
mod config;
mod http;
mod log;
mod parent;
mod proxy;
mod render;
mod server;
#[cfg(unix)]
mod supervisor;
mod time;
