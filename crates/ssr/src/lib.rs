//! Renders a Lumen app to an HTML document, once per request.
//!
//! A build writes a page from the state an app settles into on the machine
//! that built it. A server writes it from the state the app settles into for
//! the visitor asking: the address they asked for, the language their browser
//! wants, the record their path names, the data an API answered a moment ago.
//! Same app, same markup, same emitter; what differs is when the state is
//! read and who it is read for.
//!
//! There is no transport here. No sockets, no HTTP parsing, no async runtime:
//! a request goes in as a struct and a response comes back as one, so this
//! goes inside whatever server you already have.
//!
//! ```no_run
//! use std::sync::Arc;
//! use lumen_ssr::{RenderOptions, Renderer, SsrRequest, SsrSite};
//! use lumen_web::WebSpec;
//!
//! let compiled = lumen_ir::artifact::read("dist/web/app.lmna".as_ref())?;
//! let site = SsrSite::new(compiled, WebSpec::default())?;
//! let renderer = Renderer::start(Arc::new(site), RenderOptions::default())?;
//!
//! let response = renderer.render(SsrRequest::get("/user/42"))?;
//! println!("{} {}", response.status, response.body.len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # One render at a time, per process
//!
//! [`Renderer::start`] fails with [`SsrError::AlreadyRunning`] if the process
//! already has one. An app reads what its scripts write through buses that
//! belong to the process rather than to the app, so two apps ticking at once
//! take each other's writes and one visitor's data lands in another
//! visitor's page. Serve more requests at once by running more processes
//! behind whatever balances them.
//!
//! # An address no page answers for
//!
//! A path that matches no page key, and is no document a build wrote, is
//! answered with the app shell and a 404: the same document and the same
//! status a static build of the app gives that address. The shell holds no
//! state, so it is written once and the app is not built for it. Ask
//! [`SsrSite::page_for`] first to give such an address an answer of your own.
//!
//! A deep path is not this case. `/user/42` in an app with a `user` page is
//! that page, with the rest of the path on `route.segment`.
//!
//! # A request is a whole life
//!
//! Every request builds an app, ticks it, reads it and drops it. Nothing
//! survives to the next one: `on_start` and `on_ready` run every time, a
//! signal written for one visitor is gone before the next arrives, and
//! anything that has to outlive a request lives in your database. The boot
//! is the cost of that, and it is small enough to pay per request.
//!
//! # Waiting for the app's own requests
//!
//! An app that fetches its data in `on_ready` has none of it on the tick the
//! request goes out. So a render waits: what the app has asked for is
//! counted, and the document is written when the count reaches zero and the
//! app's state has stopped changing. An app that is still waiting when its
//! budget runs out is answered with what it has, plus a header saying so, on
//! the reasoning that a working page the browser finishes beats an error
//! page. Timers are not waited for; a page does not wait on a clock.
//!
//! What a render may ask for is named up front. An app fetching an address is
//! a visitor's request making the server make a request, so a render reaches
//! only the hosts [`FetchPolicy`] lists and makes at most as many requests as
//! it allows.
//!
//! # What a document does not carry
//!
//! Nodes a script builds by hand arrive when the browser runs that script;
//! the render says so in [`SsrResponse::warnings`] rather than leaving you to
//! find out. The rest of the limits are the emitter's and are the same ones a
//! build has.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod fetch;
pub mod renderer;
pub mod request;
pub mod response;
pub mod site;

pub use error::SsrError;
pub use fetch::FetchPolicy;
pub use renderer::{RenderOptions, Renderer};
pub use request::{HeaderPolicy, SsrRequest};
pub use response::{ResponseState, SsrResponse};
pub use site::SsrSite;

/// How long one render gets, in ticks and in time.
pub use lumen_prerender::Budget;
