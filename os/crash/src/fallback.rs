//! Reports the app writes itself, when no helper is there to write them.
//!
//! A panic's report is written in full from the panic hook and kept as
//! pending: an app that ends normally deletes the reports of the panics it
//! survived ([`discard_pending`]).
//!
//! A native fault's report is written from inside the fault handler, where
//! the process may be too broken to allocate or take a lock. Everything that
//! can be prepared is prepared at install time (the folder path, the report's
//! header); the handler only formats numbers into a stack buffer and makes
//! system calls. It carries the fault, the address and the instruction, and
//! no log lines.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use lumen_crash_report::protocol::PanicMessage;
use lumen_crash_report::{Report, protocol::Hello};

use crate::Config;

/// Reports of panics the app may yet survive.
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// What the fault handler needs, ready before any fault.
struct Prepared {
    /// The folders to create, outermost first, each NUL-terminated.
    dirs: Vec<Native>,
    /// The crash folder with a trailing separator, not terminated.
    prefix: Native,
    /// The report up to the fault's own lines.
    header: Vec<u8>,
}

#[cfg(unix)]
type Native = Vec<u8>;
#[cfg(windows)]
type Native = Vec<u16>;

static PREPARED: OnceLock<Prepared> = OnceLock::new();

/// Prepare the fault handler's report for the app `config` describes.
pub(crate) fn prepare(config: &Config) {
    let dir = &config.crash_dir;
    let mut dirs: Vec<Native> = dir.ancestors().take(3).map(terminated).collect();
    dirs.reverse();
    let mut prefix = native(dir);
    #[cfg(unix)]
    prefix.push(b'/');
    #[cfg(windows)]
    prefix.push(u16::from(b'\\'));
    let mut report = Report::new(hello(config), SystemTime::now());
    report.os = lumen_crash_report::os::description();
    let name = if config.app_name.is_empty() {
        "The app".to_string()
    } else {
        config.app_name.clone()
    };
    let mut header = format!("Lumen crash report\n\n{name} stopped because of a native fault.\n\n");
    // The fields the full report opens with, minus the time, which the
    // handler writes itself.
    for line in report.render().lines().skip(4) {
        if line.is_empty() {
            break;
        }
        if !line.starts_with("Time:") && !line.starts_with("GPU:") {
            header.push_str(line);
            header.push('\n');
        }
    }
    let _ = PREPARED.set(Prepared {
        dirs,
        prefix,
        header: header.into_bytes(),
    });
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
        dialog: false,
    }
}

/// Write the report of `panic`, pending until the app ends. Only the first
/// panic since the last normal end gets one: a later panic is most often
/// the first one's echo.
pub(crate) fn write_panic(panic: &PanicMessage) {
    let Some(config) = crate::config() else {
        return;
    };
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if !pending.is_empty() {
        return;
    }
    let time = SystemTime::now();
    let mut report = Report::new(hello(config), time);
    report.os = lumen_crash_report::os::description();
    report.gpu = crate::logs::gpu();
    report.panic = Some(panic.clone());
    report.logs = crate::logs::lines();
    let stem = lumen_crash_report::free_stem(&config.crash_dir, time);
    if let Ok(path) = report.write(&config.crash_dir, &stem) {
        pending.push(path);
    }
}

/// The app ended normally: delete the reports of the panics it survived.
pub(crate) fn discard_pending() {
    for path in PENDING.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(unix)]
