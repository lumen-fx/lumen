//! Shared scaffolding for the loader tests: a fixture copy per test, and a
//! host that records what the plugins pushed at it.
//!
//! Every test takes its own copy of the fixture cdylib. A runtime plugin
//! holds one instance per process, so a test wanting a different `config`
//! table needs a library of its own.

// Each test binary compiles this module whole and uses part of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use lumen_core::app::App;
use lumen_plugin::abi::LogLevel;
use lumen_plugin::{
    HostHooks, InitEnv, LoadFailure, PluginEvent, PluginSet, ResolvedModule, testing,
};
use lumen_script::{ScriptCommand, ScriptFnRegistry, ScriptResult, ScriptValue};

/// A module declaration over a fresh copy of the fixture.
pub fn fixture_module(tag: &str, config: &str) -> ResolvedModule {
    module("lumen-plugin-fixture", &testing::fixture_copy(tag), config)
}

/// A module declaration over an arbitrary file.
pub fn module(name: &str, path: &Path, config: &str) -> ResolvedModule {
    ResolvedModule {
        name: name.to_string(),
        path: path.to_path_buf(),
        config: toml::from_str(config).expect("the test config parses"),
    }
}

/// A scratch app directory, keyed by pid so two runs cannot collide.
pub fn app_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lumen-plugin-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the app directory is writable");
    dir
}

/// The environment a test's modules are initialized with.
pub fn env(app_dir: &Path) -> InitEnv {
    InitEnv {
        app_dir: app_dir.to_path_buf(),
        app_id: "test-app".to_string(),
        headless: true,
        hot_reload: false,
    }
}

/// Load one fixture copy configured with `config`.
pub fn load_fixture(tag: &str, config: &str) -> (PluginSet, Vec<LoadFailure>, Arc<Recorder>) {
    let dir = app_dir(tag);
    let hooks = Arc::new(Recorder::default());
    let (set, failures) = PluginSet::load(
        &[fixture_module(tag, config)],
        &env(&dir),
        Arc::clone(&hooks) as Arc<dyn HostHooks>,
    );
    (set, failures, hooks)
}

/// One fixture copy, loaded and bound onto an app.
pub struct Installed {
    pub app: App,
    /// Held: the function bodies bound onto the app call into it.
    pub set: PluginSet,
    pub hooks: Arc<Recorder>,
}

impl Installed {
    /// Call a bound function by name.
    pub fn call(&self, name: &str, args: &[ScriptValue]) -> (ScriptResult, Vec<ScriptCommand>) {
        self.registry()
            .fns()
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no function called {name}"))
            .invoke(args)
    }

    /// The registry the set installed into.
    pub fn registry(&self) -> &ScriptFnRegistry {
        self.app.world.resource::<ScriptFnRegistry>()
    }
}

/// Load one fixture copy and bind it onto a fresh app.
pub fn install_fixture(tag: &str, config: &str) -> Installed {
    let (set, failures, hooks) = load_fixture(tag, config);
    assert!(failures.is_empty(), "{failures:?}");
    let mut app = App::new();
    set.install(&mut app);
    Installed { app, set, hooks }
}

/// A host that keeps everything it was handed.
#[derive(Default)]
pub struct Recorder {
    events: Mutex<Vec<(String, PluginEvent)>>,
    logs: Mutex<Vec<(String, LogLevel, String)>>,
    wakes: AtomicUsize,
}

impl Recorder {
    pub fn events(&self) -> Vec<(String, PluginEvent)> {
        self.events
            .lock()
            .expect("the recorder is not poisoned")
            .clone()
    }

    pub fn logs(&self) -> Vec<(String, LogLevel, String)> {
        self.logs
            .lock()
            .expect("the recorder is not poisoned")
            .clone()
    }

    pub fn wakes(&self) -> usize {
        self.wakes.load(Ordering::Relaxed)
    }
}

impl HostHooks for Recorder {
    fn event(&self, module: &str, event: PluginEvent) -> bool {
        self.events
            .lock()
            .expect("the recorder is not poisoned")
            .push((module.to_string(), event));
        true
    }

    fn log(&self, module: &str, level: LogLevel, message: &str) {
        self.logs
            .lock()
            .expect("the recorder is not poisoned")
            .push((module.to_string(), level, message.to_string()));
    }

    fn wake(&self) {
        self.wakes.fetch_add(1, Ordering::Relaxed);
    }
}

/// A host that forwards events down a channel, so a test can watch them
/// arrive from the plugin's own thread and can hang up.
pub struct ChannelHooks {
    tx: Mutex<mpsc::Sender<PluginEvent>>,
    wakes: AtomicUsize,
}

impl ChannelHooks {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<PluginEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(Self {
                tx: Mutex::new(tx),
                wakes: AtomicUsize::new(0),
            }),
            rx,
        )
    }

    pub fn wakes(&self) -> usize {
        self.wakes.load(Ordering::Relaxed)
    }
}

impl HostHooks for ChannelHooks {
    fn event(&self, _module: &str, event: PluginEvent) -> bool {
        self.tx
            .lock()
            .expect("the channel is not poisoned")
            .send(event)
            .is_ok()
    }

    fn log(&self, _module: &str, _level: LogLevel, _message: &str) {}

    fn wake(&self) {
        self.wakes.fetch_add(1, Ordering::Relaxed);
    }
}
