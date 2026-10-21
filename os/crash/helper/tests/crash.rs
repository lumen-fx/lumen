//! End to end: a process installs crash handling against the built helper,
//! crashes in one of the ways an app does, and the reports land on disk.
//!
//! Each case re-runs this test binary as the crashing app (the `child` test,
//! which does nothing unless the environment names a case), so the crash
//! takes down a child process and not the test runner. The dialog is off.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CASE: &str = "LUMEN_CRASH_TEST_CASE";

/// How a report names the fault the "segv" case raises: a signal on Unix,
/// a structured exception on Windows.
#[cfg(unix)]
const RAISED_FAULT: &str = "SIGSEGV";
#[cfg(windows)]
const RAISED_FAULT: &str = "EXCEPTION_ACCESS_VIOLATION";
const DIR: &str = "LUMEN_CRASH_TEST_DIR";

/// The crashing app. Runs only as a child of one of the tests below.
#[test]
fn child() {
    let (Ok(case), Ok(dir)) = (std::env::var(CASE), std::env::var(DIR)) else {
        return;
    };
    let dir = PathBuf::from(dir);
    if let Some(config) = case.strip_prefix("capability:") {
        // The runtime's path: the capability's preflight reads lumen.toml
        // and installs; no helper sits beside this test binary.
        let env = lumen_capability::CapabilityEnv::new(
            dir.join("Demo App"),
            toml::from_str(config).unwrap(),
            String::new(),
            false,
        );
        lumen_os_crash::capability::preflight(&env);
        panic!("crash-test capability panic");
    }
    let alone = case.ends_with("-alone");
    // A packaged app names no helper: it finds the one beside itself.
    let helper = if case.starts_with("beside-") {
        None
    } else if alone {
        Some(dir.join("no-such-helper"))
    } else {
        Some(PathBuf::from(env!("CARGO_BIN_EXE_lumen-crash")))
    };
    let installed = lumen_os_crash::install(lumen_os_crash::Config {
        app_name: "crash-test".into(),
        app_id: "crash-test".into(),
        crash_dir: dir,
        helper,
        // On only when a person runs a case by hand to look at the dialog.
        dialog: std::env::var_os("LUMEN_CRASH_TEST_DIALOG").is_some(),
    });
    if alone {
        assert!(
            matches!(installed, lumen_os_crash::Installed::InProcess(_)),
            "{installed:?}"
        );
    } else {
        assert!(
            matches!(installed, lumen_os_crash::Installed::Helper(_)),
            "{installed:?}"
        );
        assert!(
            lumen_os_crash::wait_for_helper(Duration::from_secs(10)),
            "the helper did not answer: {installed:?}"
        );
    }
    lumen_os_crash::log_line("crash-test: about to crash");
    lumen_os_crash::set_gpu("Test GPU (Vulkan), render-gpu");
    match base_case(&case) {
        "panic" => panic!("crash-test panic"),
        "thread-panic" => {
            let worker = std::thread::Builder::new()
                .name("worker".into())
                .spawn(|| panic!("crash-test worker panic"))
                .unwrap();
            let _ = worker.join();
            std::process::abort();
        }
        "segv" => raise_access_fault(),
        #[cfg(unix)]
        "fault" => read_unreadable_page(),
        "abort" => std::process::abort(),
        "survive" => {
            let caught = std::panic::catch_unwind(|| panic!("crash-test survived panic"));
            assert!(caught.is_err());
            lumen_os_crash::report_clean_exit();
            // Let the helper read the goodbye before the process ends.
            std::thread::sleep(Duration::from_millis(300));
        }
        other => panic!("unknown case {other}"),
    }
}

/// A case without its `beside-` and `-alone` variations.
fn base_case(case: &str) -> &str {
    case.trim_start_matches("beside-")
        .trim_end_matches("-alone")
}

