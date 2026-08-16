//! Lumen's browser runtime.
//!
//! One prebuilt wasm module serves every Lumen app on the web: a page loads it,
//! hands it the app's compiled data, and the module runs the same ECS tick a
//! desktop app runs. Nothing about an app is compiled into it, so `lumenc web`
//! emits data and copies this module beside it.
//!
//! What the browser owns and this module therefore does not: layout and paint.
//! No layout backend is installed and the extract list is emptied, because the
//! page's own CSS engine is the layout engine and the DOM is the scene.
//!
//! Scripts run on the host for the engine the app's manifest names. Each host
//! is a feature (`host-candela`), and candela is the one on by default: it runs
//! as precompiled bytecode, so no compiler reaches the page. An engine no
//! compiled-in host answers for is reported when the app boots.
//!
//! A page starts an app with [`boot`], which is what every document
//! `lumenc web` emits calls and the only thing it needs to know:
//!
//! ```js
//! import init, { boot } from "/lumen-web.js";
//! init().then(boot);
//! ```
//!
//! [`boot`] reads the document, fetches the manifest, the compiled app and
//! its scripts, adopts the prerendered markup, and starts the frame loop.
//! [`LumenWebApp`] is the surface underneath it, for a page that assembles
//! those steps itself.

#![warn(missing_docs)]

mod assemble;
mod boot;
mod hosts;
mod load;

use std::cell::RefCell;
use std::rc::Rc;

use lumen_core::prelude::App;
use lumen_script::{ScriptLoadFailure, ScriptValue};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::hosts::ScriptHostAccess;

pub use boot::boot;

/// Name the load failure reports when the page supplies no better one. A page
/// has one: the manifest records the path every script was emitted to.
const DEFAULT_SCRIPT_URI: &str = "<script>";

/// The self-re-arming animation-frame callback. Aliased to keep clippy's
/// `type_complexity` lint quiet.
type FrameCallback = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;

/// A booted Lumen app, driven from the page.
///
/// Construct one with the engine the app declares and its compiled script,
/// then either drive it yourself with [`Self::tick`] or hand it to
/// [`Self::start_frame_loop`] and let the browser's frame clock drive it.
#[wasm_bindgen]
pub struct LumenWebApp {
    app: App,
    host: ScriptHostAccess,
}

#[wasm_bindgen]
impl LumenWebApp {
    /// Boot an app, running `program` on the host for `engine`.
    ///
    /// `engine` and the bytes come from the manifest's script entry, so the app
    /// picks its own language rather than this module assuming one.
    ///
    /// A script that fails to load never throws: the app comes back and
    /// [`Self::script_error`] carries the reason, so a page can report a dead
    /// script instead of losing the module with it.
    ///
    /// # Errors
    ///
    /// This runtime was built with no host for `engine`.
    #[wasm_bindgen(constructor)]
    pub fn new(engine: &str, program: &[u8]) -> Result<LumenWebApp, JsError> {
        Self::with_uri(engine, program, DEFAULT_SCRIPT_URI)
    }

    /// Boot an app, naming the script in any load error.
    ///
    /// # Errors
    ///
    /// This runtime was built with no host for `engine`.
    #[wasm_bindgen(js_name = withUri)]
    pub fn with_uri(engine: &str, program: &[u8], uri: &str) -> Result<LumenWebApp, JsError> {
        let mut app = App::new();
        // The page lays out and paints; nothing here extracts a scene.
        app.extract_fns.clear();
        let host = hosts::install(&mut app, engine, program, uri)
            .map_err(|e| JsError::new(&e.to_string()))?;
        Ok(Self { app, host })
    }

    /// Run one tick of the app: the script's timers, derivations, and event
    /// dispatch, and the commands its builtins queued.
    pub fn tick(&mut self) {
        self.app.tick();
    }

    /// The value the script last wrote to `name`, as text, or `undefined` when
    /// the script never wrote it.
    #[must_use]
    pub fn signal(&self, name: &str) -> Option<String> {
        (self.host.signal)(&self.app.world, name)
            .as_ref()
            .map(ScriptValue::stringify)
    }

    /// Call an exported script function by name with no arguments, returning
    /// its result as text.
    ///
    /// Returns `undefined` when the script exports no such function. Commands
    /// the call queued are put back so the next tick carries them, exactly as
    /// the app's own dispatchers do.
    ///
    /// # Errors
    ///
    /// The call raised a script runtime error, or no script is loaded.
    pub fn call(&mut self, name: &str) -> Result<Option<String>, JsError> {
        (self.host.call)(&mut self.app.world, name)
            .map(|ret| ret.map(|v| v.stringify()))
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// The names of every function the loaded script exports.
    #[must_use]
    pub fn exports(&self) -> Vec<String> {
        (self.host.exports)(&self.app.world)
    }

    /// Why the script failed to load, or `undefined` when it loaded cleanly.
    #[wasm_bindgen(js_name = scriptError)]
    #[must_use]
    pub fn script_error(&self) -> Option<String> {
        self.app
            .world
            .get_resource::<ScriptLoadFailure>()
            .map(|f| f.0.clone())
    }

    /// Tick the app once per animation frame, for as long as the page lives.
    ///
    /// Consumes the app: the frame callback owns it from here, and the page
    /// reaches it through the DOM rather than through this handle.
    ///
    /// # Errors
    ///
    /// There is no `window` to request frames from, which is the case in a
    /// worker.
    #[wasm_bindgen(js_name = startFrameLoop)]
    pub fn start_frame_loop(self) -> Result<(), JsError> {
        if web_sys::window().is_none() {
            return Err(JsError::new("no window to render into"));
        }
        let app = Rc::new(RefCell::new(self));
        // The callback re-arms itself, so it has to outlive the call that
        // created it and be reachable from inside its own body; the cell is
        // what breaks that cycle.
        let next: FrameCallback = Rc::new(RefCell::new(None));
        let armed = Rc::clone(&next);
        *armed.borrow_mut() = Some(Closure::wrap(Box::new(move || {
            app.borrow_mut().tick();
            if let Some(callback) = next.borrow().as_ref() {
                request_frame(callback);
            }
        }) as Box<dyn FnMut()>));
        if let Some(callback) = armed.borrow().as_ref() {
            request_frame(callback);
        }
        // Handed to the browser, which now owns the callback chain.
        std::mem::forget(armed);
        Ok(())
    }
}

impl LumenWebApp {
    /// Take over an app that is already assembled and whose host, if it has
    /// one, is already installed.
    ///
    /// `engine` names the host to read signals and call exports through. An
    /// app with no script has none, and the accessors answer with nothing.
    pub(crate) fn from_parts(app: App, engine: Option<String>) -> Self {
        let host = engine
            .and_then(|engine| hosts::access(&engine))
            .unwrap_or_else(ScriptHostAccess::absent);
        Self { app, host }
    }
}

/// Ask the browser to run `callback` on the next animation frame. A missing
/// window or a refused request drops the frame rather than failing the tick;
/// the page is going away in both cases.
fn request_frame(callback: &Closure<dyn FnMut()>) {
    if let Some(window) = web_sys::window() {
        let _ = window.request_animation_frame(callback.as_ref().unchecked_ref());
    }
}