fn native(path: &Path) -> Native {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn native(path: &Path) -> Native {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().collect()
}

fn terminated(path: &Path) -> Native {
    let mut n = native(path);
    n.push(0);
    n
}

/// A fixed buffer the handler formats into without allocating.
struct Buf<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Buf<N> {
    const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    fn push(&mut self, s: &[u8]) {
        let n = s.len().min(N - self.len);
        self.bytes[self.len..self.len + n].copy_from_slice(&s[..n]);
        self.len += n;
    }

    fn decimal(&mut self, mut v: u64, width: usize) {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        for _ in (digits.len() - i)..width {
            self.push(b"0");
        }
        let tail = digits;
        self.push(&tail[i..]);
    }

    fn hex(&mut self, v: u64) {
        self.push(b"0x");
        for shift in (0..16).rev() {
            let nibble = ((v >> (shift * 4)) & 0xf) as u8;
            self.push(&[if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            }]);
        }
    }

    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// The calendar stamp of `secs` since 1970 into `buf`, as the report file
/// names use it, without allocating.
fn stamp<const N: usize>(buf: &mut Buf<N>, secs: u64, file: bool) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days, as in lumen-crash-report.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u64;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as u64;
    let (date_sep, mid, time_sep) = if file {
        (b"-", b"_", b"-")
    } else {
        (b"-", b" ", b":")
    };
    buf.decimal(y, 4);
    buf.push(date_sep);
    buf.decimal(m, 2);
    buf.push(date_sep);
    buf.decimal(d, 2);
    buf.push(mid);
    buf.decimal(rem / 3600, 2);
    buf.push(time_sep);
    buf.decimal(rem % 3600 / 60, 2);
    buf.push(time_sep);
    buf.decimal(rem % 60, 2);
}

