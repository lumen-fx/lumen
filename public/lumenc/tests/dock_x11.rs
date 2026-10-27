// Starts the lumenc binary, which the dock fixture needs built with the
// bundled modules `dev-run` links.
#![cfg(all(feature = "dev-run", target_os = "linux"))]

//! Drags across real windows on X11: the dock fixture runs on a private
//! Xvfb server, the pointer is pressed, moved and released through XTest,
//! and what the app prints and the windows the server has are read back. A
//! tab torn off past the window's edge opens in a window of its own that
//! follows the pointer; that window's tab dragged back over the main window
//! docks it there.
//!
//! The test starts its own server, never the desktop's. Without an `Xvfb`
//! on the `PATH` it says so and passes: the drag code it reaches is the
//! winit backend's, which no other test can run.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, MapState, Window};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

/// A child process killed when the test is done with it.
struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

/// A private X server on a free display number.
fn xvfb() -> Option<(Proc, String)> {
    if !on_path("Xvfb") {
        return None;
    }
    let n = (140..200).find(|n| {
        !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
            && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
    })?;
    let display = format!(":{n}");
    let child = Command::new("Xvfb")
        .args([&display, "-screen", "0", "1280x1024x24", "-nolisten", "tcp"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let proc = Proc(child);
    let socket = format!("/tmp/.X11-unix/X{n}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !Path::new(&socket).exists() {
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Some((proc, display))
}

/// Every mapped window with a name, as `(name, x, y, width, height)` on the
/// root.
fn windows(conn: &RustConnection, root: Window) -> Vec<(String, i32, i32, u16, u16)> {
    let mut out = Vec::new();
    let Ok(Ok(tree)) = conn.query_tree(root).map(|c| c.reply()) else {
        return out;
    };
    for w in tree.children {
        let Ok(Ok(attrs)) = conn.get_window_attributes(w).map(|c| c.reply()) else {
            continue;
        };
        if attrs.map_state != MapState::VIEWABLE {
            continue;
        }
        let name = conn
            .get_property(false, w, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.value).to_string())
            .unwrap_or_default();
        let Ok(Ok(geo)) = conn.get_geometry(w).map(|c| c.reply()) else {
            continue;
        };
        let Ok(Ok(at)) = conn.translate_coordinates(w, root, 0, 0).map(|c| c.reply()) else {
            continue;
        };
        if !name.is_empty() {
            out.push((
                name,
                i32::from(at.dst_x),
                i32::from(at.dst_y),
                geo.width,
                geo.height,
            ));
        }
    }
    out
}

/// Wait up to `secs` for `check` to hold.
fn wait_for(secs: u64, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    check()
}

fn motion(conn: &RustConnection, root: Window, x: i32, y: i32) {
    conn.xtest_fake_input(6, 0, 0, root, x as i16, y as i16, 0)
        .expect("motion");
    conn.flush().expect("flush");
}

fn button(conn: &RustConnection, root: Window, down: bool) {
    let kind = if down { 4 } else { 5 };
    conn.xtest_fake_input(kind, 1, 0, root, 0, 0, 0)
        .expect("button");
    conn.flush().expect("flush");
}

/// Press at `from`, move to `to` in steps a frame apart, release.
fn drag(conn: &RustConnection, root: Window, from: (i32, i32), to: (i32, i32)) {
    motion(conn, root, from.0, from.1);
    std::thread::sleep(Duration::from_millis(200));
    button(conn, root, true);
    std::thread::sleep(Duration::from_millis(200));
    let steps = 40;
    for k in 1..=steps {
        let x = from.0 + (to.0 - from.0) * k / steps;
        let y = from.1 + (to.1 - from.1) * k / steps;
        motion(conn, root, x, y);
        std::thread::sleep(Duration::from_millis(25));
    }
    std::thread::sleep(Duration::from_millis(300));
    button(conn, root, false);
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("app dir");
    for e in std::fs::read_dir(from).expect("fixture") {
        let e = e.expect("entry");
        let dest = to.join(e.file_name());
        if e.file_type().expect("type").is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).expect("copy");
        }
    }
}

fn read(path: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut s);
    }
    s
}

#[test]
fn a_tab_tears_off_into_a_window_and_docks_back_on_x11() {
    let Some((_server, display)) = xvfb() else {
        eprintln!("dock_x11: no Xvfb on the PATH; the drag across real windows is not driven");
        return;
    };
    let root_dir: PathBuf =
        std::env::temp_dir().join(format!("lumenc-dock-x11-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root_dir);
    let app_dir = root_dir.join("app");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dock-drag"),
        &app_dir,
    );
    let log = root_dir.join("out.txt");
    let out = std::fs::File::create(&log).expect("log");
    let app = Proc(
        Command::new(env!("CARGO_BIN_EXE_lumenc"))
            .arg("run")
            .arg(&app_dir)
            .env("DISPLAY", &display)
            .env("WINIT_UNIX_BACKEND", "x11")
            .env_remove("WAYLAND_DISPLAY")
            .env("XDG_DATA_HOME", root_dir.join("data"))
            .env("XDG_CONFIG_HOME", root_dir.join("config"))
            .stdout(out.try_clone().expect("log"))
            .stderr(out)
            .spawn()
            .expect("lumenc runs"),
    );
    let (conn, screen) = x11rb::connect(Some(&display)).expect("the private server");
    let root = conn.setup().roots[screen].root;
    let main = || {
        windows(&conn, root)
            .into_iter()
            .find(|w| w.0 == "dock-drag-check")
    };
    let started = wait_for(60, || {
        main().is_some() && read(&log).contains("dock-drag ready")
    });
    assert!(started, "the app did not come up:\n{}", read(&log));
    // Let the first frames land and the tabs lay out.
    std::thread::sleep(Duration::from_millis(1500));
    let (_, x, y, w, h) = main().expect("the main window");
    assert!(w >= 700, "the window has its size: {w}");
    let bottom = y + i32::from(h);

    // Files' tab, past the window's bottom edge: it tears off.
    let tab = (x + 40, y + 15);
    drag(&conn, root, tab, (x + 60, bottom + 140));
    let torn = wait_for(15, || {
        read(&log).contains("dock-drag files window")
            && windows(&conn, root).iter().any(|w| w.0 == "Files")
    });
    assert!(
        torn,
        "the tab tore off into a window of its own:\n{}\n{:?}",
        read(&log),
        windows(&conn, root)
    );
    let (_, fx, fy, _, _) = windows(&conn, root)
        .into_iter()
        .find(|w| w.0 == "Files")
        .expect("the pop-out");
    assert!(
        fy > bottom,
        "the pop-out was left where the pointer took it: {fx},{fy} below the main window's {bottom}"
    );
    std::thread::sleep(Duration::from_millis(800));

    // Its tab dragged over the main window's center stack docks it there.
    let handle = (fx + 40, fy + 15);
    drag(&conn, root, handle, (x + 520, y + 280));
    let docked = wait_for(15, || {
        read(&log).contains("dock-drag files dock")
            && !windows(&conn, root).iter().any(|w| w.0 == "Files")
    });
    assert!(
        docked,
        "the pop-out docked back:\n{}\n{:?}",
        read(&log),
        windows(&conn, root)
    );
    drop(app);
    let _ = std::fs::remove_dir_all(&root_dir);
}