/// A bad memory access: a read from a page mapped with no access.
#[cfg(unix)]
fn read_unreadable_page() {
    // SAFETY: maps one fresh page and reads it; the read faults by design,
    // which is the point of the case, and touches no memory Rust owns.
    unsafe {
        let page = libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        assert_ne!(page, libc::MAP_FAILED);
        std::ptr::read_volatile(page.cast::<u8>());
    }
}

/// Ends the process the way a bad memory access does, without one: the
/// signal on Unix, the structured exception on Windows.
fn raise_access_fault() {
    #[cfg(unix)]
    // SAFETY: raising a signal has no memory-safety preconditions.
    unsafe {
        libc::raise(libc::SIGSEGV);
    }
    #[cfg(windows)]
    // SAFETY: the exception carries no arguments to read.
    unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::RaiseException(
            windows_sys::Win32::Foundation::EXCEPTION_ACCESS_VIOLATION as u32,
            0,
            0,
            std::ptr::null(),
        );
    }
}

/// Held while a test writes an executable and while one starts a process.
/// The tests run on threads of one process: a process started from one
/// thread holds every descriptor of the parent from its fork until its
/// exec, so an executable another thread is still writing would be open
/// for writing in it, and running that executable fails with "text file
/// busy" until the started process gets to its exec.
static FORKS: Mutex<()> = Mutex::new(());

/// Start `command`. `spawn` returns once the child has run its exec, which
/// closes what it inherited.
fn spawn(command: &mut Command) -> Child {
    let _forks = FORKS.lock().unwrap_or_else(|e| e.into_inner());
    command.spawn().unwrap()
}

/// Run `case` as a child crashing into a fresh folder; the folder.
fn crash(case: &str) -> PathBuf {
    crash_as(case, &std::env::current_exe().unwrap())
}

/// The folder a case's reports go to.
fn case_dir(case: &str) -> PathBuf {
    let tag: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(24)
        .collect();
    std::env::temp_dir().join(format!("lumen-crash-e2e-{tag}-{}", std::process::id()))
}

/// [`crash`], running `exe` (this test binary or a copy of it) as the app.
fn crash_as(case: &str, exe: &Path) -> PathBuf {
    let dir = case_dir(case);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut command = Command::new(exe);
    command
        .args(["child", "--exact", "--nocapture", "--test-threads=1"])
        .env(CASE, case)
        .env(DIR, &dir)
        .env("XDG_DATA_HOME", dir.join("data"))
        .env_remove("RUST_BACKTRACE")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let output = spawn(&mut command).wait_with_output().unwrap();
    let status = output.status;
    // What the child said, for a failure: a child that panicked before its
    // crash tells why here.
    let said = String::from_utf8_lossy(&output.stderr);
    if case.starts_with("survive") {
        assert!(status.success(), "{case}: {status}\n{said}");
    } else {
        assert!(!status.success(), "{case}: {status}\n{said}");
    }
    // After the report, a fault or a sent signal still ends the process the
    // way it would have without crash handling.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        let ended_by = status.signal();
        match base_case(case) {
            "segv" => assert_eq!(ended_by, Some(libc::SIGSEGV), "{case}: {status}\n{said}"),
            "fault" => assert!(
                matches!(ended_by, Some(libc::SIGSEGV | libc::SIGBUS)),
                "{case}: {status}\n{said}"
            ),
            _ => {}
        }
    }
    dir
}

