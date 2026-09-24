//! WebSocket connections a script names by key, text frames only.
//!
//! Install [`WebSocketPlugin`] and the app gains the `ws` namespace, in every
//! host:
//!
//! ```text
//! ws::open(key, url) -> bool           ws::send(key, text) -> bool
//! ws::close(key, code, reason) -> bool ws::state(key) -> string
//! ```
//!
//! and four events, each with the key first: `on_ws_open(key)`,
//! `on_ws_message(key, text)`, `on_ws_close(key, code, reason, clean)` and
//! `on_ws_error(key, message)`. A per-key `on("on_ws_message", key, fn)`
//! registration wins over the handler of that name.
//!
//! The surface is declared once, in the module's web half
//! (`web/lumen-addon.toml`), which a web build ships as the browser's
//! WebSocket. On the desktop each connection runs on a thread of its own and
//! hands every event to the plugin-event bus, so a slow server never holds up
//! a frame. `wss://` is TLS through rustls with the Mozilla root set, and an
//! `http://` or `https://` URL is taken as `ws://` or `wss://`, as a page
//! takes it.
//!
//! ```toml
//! [dependencies]
//! lumen-websocket = { bundled = true }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod connection;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lumen_module::lumen_core::app::{App, Plugin};
use lumen_module::lumen_script::{ScriptFnAppExt, ScriptFnBody, ScriptValue};

use connection::{Connection, Outgoing, State};

/// The web half's descriptor, which is where this module's functions are
/// declared.
pub const DESCRIPTOR: &str = include_str!("../web/lumen-addon.toml");

/// The module's name, as an app declares it.
pub const NAME: &str = "lumen-websocket";

/// WebSocket connections for a Lumen app: install it and the `ws` functions
/// exist.
///
/// Ships as the bundled `lumen-websocket` runtime module, and works the same
/// added as an ordinary plugin in a static build.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebSocketPlugin;

impl Plugin for WebSocketPlugin {
    fn build(self, app: &mut App) {
        match script_fns(&Sockets::default()) {
            Ok(fns) => {
                app.add_script_fns(fns);
            }
            Err(reason) => eprintln!("lumen-runtime: {reason}"),
        }
    }
}

/// Every connection the app holds, by key.
#[derive(Clone, Default)]
pub(crate) struct Sockets {
    live: Arc<Mutex<HashMap<String, Connection>>>,
    next: Arc<AtomicU64>,
}

impl Sockets {
    /// The connection under `key`, when there is one.
    fn with<T>(&self, key: &str, f: impl FnOnce(Option<&Connection>) -> T) -> T {
        let live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        f(live.get(key))
    }

    /// Whether `id` is still the connection under `key`: one replaced under
    /// the same key says nothing more.
    pub(crate) fn is_current(&self, key: &str, id: u64) -> bool {
        self.with(key, |c| c.is_some_and(|c| c.id == id))
    }

    /// Forget the connection `id` under `key`, unless another replaced it.
    pub(crate) fn forget(&self, key: &str, id: u64) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if live.get(key).is_some_and(|c| c.id == id) {
            live.remove(key);
        }
    }

    fn open(&self, key: String, url: &str) -> Result<bool, String> {
        let url = connection::socket_url(url)?;
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if live
            .get(&key)
            .is_some_and(|c| matches!(c.state(), State::Connecting | State::Open))
        {
            return Ok(false);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let connection = connection::start(self.clone(), key.clone(), id, url)?;
        live.insert(key, connection);
        Ok(true)
    }

    fn send(&self, key: &str, text: String) -> bool {
        self.with(key, |c| {
            c.is_some_and(|c| c.state() == State::Open && c.push(Outgoing::Text(text)))
        })
    }

    fn close(&self, key: &str, code: i64, reason: String) -> Result<bool, String> {
        let code = u16::try_from(code)
            .ok()
            .filter(|code| *code == 1000 || (3000..=4999).contains(code))
            .ok_or_else(|| format!("ws::close: code {code} is neither 1000 nor in 3000 to 4999"))?;
        if reason.len() > 123 {
            return Err("ws::close: the reason is longer than 123 bytes".to_string());
        }
        Ok(self.with(key, |c| {
            c.is_some_and(|c| {
                matches!(c.state(), State::Connecting | State::Open) && {
                    c.set_state(State::Closing);
                    c.push(Outgoing::Close(code, reason))
                }
            })
        }))
    }

    fn state(&self, key: &str) -> &'static str {
        self.with(key, |c| c.map_or(State::Closed, Connection::state).name())
    }
}

/// The four functions, bound to `sockets`.
fn script_fns(sockets: &Sockets) -> Result<Vec<lumen_module::lumen_script::ScriptFn>, String> {
    let open: ScriptFnBody = {
        let sockets = sockets.clone();
        Arc::new(move |cx| {
            sockets
                .open(cx.str_arg(0), &cx.str_arg(1))
                .map(ScriptValue::Bool)
        })
    };
    let send: ScriptFnBody = {
        let sockets = sockets.clone();
        Arc::new(move |cx| {
            Ok(ScriptValue::Bool(
                sockets.send(&cx.str_arg(0), cx.str_arg(1)),
            ))
        })
    };
    let close: ScriptFnBody = {
        let sockets = sockets.clone();
        Arc::new(move |cx| {
            sockets
                .close(&cx.str_arg(0), cx.int_arg(1), cx.str_arg(2))
                .map(ScriptValue::Bool)
        })
    };
    let state: ScriptFnBody = {
        let sockets = sockets.clone();
        Arc::new(move |cx| Ok(ScriptValue::Str(sockets.state(&cx.str_arg(0)).to_string())))
    };
    lumen_module::web_half_fns(
        NAME,
        DESCRIPTOR,
        vec![
            ("open".into(), open),
            ("send".into(), send),
            ("close".into(), close),
            ("state".into(), state),
        ],
    )
}

lumen_module::lumen_module!("lumen-websocket", |_config: lumen_module::ModuleConfig| {
    WebSocketPlugin
});

#[cfg(test)]
mod tests {
    use super::*;

    /// One surface on every target: every function the web half declares
    /// has a desktop body.
    #[test]
    fn the_desktop_half_answers_every_function_the_web_half_declares() {
        let fns = script_fns(&Sockets::default()).expect("every function has a body");
        let declared =
            lumen_module::describe_web_half(NAME, DESCRIPTOR).expect("the descriptor reads");
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        let expected: Vec<&str> = declared.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn a_key_with_no_connection_is_closed_and_takes_nothing() {
        let sockets = Sockets::default();
        assert_eq!(sockets.state("none"), "closed");
        assert!(!sockets.send("none", "hi".to_string()));
        assert_eq!(sockets.close("none", 1000, String::new()), Ok(false));
    }

    #[test]
    fn a_close_code_the_protocol_reserves_is_refused() {
        let sockets = Sockets::default();
        for code in [0, 1001, 1006, 2999, 5000] {
            assert!(sockets.close("k", code, String::new()).is_err(), "{code}");
        }
        assert!(sockets.close("k", 1000, "x".repeat(124)).is_err());
    }
}
