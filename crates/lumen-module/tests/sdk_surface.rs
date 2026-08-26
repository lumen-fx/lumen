//! Drives the authoring surface in-process: the config wrapper, the install
//! body, and the [`lumen_module!`] expansion.
//!
//! The fixture module exercises the same surface end to end, but it is built
//! by a nested cargo without instrumentation, so nothing it runs is measured.
//! Expanding the macro here links its probe into this test binary, where a
//! local extern block reaches it; the install entry keeps the Rust ABI the
//! loader calls it with, so its body is driven through [`install_with`]
//! directly, which is the whole of it.

#![cfg(not(windows))]

use std::os::raw::c_char;
use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::Mutex;

use lumen_module::{
    App, BUILD_ID_C, INSTALL_BAD_CONFIG, INSTALL_OK, INSTALL_PANICKED, ModuleConfig, Plugin,
    install_with, lumen_module,
};
use serde::Deserialize;

static BUILT: AtomicBool = AtomicBool::new(false);
static UNITS: Mutex<String> = Mutex::new(String::new());

struct Probe {
    units: String,
}

impl Plugin for Probe {
    fn build(self, _app: &mut App) {
        BUILT.store(true, Ordering::SeqCst);
        *UNITS.lock().unwrap() = self.units;
    }
}

lumen_module!(|config: ModuleConfig| Probe {
    units: config.str("units").unwrap_or("px").to_string(),
});

unsafe extern "C" {
    fn lumen_module_probe() -> *const c_char;
}

#[test]
fn the_probe_answers_the_build_id() {
    let probe = unsafe { std::ffi::CStr::from_ptr(lumen_module_probe()) };
    assert_eq!(probe.to_bytes_with_nul(), BUILD_ID_C.as_bytes());
}

#[test]
fn install_parses_the_config_and_builds_the_plugin() {
    let mut app = App::new();
    let status = install_with(&mut app, "units = \"mm\"", |config: ModuleConfig| Probe {
        units: config.str("units").unwrap_or("px").to_string(),
    });
    assert_eq!(status, INSTALL_OK);
    assert!(BUILT.load(Ordering::SeqCst));
    assert_eq!(*UNITS.lock().unwrap(), "mm");
}

#[test]
fn install_reports_a_config_that_does_not_parse() {
    let mut app = App::new();
    let status = install_with(&mut app, "not [ toml", |_: ModuleConfig| Probe {
        units: String::new(),
    });
    assert_eq!(status, INSTALL_BAD_CONFIG);
}

#[test]
fn install_turns_a_panicking_constructor_into_a_status() {
    let mut app = App::new();
    let status = install_with(&mut app, "", |_: ModuleConfig| -> Probe {
        panic!("the constructor blew up");
    });
    assert_eq!(status, INSTALL_PANICKED);
}

#[test]
fn the_config_wrapper_reads_each_shape() {
    let mut app = App::new();
    let status = install_with(
        &mut app,
        "units = \"mm\"\nloud = true\ncount = 3",
        |config: ModuleConfig| {
            assert_eq!(config.str("units"), Some("mm"));
            assert_eq!(config.bool("loud"), Some(true));
            assert_eq!(config.int("count"), Some(3));
            assert_eq!(config.str("absent"), None);
            assert_eq!(config.table().len(), 3);

            #[derive(Deserialize)]
            struct Typed {
                units: String,
                count: i64,
            }
            let typed: Typed = config.typed().expect("the table matches the type");
            assert_eq!(typed.units, "mm");
            assert_eq!(typed.count, 3);

            #[derive(Deserialize, Debug)]
            struct Wrong {
                #[allow(dead_code)]
                units: i64,
            }
            let err = config.typed::<Wrong>().unwrap_err();
            assert!(err.contains("invalid type"), "{err}");

            Probe {
                units: config.str("units").unwrap().to_string(),
            }
        },
    );
    assert_eq!(status, INSTALL_OK);
}
