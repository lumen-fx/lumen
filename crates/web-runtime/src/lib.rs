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
//! Scripts run as precompiled candela bytecode on `candela-vm`. The compiler
//! stays out of the page: an app's `.cdl` is built to a `.cdlb` image ahead of
//! time, and the image's `host` declarations bind by name against the builtins
//! this module registers.
//!
//! This is the first step of that runtime. It boots an app from a `.cdlb`,
//! ticks it, and exposes the script's signal writes and load failures to the
//! page. Adopting prerendered HTML, projecting the ECS onto real elements, and
//! routing DOM events all come later.
//!
//! ```js
//! import init, { LumenWebApp } from "./lumen_web_runtime.js";
//!
//! await init();
//! const app = new LumenWebApp(await (await fetch("app.cdlb")).arrayBuffer());
//! if (app.scriptError()) console.error(app.scriptError());
//! app.startFrameLoop();
//! ```

#![warn(missing_docs)]

use std::cell::RefCell;
use std::rc::Rc;

use lumen_core::prelude::App;
use lumen_script::{ScriptHost, ScriptLoadFailure, ScriptValue};
use lumen_script_candela::{CandelaVmHost, ScriptCandelaVmPlugin};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// Name the load failure reports when the page supplies no better one.
const DEFAULT_ARTIFACT_URI: &str = "app.cdlb";

/// The self-re-arming animation-frame callback. Aliased to keep clippy's
/// `type_complexity` lint quiet.
type FrameCallback = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;

/// A booted Lumen app, driven from the page.
///
/// Construct one with the app's `.cdlb` bytes, then either drive it yourself
/// with [`Self::tick`] or hand it to [`Self::start_frame_loop`] and let the
/// browser's frame clock drive it.
#[wasm_bindgen]
pub struct LumenWebApp {
    app: App,
}

#[wasm_bindgen]
impl LumenWebApp {
    /// Boot an app over the candela bytecode in `artifact`.
    ///
    /// A script that fails to load never throws: the app comes back and
    /// [`Self::script_error`] carries the reason, so a page can report a dead
    /// script instead of losing the module with it.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(artifact: &[u8]) -> Self {
        Self::with_uri(artifact, DEFAULT_ARTIFACT_URI)
    }

    /// Boot an app, naming the artifact in any load error.
    #[wasm_bindgen(js_name = withUri)]
    #[must_use]
    pub fn with_uri(artifact: &[u8], uri: &str) -> Self {
        let mut app = App::new();
        // The page lays out and paints; nothing here extracts a scene.
        app.extract_fns.clear();
        app.add_plugin(ScriptCandelaVmPlugin::new(artifact.to_vec()).with_uri(uri));
        Self { app }
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
        self.host()
            .and_then(|h| h.mirror_get(name))
            .as_ref()
            .map(ScriptValue::stringify)
    }

    /// Call an exported script function by name with no arguments, returning
    /// its result as text.
    ///
    /// Returns `undefined` when the artifact exports no such function.
    /// Commands the call queued are put back so the next tick carries them,
    /// exactly as the app's own dispatchers do.
    ///
    /// # Errors
    ///
    /// The call raised a script runtime error, or no script is loaded.
    pub fn call(&mut self, name: &str) -> Result<Option<String>, JsError> {
        let Some(mut host) = self.app.world.get_resource_mut::<CandelaVmHost>() else {
            return Err(JsError::new("no script is loaded"));
        };
        let outcome = host
            .call(name, &[])
            .map_err(|e| JsError::new(&e.to_string()))?;
        host.push_commands(outcome.commands);
        Ok(outcome.ret.filter(|_| outcome.found).map(|v| v.stringify()))
    }

    /// The names of every function the loaded artifact exports.
    #[must_use]
    pub fn exports(&self) -> Vec<String> {
        self.host().map(CandelaVmHost::exports).unwrap_or_default()
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

    /// The script host, once one is installed.
    fn host(&self) -> Option<&CandelaVmHost> {
        self.app.world.get_resource::<CandelaVmHost>()
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
