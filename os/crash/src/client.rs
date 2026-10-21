//! Starting the `lumen-crash` helper and talking to it.
//!
//! The helper is started as soon as crash handling is installed, and
//! connected to from a thread of its own, so the app does not wait for it.
//! Until it answers, a crash is written in-process; a panic in that window
//! waits a moment for the connection first.

use std::path::{Path, PathBuf};
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
    let pid = std::process::id().to_string();
    let mut child = match spawn::helper(&helper, &["--socket", socket, "--pid", &pid]) {
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
                // Up only once the helper answers: a connection succeeds
                // while it waits in the listener's queue, and on macOS the
                // handshake reads a helper that closed it unread as a yes.
                if let Ok(client) = Client::with_name(socket_name_of(socket))
                    && client.ping().is_ok()
                {
                    connected(client, &hello);
                    return;
                }
                let gone = child.exited();
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

/// Starting the helper as a process of its own: no standard streams, and
/// nothing else of the app's either. The helper outlives the app (it writes
/// the report and waits on the dialog after the app is gone), so any handle
/// it held of the app's would stay open: a pipe the app's output goes into
/// would never reach its end.
mod spawn {
    use std::io;
    use std::path::Path;

    /// Start `program` with `args`, detached from the app's console session
    /// so a Ctrl+C in the terminal that started the app does not take the
    /// helper down with it.
    pub(super) fn helper(program: &Path, args: &[&str]) -> io::Result<Helper> {
        imp::spawn(program, args)
    }

    pub(super) use imp::Helper;

    #[cfg(unix)]
    mod imp {
        use std::io;
        use std::os::unix::process::CommandExt;
        use std::path::Path;
        use std::process::{Child, Command, Stdio};

        /// The started helper.
        pub(in super::super) struct Helper(Child);

        impl Helper {
            pub(in super::super) fn id(&self) -> u32 {
                self.0.id()
            }

            /// Whether the helper has ended.
            pub(in super::super) fn exited(&mut self) -> bool {
                matches!(self.0.try_wait(), Ok(Some(_)))
            }
        }

        /// std closes every descriptor but the three standard ones in the
        /// child, and those three are null here.
        pub(super) fn spawn(program: &Path, args: &[&str]) -> io::Result<Helper> {
            Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .map(Helper)
        }
    }

    /// std starts a child with every inheritable handle of the parent (the
    /// standard streams a shell redirected among them), so the helper is
    /// started with `CreateProcessW` directly and inherits none.
    #[cfg(windows)]
    mod imp {
        use std::ffi::OsStr;
        use std::io;
        use std::os::windows::ffi::OsStrExt;
        use std::path::Path;

        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION,
            STARTUPINFOW, WaitForSingleObject,
        };

        /// The started helper: its id and a handle to its process.
        pub(in super::super) struct Helper {
            id: u32,
            process: HANDLE,
        }

        // SAFETY: a process handle is a kernel object reference, usable from
        // any thread.
        unsafe impl Send for Helper {}

        impl Helper {
            pub(in super::super) fn id(&self) -> u32 {
                self.id
            }

            /// Whether the helper has ended.
            pub(in super::super) fn exited(&mut self) -> bool {
                // SAFETY: `process` is the handle CreateProcessW returned,
                // open until drop.
                unsafe { WaitForSingleObject(self.process, 0) == WAIT_OBJECT_0 }
            }
        }

        impl Drop for Helper {
            fn drop(&mut self) {
                // SAFETY: closed once, here.
                unsafe { CloseHandle(self.process) };
            }
        }

        pub(super) fn spawn(program: &Path, args: &[&str]) -> io::Result<Helper> {
            let mut line = quote(program.as_os_str());
            for arg in args {
                line.push(u16::from(b' '));
                line.extend(quote(OsStr::new(arg)));
            }
            line.push(0);
            let mut app: Vec<u16> = program.as_os_str().encode_wide().collect();
            app.push(0);
            // SAFETY: zeroed is a valid STARTUPINFOW once `cb` is set; no
            // STARTF_USESTDHANDLES, so the child gets no standard streams.
            let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            // SAFETY: plain out-parameter.
            let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: both strings are NUL-terminated and live across the
            // call; `bInheritHandles` is false, so no handle crosses over.
            let ok = unsafe {
                CreateProcessW(
                    app.as_ptr(),
                    line.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
                    std::ptr::null(),
                    std::ptr::null(),
                    &startup,
                    &mut info,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the thread handle is not needed; closed once.
            unsafe { CloseHandle(info.hThread) };
            Ok(Helper {
                id: info.dwProcessId,
                process: info.hProcess,
            })
        }

        /// One argument as the C runtime's command-line parser reads it back:
        /// quoted when it holds a space, a tab or a quote, with the
        /// backslashes before a quote doubled.
        fn quote(arg: &OsStr) -> Vec<u16> {
            let wide: Vec<u16> = arg.encode_wide().collect();
            let plain = !wide.is_empty()
                && !wide.iter().any(|&c| {
                    c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"')
                });
            if plain {
                return wide;
            }
            let backslash = u16::from(b'\\');
            let mut out = vec![u16::from(b'"')];
            let mut slashes = 0;
            for &c in &wide {
                if c == backslash {
                    slashes += 1;
                } else if c == u16::from(b'"') {
                    out.extend(std::iter::repeat_n(backslash, slashes * 2 + 1));
                    slashes = 0;
                } else {
                    out.extend(std::iter::repeat_n(backslash, slashes));
                    slashes = 0;
                }
                if c != backslash {
                    out.push(c);
                }
            }
            out.extend(std::iter::repeat_n(backslash, slashes * 2));
            out.push(u16::from(b'"'));
            out
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            fn q(arg: &str) -> String {
                String::from_utf16(&quote(OsStr::new(arg))).unwrap()
            }

            #[test]
            fn arguments_quote_the_way_the_c_runtime_reads_them() {
                assert_eq!(q("--pid"), "--pid");
                assert_eq!(q(r"C:\Users\A B\x.sock"), r#""C:\Users\A B\x.sock""#);
                assert_eq!(q(r"C:\dir with space\"), r#""C:\dir with space\\""#);
                assert_eq!(q(r#"say "hi""#), r#""say \"hi\"""#);
                assert_eq!(q(""), r#""""#);
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    mod imp {
        use std::io;
        use std::path::Path;
        use std::process::{Child, Command, Stdio};

        pub(in super::super) struct Helper(Child);

        impl Helper {
            pub(in super::super) fn id(&self) -> u32 {
                self.0.id()
            }

            pub(in super::super) fn exited(&mut self) -> bool {
                matches!(self.0.try_wait(), Ok(Some(_)))
            }
        }

        pub(super) fn spawn(program: &Path, args: &[&str]) -> io::Result<Helper> {
            Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map(Helper)
        }
    }
}