/// What the fault was, in the report's words, read from the crash context.
struct FaultLines {
    what: Buf<128>,
    address: Option<u64>,
    instruction: Option<u64>,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn describe(context: &crash_handler::CrashContext) -> FaultLines {
    let signo = context.siginfo.ssi_signo as i32;
    let mut what = Buf::new();
    let name: &[u8] = match signo {
        libc::SIGSEGV => b"SIGSEGV (segmentation fault)",
        libc::SIGBUS => b"SIGBUS (bus error)",
        libc::SIGABRT => b"SIGABRT (abort)",
        libc::SIGILL => b"SIGILL (illegal instruction)",
        libc::SIGFPE => b"SIGFPE (arithmetic error)",
        libc::SIGTRAP => b"SIGTRAP (trap)",
        _ => b"signal",
    };
    what.push(name);
    what.push(b", signal ");
    what.decimal(signo as u64, 1);
    #[cfg(target_arch = "x86_64")]
    let instruction = Some(context.context.uc_mcontext.gregs[16] as u64);
    #[cfg(target_arch = "aarch64")]
    let instruction = Some(context.context.uc_mcontext.pc);
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let instruction = None;
    FaultLines {
        what,
        address: Some(context.siginfo.ssi_addr),
        instruction,
    }
}

#[cfg(target_os = "macos")]
fn describe(context: &crash_handler::CrashContext) -> FaultLines {
    let mut what = Buf::new();
    let (kind, code, subcode) = context
        .exception
        .map(|e| (e.kind, e.code, e.subcode))
        .unwrap_or((0, 0, None));
    let name: &[u8] = match kind {
        1 => b"EXC_BAD_ACCESS (bad memory access)",
        2 => b"EXC_BAD_INSTRUCTION (illegal instruction)",
        3 => b"EXC_ARITHMETIC (arithmetic error)",
        6 => b"EXC_BREAKPOINT (trap)",
        10 => b"EXC_CRASH (abort)",
        11 => b"EXC_RESOURCE (resource limit)",
        12 => b"EXC_GUARD (guarded resource)",
        _ => b"exception",
    };
    what.push(name);
    what.push(b", code ");
    what.hex(code);
    FaultLines {
        what,
        address: subcode,
        instruction: None,
    }
}

#[cfg(windows)]
fn describe(context: &crash_handler::CrashContext) -> FaultLines {
    let mut what = Buf::new();
    let code = context.exception_code as u32;
    let name: &[u8] = match code {
        0xC000_0005 => b"EXCEPTION_ACCESS_VIOLATION",
        0xC000_001D => b"EXCEPTION_ILLEGAL_INSTRUCTION",
        0xC000_0094 => b"EXCEPTION_INT_DIVIDE_BY_ZERO",
        0xC000_00FD => b"EXCEPTION_STACK_OVERFLOW",
        0xC000_0374 => b"STATUS_HEAP_CORRUPTION",
        0xC000_0409 => b"STATUS_STACK_BUFFER_OVERRUN",
        0x8000_0003 => b"EXCEPTION_BREAKPOINT",
        _ => b"exception",
    };
    what.push(name);
    what.push(b", code ");
    what.hex(u64::from(code));
    // SAFETY: the pointers come from the exception dispatcher (or the
    // simulated record) and stay valid while the handler runs.
    let (address, instruction) = unsafe {
        let pointers = context.exception_pointers;
        if pointers.is_null() || (*pointers).ExceptionRecord.is_null() {
            (None, None)
        } else {
            let record = &*(*pointers).ExceptionRecord;
            let instruction = record.ExceptionAddress as u64;
            let address = if code == 0xC000_0005 && record.NumberParameters >= 2 {
                record.ExceptionInformation[1] as u64
            } else {
                instruction
            };
            (Some(address), Some(instruction))
        }
    };
    FaultLines {
        what,
        address,
        instruction,
    }
}

/// Write the minimal report of the fault in `context`. Runs in the fault
/// handler: no allocation, no locks.
pub(crate) fn write_fault(context: &crash_handler::CrashContext) {
    let Some(prepared) = PREPARED.get() else {
        return;
    };
    let secs = now_secs();
    let lines = describe(context);
    let mut body: Buf<1024> = Buf::new();
    body.push(b"Time:       ");
    stamp(&mut body, secs, false);
    body.push(b" UTC\n\nNative fault\n------------\nFault:      ");
    body.push(lines.what.as_slice());
    body.push(b"\n");
    if let Some(address) = lines.address {
        body.push(b"Address:    ");
        body.hex(address);
        body.push(b"\n");
    }
    if let Some(ip) = lines.instruction {
        body.push(b"Code at:    ");
        body.hex(ip);
        body.push(b"\n");
    }
    body.push(
        b"\nThe crash helper was not running, so no minidump was written and the \
          log lines are not included.\n\nThis report stays on this computer; nothing \
          was sent anywhere.\n",
    );
    for dir in &prepared.dirs {
        sys::make_dir(dir);
    }
    for attempt in 1..10u64 {
        let mut name: Buf<64> = Buf::new();
        stamp(&mut name, secs, true);
        if attempt > 1 {
            name.push(b"-");
            name.decimal(attempt, 1);
        }
        name.push(b".txt");
        match sys::create(&prepared.prefix, name.as_slice()) {
            sys::Created::File(file) => {
                sys::write(&file, &prepared.header);
                sys::write(&file, body.as_slice());
                sys::close(file);
                return;
            }
            sys::Created::Exists => continue,
            sys::Created::Failed => return,
        }
    }
}

#[cfg(unix)]
fn now_secs() -> u64 {
    // SAFETY: clock_gettime is async-signal-safe and writes the struct.
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts);
        ts.tv_sec.max(0) as u64
    }
}

#[cfg(windows)]
fn now_secs() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    // SAFETY: fills one FILETIME.
    let ft = unsafe {
        let mut ft = std::mem::zeroed();
        GetSystemTimeAsFileTime(&mut ft);
        ft
    };
    let ticks = (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime);
    (ticks / 10_000_000).saturating_sub(11_644_473_600)
}

#[cfg(unix)]
mod sys {
    pub(super) enum Created {
        File(libc::c_int),
        Exists,
        Failed,
    }

    pub(super) fn make_dir(path: &[u8]) {
        // SAFETY: a NUL-terminated path; mkdir is async-signal-safe.
        unsafe {
            libc::mkdir(path.as_ptr().cast(), 0o755);
        }
    }