/// The files in `dir`, sorted, once a `.txt` has appeared and the helper
/// has settled (or after `timeout` without one).
fn wait_for_report(dir: &Path, timeout: Duration) -> Vec<String> {
    let start = Instant::now();
    loop {
        let names = files(dir);
        let done = names.iter().any(|n| n.ends_with(".txt"))
            && !names.iter().any(|n| n.starts_with(".pending"));
        if done || start.elapsed() > timeout {
            return names;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn report(dir: &Path, names: &[String]) -> String {
    let txt = names
        .iter()
        .find(|n| n.ends_with(".txt"))
        .unwrap_or_else(|| panic!("no report in {}: {names:?}", dir.display()));
    std::fs::read_to_string(dir.join(txt)).unwrap()
}

/// Exactly one report and at most one minidump in `dir`, once the helper
/// has had time to write a second if there were going to be one.
fn assert_one_report(dir: &Path) {
    std::thread::sleep(Duration::from_secs(1));
    let names = files(dir);
    let count = |ext: &str| names.iter().filter(|n| n.ends_with(ext)).count();
    assert_eq!(count(".txt"), 1, "{names:?}");
    assert!(count(".dmp") <= 1, "{names:?}");
}

fn assert_minidump(dir: &Path, names: &[String], text: &str) {
    let dump = names
        .iter()
        .find(|n| n.ends_with(".dmp"))
        .unwrap_or_else(|| panic!("no minidump in {names:?}"));
    let bytes = std::fs::read(dir.join(dump)).unwrap();
    assert_eq!(&bytes[..4], b"MDMP", "{dump} is not a minidump");
    assert!(text.contains(&format!("Minidump:   {dump}")), "{text}");
}

#[test]
fn a_panic_that_ends_the_app_leaves_a_report_and_a_minidump() {
    let dir = crash("panic");
    let names = wait_for_report(&dir, Duration::from_secs(20));
    let text = report(&dir, &names);
    assert!(
        text.contains("crash-test stopped because of a panic."),
        "{text}"
    );
    assert!(text.contains("crash-test panic"), "{text}");
    // A panic's location is the path as the compiler saw it, with the
    // platform's separator.
    assert!(
        text.replace('\\', "/")
            .contains("Location:   os/crash/helper/tests/crash.rs:"),
        "{text}"
    );
    assert!(text.contains("Backtrace\n---------\n"), "{text}");
    assert!(
        text.contains("GPU:        Test GPU (Vulkan), render-gpu"),
        "{text}"
    );
    assert!(text.contains("crash-test: about to crash"), "{text}");
    assert_minidump(&dir, &names, &text);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The packaged layout: the app and `lumen-crash` side by side in one
/// folder, and the app naming no helper. It finds the one beside its
/// executable, which writes the report and the minidump.
#[test]
fn a_packaged_app_finds_the_helper_beside_itself() {
    let case = "beside-segv";
    let package = case_dir(case).with_extension("app");
    let _ = std::fs::remove_dir_all(&package);
    std::fs::create_dir_all(&package).unwrap();
    let suffix = std::env::consts::EXE_SUFFIX;
    let app = package.join(format!("crash-app{suffix}"));
    {
        let _forks = FORKS.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::copy(std::env::current_exe().unwrap(), &app).unwrap();
        std::fs::copy(
            env!("CARGO_BIN_EXE_lumen-crash"),
            package.join(format!("lumen-crash{suffix}")),
        )
        .unwrap();
    }

    let dir = crash_as(case, &app);
    let names = wait_for_report(&dir, Duration::from_secs(20));
    let text = report(&dir, &names);
    assert!(text.contains("native fault"), "{text}");
    assert!(text.contains(RAISED_FAULT), "{text}");
    assert!(
        text.contains(&format!("crash-app{suffix}")),
        "the report names the packaged executable: {text}"
    );
    assert_minidump(&dir, &names, &text);
    std::fs::remove_dir_all(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&package);
}

#[test]
fn a_survived_panic_leaves_nothing() {
    let dir = crash("survive");
    // The dump of the panicking moment is written and then dropped when the
    // app says it is fine; give the helper time to finish both.
    std::thread::sleep(Duration::from_secs(2));
    assert!(files(&dir).is_empty(), "{:?}", files(&dir));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_worker_panic_is_reported_first_when_the_app_then_dies() {
    let dir = crash("thread-panic");
    let names = wait_for_report(&dir, Duration::from_secs(20));
    let text = report(&dir, &names);
    assert!(text.contains("crash-test worker panic"), "{text}");
    assert!(text.contains("Thread:     worker"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_segfault_leaves_a_report_with_the_fault_and_a_minidump() {
    let dir = crash("segv");
    let names = wait_for_report(&dir, Duration::from_secs(20));
    let text = report(&dir, &names);
    assert!(
        text.contains(&format!(
            "stopped because of a native fault: {RAISED_FAULT}"
        )),
        "{text}"
    );
    // Linux records the signal's address, which is none for a sent one;
    // macOS has no address for a sent signal and says it was one.
    #[cfg(target_os = "linux")]
    assert!(text.contains("Address:    0x0000000000000000"), "{text}");
    #[cfg(target_os = "macos")]
    {
        assert!(text.contains("SIGSEGV / EXC_SOFT_SIGNAL"), "{text}");
        assert!(!text.contains("Address:"), "{text}");
    }
    assert!(text.contains("Stack\n-----\n"), "{text}");
    assert_minidump(&dir, &names, &text);
    assert_one_report(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A real bad access, which on macOS is a Mach exception before it is a
/// signal: one report, naming the fault.
#[cfg(unix)]
#[test]
fn a_bad_access_leaves_one_report_with_the_fault() {
    let dir = crash("fault");
    let names = wait_for_report(&dir, Duration::from_secs(20));
    let text = report(&dir, &names);
    #[cfg(target_os = "linux")]
    assert!(
        text.contains("stopped because of a native fault: SIGSEGV"),
        "{text}"
    );
    #[cfg(target_os = "macos")]
    assert!(
        text.contains("stopped because of a native fault: EXC_BAD_ACCESS"),
        "{text}"
    );
    assert_minidump(&dir, &names, &text);
    assert_one_report(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Without the helper, a real bad access is written once from the handler.
#[cfg(unix)]
#[test]
fn without_the_helper_a_bad_access_is_written_once() {
    let dir = crash("fault-alone");
    let names = wait_for_report(&dir, Duration::from_secs(5));
    let text = report(&dir, &names);
    #[cfg(target_os = "linux")]
    assert!(text.contains("Fault:      SIGSEGV"), "{text}");
    #[cfg(target_os = "macos")]
    assert!(
        text.contains("Fault:      EXC_BAD_ACCESS (bad memory access)"),
        "{text}"
    );
    assert_one_report(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(windows)]
#[test]
fn an_access_violation_leaves_a_report_with_the_fault_and_a_minidump() {
    let dir = crash("segv");
    let names = wait_for_report(&dir, Duration::from_secs(20));
    let text = report(&dir, &names);
    assert!(text.contains("EXCEPTION_ACCESS_VIOLATION"), "{text}");
    assert_minidump(&dir, &names, &text);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Not on Windows: there `std::process::abort` ends the process with a fail
/// fast, which no handler in the process sees and which leaves nothing for
/// the helper to dump.
#[cfg(unix)]
#[test]
fn an_abort_leaves_a_report() {
    let dir = crash("abort");
    let names = wait_for_report(&dir, Duration::from_secs(20));
    let text = report(&dir, &names);
    assert!(text.contains("native fault: SIGABRT"), "{text}");
    assert_minidump(&dir, &names, &text);
    assert_one_report(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn without_the_helper_a_panic_is_written_in_process() {
    let dir = crash("panic-alone");
    let names = wait_for_report(&dir, Duration::from_secs(5));
    assert!(!names.iter().any(|n| n.ends_with(".dmp")), "{names:?}");
    let text = report(&dir, &names);
    assert!(
        text.contains("crash-test stopped because of a panic."),
        "{text}"
    );
    assert!(text.contains("crash-test panic"), "{text}");
    assert!(text.contains("crash-test: about to crash"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn without_the_helper_a_fault_is_written_from_the_handler() {
    let dir = crash("segv-alone");
    let names = wait_for_report(&dir, Duration::from_secs(5));
    let text = report(&dir, &names);
    assert!(
        text.starts_with("Lumen crash report\n\ncrash-test stopped because of a native fault.\n"),
        "{text}"
    );
    assert!(text.contains("App:        crash-test"), "{text}");
    #[cfg(unix)]
    assert!(
        text.contains("Fault:      SIGSEGV (segmentation fault), signal 11"),
        "{text}"
    );
    #[cfg(windows)]
    assert!(text.contains(RAISED_FAULT), "{text}");
    assert!(text.contains("no minidump was written"), "{text}");
    assert_one_report(&dir);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A connection that closes before the app says who it is does not end the
/// helper. On macOS a client reaches the socket before the helper's Mach
/// port is registered, finds no port, and closes; the app's next attempt
/// has to find the helper still there.
#[test]
fn a_connection_that_closes_unannounced_leaves_the_helper_running() {
    let dir = case_dir("unannounced");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = helper_socket(&dir);
    let mut helper = spawn(
        Command::new(env!("CARGO_BIN_EXE_lumen-crash"))
            .args([
                "--socket",
                &socket,
                "--pid",
                &std::process::id().to_string(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped()),
    );
    let connect = || {
        let start = Instant::now();
        loop {
            match minidumper::Client::with_name(socket_name(&socket)) {
                Ok(client) => return client,
                Err(e) if start.elapsed() > Duration::from_secs(10) => {
                    panic!("no helper to connect to: {e}")
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    };
    drop(connect());
    // The helper's loop wakes every 10 ms; give it many turns to see the
    // closed connection.
    std::thread::sleep(Duration::from_millis(500));
    let ended = helper.try_wait().unwrap();
    if let Some(status) = ended {
        let mut stderr = String::new();
        let _ = helper.stderr.take().unwrap().read_to_string(&mut stderr);
        panic!("the helper ended with {status}: {stderr}");
    }
    let client = connect();
    assert!(client.ping().is_ok(), "the helper does not answer");
    let _ = helper.kill();
    let _ = helper.wait();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A socket name for a helper the test starts by hand.
fn helper_socket(dir: &Path) -> String {
    if cfg!(any(target_os = "linux", target_os = "android")) {
        format!("lumen-crash-e2e-{}", std::process::id())
    } else if cfg!(windows) {
        dir.join("helper.sock").display().to_string()
    } else {
        // `sun_path` holds 104 bytes on macOS, and the runner's temp
        // directory is long.
        format!("/tmp/lumen-crash-e2e-{}.sock", std::process::id())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn socket_name(name: &str) -> minidumper::SocketName<'_> {
    minidumper::SocketName::abstract_namespace(name)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn socket_name(name: &str) -> minidumper::SocketName<'_> {
    minidumper::SocketName::Path(Path::new(name))
}

/// The runtime's preflight puts reports in the app's data folder and names
/// the app by its window title.
#[cfg(target_os = "linux")]
#[test]
fn the_capability_reports_into_the_apps_data_folder() {
    let dir = crash("capability:[window]\ntitle = \"Demo\"\n");
    let crashes = dir.join("data/lumen/demo-app/crashes");
    let names = wait_for_report(&crashes, Duration::from_secs(5));
    let text = report(&crashes, &names);
    assert!(text.contains("Demo stopped because of a panic."), "{text}");
    assert!(text.contains("App:        Demo (demo-app)"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `[crash] enabled = false` leaves crash handling out.
#[cfg(target_os = "linux")]
#[test]
fn the_capability_stays_out_when_turned_off() {
    let dir = crash("capability:[crash]\nenabled = false\n");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!dir.join("data").exists(), "{:?}", files(&dir.join("data")));
    std::fs::remove_dir_all(&dir).unwrap();
}
