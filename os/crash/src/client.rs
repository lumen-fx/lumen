//! Starting the `lumen-crash` helper and talking to it.
//!
//! The helper is started as soon as crash handling is installed, and
//! connected to from a thread of its own, so the app does not wait for it.
//! Until it answers, a crash is written in-process; a panic in that window
//! waits a moment for the connection first.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lumen_crash_report::protocol::{Hello, Message};
use minidumper::{Client, SocketName};

use crate::{Config, Installed};

/// How long the app keeps trying to reach a helper it started.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The live connection, null until the helper answers. A raw pointer so the
/// fault handler reads it with one atomic load; every connection is leaked,
/// so a pointer once published stays valid for the life of the process.
static CLIENT: AtomicPtr<Client> = AtomicPtr::new(std::ptr::null_mut());
/// The socket the helper listens on.
static SOCKET: OnceLock<String> = OnceLock::new();
/// The helper's process id, 0 when there is none.
static HELPER_PID: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Connection {
    Pending,
    Up,
    Down,
}

static CONNECTION: Mutex<Connection> = Mutex::new(Connection::Down);
static CHANGED: Condvar = Condvar::new();

fn set_connection(state: Connection) {
    *CONNECTION.lock().unwrap_or_else(|e| e.into_inner()) = state;
    CHANGED.notify_all();
}

/// Find and start the helper, and connect to it in the background.
pub(crate) fn start(config: &Config) -> Installed {
    let Some(helper) = find_helper(config.helper.as_deref()) else {
        return Installed::InProcess(format!(
            "no {} beside the app",
            lumen_crash_report::helper_file_name()
        ));
    };
    let socket = SOCKET.get_or_init(socket_name);
    let mut command = Command::new(&helper);
    command
        .args(["--socket", socket, "--pid", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Installed::InProcess(format!("could not start {}: {e}", helper.display()));
        }
    };
    HELPER_PID.store(child.id(), Ordering::Relaxed);
    set_connection(Connection::Pending);
    let hello = hello(config);
    let spawned = std::thread::Builder::new()
        .name("lumen-crash-connect".into())
        .spawn(move || {
            let start = Instant::now();
            loop {
                if let Ok(client) = Client::with_name(socket_name_of(socket)) {
                    connected(client, &hello);
                    return;
                }
                let gone = matches!(child.try_wait(), Ok(Some(_)));
                if gone || start.elapsed() > CONNECT_TIMEOUT {
                    HELPER_PID.store(0, Ordering::Relaxed);
                    set_connection(Connection::Down);
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
    if spawned.is_err() {
        set_connection(Connection::Down);
        return Installed::InProcess("could not start the connecting thread".into());
    }
    Installed::Helper(helper)
}

fn hello(config: &Config) -> Hello {
    Hello {
        app_name: config.app_name.clone(),
        app_id: config.app_id.clone(),
        exe: std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        lumen_version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        crash_dir: config.crash_dir.clone(),
        dialog: config.dialog,
    }
}

fn connected(client: Client, hello: &Hello) {
    let client: &'static Client = Box::leak(Box::new(client));
    send_on(client, &Message::Hello(hello.clone()));
    crate::native::allow_dumps_from(HELPER_PID.load(Ordering::Relaxed));
    // Publish the connection with the ring held, so every line lands on the
    // helper exactly once: the kept ones now, later ones as they arrive.
    crate::logs::replay(
        |message| send_on(client, &message),
        || {
            CLIENT.store(std::ptr::from_ref(client).cast_mut(), Ordering::Release);
        },
    );
    set_connection(Connection::Up);
}

/// The live connection to the helper.
pub(crate) fn current() -> Option<&'static Client> {
    let ptr = CLIENT.load(Ordering::Acquire);
    // SAFETY: every pointer stored here comes from a leaked box and is never
    // freed.
    (!ptr.is_null()).then(|| unsafe { &*ptr })
}

/// The live connection, waiting up to `timeout` while the helper is still
/// being reached.
pub(crate) fn wait(timeout: Duration) -> Option<&'static Client> {
    let state = CONNECTION.lock().unwrap_or_else(|e| e.into_inner());
    let _ = CHANGED
        .wait_timeout_while(state, timeout, |s| *s == Connection::Pending)
        .map_err(|e| e.into_inner());
    current()
}

/// A second connection to the helper, opened before a panic's dump is
/// asked for: the helper drops the connection a dump arrives on, and seeing
/// none left would read as the app having exited.
pub(crate) fn open_next() -> Option<&'static Client> {
    let socket = SOCKET.get()?;
    let client = Client::with_name(socket_name_of(socket)).ok()?;
    Some(Box::leak(Box::new(client)))
}

/// Make `client` the live connection.
pub(crate) fn replace(client: &'static Client) {
    CLIENT.store(std::ptr::from_ref(client).cast_mut(), Ordering::Release);
}

/// Send `message` to the helper, when one is connected.
pub(crate) fn send(message: Message) {
    if let Some(client) = current() {
        send_on(client, &message);
    }
}

fn send_on(client: &Client, message: &Message) {
    let (kind, payload) = message.encode();
    let _ = client.send_message(kind, payload);
}

/// A socket name nobody else uses: the process id and the time.
fn socket_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("lumen-crash-{}-{nanos:x}", std::process::id());
    if cfg!(any(target_os = "linux", target_os = "android")) {
        return name;
    }
    // A socket path has to fit `sun_path` (104 bytes on macOS); a long temp
    // directory falls back to `/tmp`.
    let path = std::env::temp_dir().join(format!("{name}.sock"));
    let path = if path.as_os_str().len() < 100 || cfg!(windows) {
        path
    } else {
        PathBuf::from("/tmp").join(format!("{name}.sock"))
    };
    path.display().to_string()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn socket_name_of(name: &str) -> SocketName<'_> {
    SocketName::abstract_namespace(name)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn socket_name_of(name: &str) -> SocketName<'_> {
    SocketName::Path(Path::new(name))
}

/// The helper to start: the configured one, else `lumen-crash` beside the
/// executable, else beside the library this code is linked into (the
/// engine a C++ or Python host loads).
fn find_helper(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return path.is_file().then(|| path.to_path_buf());
    }
    let name = lumen_crash_report::helper_file_name();
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(&name)));
    let beside_lib = crate::native::module_dir().map(|d| d.join(&name));
    [beside_exe, beside_lib]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
}

/// Keep the helper out of the app's console session: a Ctrl+C in the
/// terminal that started the app must not take the helper down with it.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(not(any(unix, windows)))]
fn detach(_command: &mut Command) {}