    pub(super) fn create(prefix: &[u8], name: &[u8]) -> Created {
        let mut path = [0u8; 4096];
        if prefix.len() + name.len() + 1 > path.len() {
            return Created::Failed;
        }
        path[..prefix.len()].copy_from_slice(prefix);
        path[prefix.len()..prefix.len() + name.len()].copy_from_slice(name);
        // SAFETY: `path` is NUL-terminated (zero-filled past the name); open
        // is async-signal-safe.
        let fd = unsafe {
            libc::open(
                path.as_ptr().cast(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd >= 0 {
            Created::File(fd)
        } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
            Created::Exists
        } else {
            Created::Failed
        }
    }

    pub(super) fn write(fd: &libc::c_int, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            // SAFETY: writes from a live slice; async-signal-safe.
            let n = unsafe { libc::write(*fd, bytes.as_ptr().cast(), bytes.len()) };
            if n <= 0 {
                return;
            }
            bytes = &bytes[n as usize..];
        }
    }

    pub(super) fn close(fd: libc::c_int) {
        // SAFETY: closes the descriptor `create` opened.
        unsafe {
            libc::close(fd);
        }
    }
}

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_FILE_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, WriteFile,
    };

    const GENERIC_WRITE: u32 = 0x4000_0000;

    pub(super) enum Created {
        File(HANDLE),
        Exists,
        Failed,
    }

    pub(super) fn make_dir(path: &[u16]) {
        // SAFETY: a NUL-terminated wide path.
        unsafe {
            CreateDirectoryW(path.as_ptr(), std::ptr::null());
        }
    }

    pub(super) fn create(prefix: &[u16], name: &[u8]) -> Created {
        let mut path = [0u16; 2048];
        if prefix.len() + name.len() + 1 > path.len() {
            return Created::Failed;
        }
        path[..prefix.len()].copy_from_slice(prefix);
        for (slot, b) in path[prefix.len()..].iter_mut().zip(name) {
            *slot = u16::from(*b);
        }
        // SAFETY: `path` is NUL-terminated (zero-filled past the name).
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            Created::File(handle)
        // SAFETY: reads the calling thread's last error.
        } else if unsafe { GetLastError() } == ERROR_FILE_EXISTS {
            Created::Exists
        } else {
            Created::Failed
        }
    }

    pub(super) fn write(handle: &HANDLE, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let mut written = 0u32;
            // SAFETY: writes from a live slice to the handle `create` opened.
            let ok = unsafe {
                WriteFile(
                    *handle,
                    bytes.as_ptr(),
                    bytes.len().min(u32::MAX as usize) as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || written == 0 {
                return;
            }
            bytes = &bytes[written as usize..];
        }
    }

    pub(super) fn close(handle: HANDLE) {
        // SAFETY: closes the handle `create` opened.
        unsafe {
            CloseHandle(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handler_formats_numbers_and_times_like_the_report() {
        let mut buf: Buf<64> = Buf::new();
        buf.hex(0x1f);
        assert_eq!(buf.as_slice(), b"0x000000000000001f");
        let mut buf: Buf<64> = Buf::new();
        buf.decimal(7, 2);
        buf.decimal(1234, 1);
        assert_eq!(buf.as_slice(), b"071234");
        for secs in [0u64, 951_782_400, 1_760_796_202, 1_798_761_599] {
            let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            let mut file: Buf<64> = Buf::new();
            stamp(&mut file, secs, true);
            assert_eq!(
                file.as_slice(),
                lumen_crash_report::time::file_stamp(at).as_bytes()
            );
            let mut shown: Buf<64> = Buf::new();
            stamp(&mut shown, secs, false);
            let expected = lumen_crash_report::time::display(at);
            assert_eq!(
                shown.as_slice(),
                expected.trim_end_matches(" UTC").as_bytes()
            );
        }
    }

    #[test]
    fn a_full_buffer_drops_the_rest_instead_of_overrunning() {
        let mut buf: Buf<4> = Buf::new();
        buf.push(b"abcdef");
        assert_eq!(buf.as_slice(), b"abcd");
    }
}
