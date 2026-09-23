//! Serves a Lumen site whose pages are rendered for the request that asks.
//!
//! `lumenc web --render ssr` writes a site directory: `lumen.site.json`, the
//! compiled app, and the files a page loads. This crate serves that
//! directory, and serves any other site `lumenc web` builds as its files. Files come straight from disk, every page is a render of the
//! app through [`lumen_ssr`], and the process holds the limits a server
//! facing the public needs: a bounded number of connections, a bounded
//! render queue that turns a request away with a 503 when it is full,
//! deadlines on every read and write, health endpoints, an access log, a
//! graceful stop, and worker processes that restart themselves.
//!
//! The `lumen-server` binary is the production server. `lumenc web --serve`
//! starts the same binary with `--dev`, which turns on development defaults.
//!
//! A process renders one page at a time, because the buses an app reads its
//! state through belong to the process. On unix, `--workers N` runs N worker
//! processes sharing one listening socket; everywhere, more processes behind
//! a balancer is how a site answers more at once.

pub mod cli;
pub mod config;
pub mod http;
pub mod log;
mod parent;
pub mod proxy;
pub mod render;
pub mod server;
#[cfg(unix)]
mod supervisor;
mod time;

pub use http::{Request, Response};
pub use log::{Log, LogFormat};
pub use proxy::{Cidr, Trust};
pub use render::{ErrorPages, RenderHandler, RenderSettings};
pub use server::{Exit, HEALTH_PATH, LOOPBACK, Limits, RequestHandler, Server, Shutdown, Status};
