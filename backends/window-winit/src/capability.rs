//! The `window-winit` capability: this backend in the app's window-backend
//! registry, under the name `winit`.

use lumen_capability::CapabilityEnv;
use lumen_core::app::App;
use lumen_core::backends::register_backend;
use lumen_core::traits::WindowBackend;
use lumen_core::window_backend::WindowBackendEntry;

use crate::WinitBackend;

/// The name the backend is registered under.
pub const NAME: &str = "winit";

/// The rank among window backends; the highest one runs the app.
pub const PRIORITY: i32 = 0;

/// Register the backend. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, _env: &CapabilityEnv) {
    register_backend(
        app,
        WindowBackendEntry {
            name: NAME,
            priority: PRIORITY,
            backend,
        },
    );
}

fn backend() -> Box<dyn WindowBackend> {
    Box::new(WinitBackend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::window_backend::WindowBackends;

    /// Installing the capability registers `winit` as the backend a launch
    /// prefers. Running it needs a display, which a test does not have.
    #[test]
    fn the_capability_registers_the_winit_backend() {
        let mut app = App::new();
        install(
            &mut app,
            &CapabilityEnv::new(".", toml::Table::new(), String::new(), false),
        );
        let preferred = app
            .world
            .resource::<WindowBackends>()
            .preferred()
            .expect("registered");
        assert_eq!((preferred.name, preferred.priority), (NAME, PRIORITY));
    }
}
