//! A panicking module constructor must explain itself on stderr even when the
//! app installed its own panic hook.
//!
//! `install_with` cannot rely on the default hook printing the message: an app
//! (or an embedder) may have replaced it with a silent one, and the loader's
//! banner then pointed at a message that never appeared. So `install_with`
//! captures the payload and prints it itself. Proving that needs a real
//! process boundary - libtest's output capture hides in-process stderr - so
//! the test re-runs itself as a child with a silent hook installed and reads
//! the child's stderr.

use lumen_module::{App, INSTALL_PANICKED, Plugin, install_with};

const CHILD_ENV: &str = "LUMEN_MODULE_PANIC_CHILD";
const MESSAGE: &str = "the flux capacitor is missing";

struct Boom;

impl Plugin for Boom {
    fn build(self, _app: &mut App) {
        panic!("{MESSAGE}");
    }
}

#[test]
fn the_panic_message_reaches_stderr_despite_a_silent_hook() {
    if std::env::var_os(CHILD_ENV).is_some() {
        // Child arm: swallow everything the panic hook would print, so the
        // only way the message reaches stderr is install_with printing it.
        std::panic::set_hook(Box::new(|_| {}));
        let mut app = App::new();
        let status = install_with(&mut app, "", |_config| Boom);
        assert_eq!(status, INSTALL_PANICKED);
        return;
    }

    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args([
            "the_panic_message_reaches_stderr_despite_a_silent_hook",
            "--exact",
            // Without this, libtest captures the child's stderr and discards
            // it when the child passes; the parent is asserting on that
            // stream.
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("child test run");
    assert!(
        out.status.success(),
        "child failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(MESSAGE),
        "install_with printed the captured panic message:\n{stderr}"
    );
    assert!(
        !stderr.contains("panicked at"),
        "the silent hook stayed silent; only install_with spoke:\n{stderr}"
    );
}
