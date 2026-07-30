//! Screen-saver / sleep inhibit host for Lumen.
//!
//! Spec'd by the audit (section 394-412). v2 ships the per-platform backend calls behind a small RAII surface:
//!
//! - **Linux** - `org.freedesktop.ScreenSaver.Inhibit(app_name, reason) -> cookie` over the session bus via
//!   [`zbus`]. Best-effort: when no session bus / daemon is available the call logs at debug and the in-process
//!   token still works as a marker.
//! - **macOS** - `IOPMAssertionCreateWithName(kIOPMAssertionTypeNoIdleSleep, ...) -> IOPMAssertionID`
//!   paired with `IOPMAssertionRelease(id)`. Wired via `core-foundation` for CFString construction; the IOKit
//!   symbols are declared `extern "C"` (raw FFI mirrors what every macOS app does directly).
//! - **Windows** - `SetThreadExecutionState(ES_SYSTEM_REQUIRED | ES_CONTINUOUS)` to inhibit;
//!   `SetThreadExecutionState(ES_CONTINUOUS)` to release. Best-effort - the call can never fail in practice.
//!
//! Mirrors `gtk_application_inhibit` / `gtk_application_uninhibit`'s cookie-pair semantics. The
//! [`InhibitToken`] returned from [`PowerInhibitor::start`] holds the platform handle internally; dropping the
//! token releases the inhibit (RAII).

#![warn(missing_docs)]

use bevy_ecs::prelude::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

/// Bitset of inhibit kinds.
///
/// Mirrors `GtkApplicationInhibitFlags`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InhibitKinds(u32);

impl InhibitKinds {
    /// Prevent the display going to sleep.
    pub const DISPLAY: Self = Self(1 << 0);
    /// Prevent the system suspending.
    pub const SUSPEND: Self = Self(1 << 1);
    /// Prevent the user logging out.
    pub const LOGOUT: Self = Self(1 << 2);

    /// Empty bitset.
    pub fn empty() -> Self {
        Self(0)
    }

    /// Union of `self | other`.
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Test whether `flag` is set.
    pub fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }

    /// Raw bit representation.
    pub fn bits(self) -> u32 {
        self.0
    }
}

/// Per-platform handle held inside an [`InhibitToken`]. When the token drops the matching release call fires
/// (D-Bus `UnInhibit(cookie)` on Linux, `IOPMAssertionRelease(id)` on macOS, `SetThreadExecutionState(ES_CONTINUOUS)`
/// on Windows).
enum PlatformHandle {
    /// Nothing acquired (platform unsupported or backend call failed best-effort).
    None,
    #[cfg(target_os = "linux")]
    /// Live D-Bus connection + cookie from `org.freedesktop.ScreenSaver.Inhibit`.
    /// The connection MUST stay open for the inhibit's lifetime - the daemon
    /// tracks the inhibit against the owning bus name and auto-releases the
    /// moment that connection drops. `UnInhibit` fires on this same
    /// connection in `Drop`.
    Linux(linux::Inhibit),
    #[cfg(target_os = "macos")]
    /// `IOPMAssertionID` returned by `IOPMAssertionCreateWithName`.
    MacOs(u32),
    #[cfg(target_os = "windows")]
    /// Token marker - Windows uses thread-local state via `SetThreadExecutionState`. The presence of the
    /// `Windows` variant tells `Drop` to call `SetThreadExecutionState(ES_CONTINUOUS)` on release.
    Windows,
}

/// Opaque cookie identifying one live inhibit request. Constructed by
/// [`PowerInhibitor::start`] and released on `Drop`.
pub struct InhibitToken {
    id: u64,
    parent: Arc<Mutex<InhibitState>>,
    handle: PlatformHandle,
}

impl Drop for InhibitToken {
    fn drop(&mut self) {
        if let Ok(mut state) = self.parent.lock() {
            state.live.retain(|e| e.id != self.id);
        }
        // Release the platform handle. Failures are best-effort - log and move on.
        match std::mem::replace(&mut self.handle, PlatformHandle::None) {
            PlatformHandle::None => {}
            #[cfg(target_os = "linux")]
            PlatformHandle::Linux(inhibit) => {
                let cookie = inhibit.cookie();
                if let Err(e) = linux::release(&inhibit) {
                    tracing::debug!("lumen-os-power: UnInhibit({cookie}) failed: {e}");
                }
                // `inhibit` (and its Connection) drops here regardless.
            }
            #[cfg(target_os = "macos")]
            PlatformHandle::MacOs(id) => {
                macos::release(id);
            }
            #[cfg(target_os = "windows")]
            PlatformHandle::Windows => {
                windows_backend::release();
            }
        }
    }
}

impl std::fmt::Debug for InhibitToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InhibitToken")
            .field("id", &self.id)
            .finish()
    }
}

/// One recorded inhibit request (reason + flags).
#[derive(Clone, Debug)]
struct InhibitEntry {
    id: u64,
    reason: String,
    /// Stored so platform backends can route per-kind calls in a future revision.
    #[allow(dead_code)]
    kinds: InhibitKinds,
}

#[derive(Default)]
struct InhibitState {
    live: Vec<InhibitEntry>,
}

/// Screen-saver / sleep inhibit host.
///
/// Holds the in-process bookkeeping and dispatches per-platform acquire calls when [`Self::start`] runs.
/// Best-effort: when the platform call fails (no daemon, sandboxed off the bus, headless CI) the token still
/// works as a marker - it simply doesn't move the real system idle timer.
#[derive(Resource, Clone, Default)]
pub struct PowerInhibitor {
    next_id: Arc<AtomicU64>,
    state: Arc<Mutex<InhibitState>>,
    /// Configurable app name for the D-Bus `Inhibit(app_name, reason)` first arg. Defaults to `"lumen"`.
    app_name: Arc<String>,
}

impl PowerInhibitor {
    /// Empty inhibitor.
    pub fn new() -> Self {
        Self {
            next_id: Arc::new(AtomicU64::new(0)),
            state: Arc::new(Mutex::new(InhibitState::default())),
            app_name: Arc::new("lumen".to_string()),
        }
    }

    /// Configure the app name advertised to the screensaver daemon.
    pub fn with_app_name(mut self, name: impl Into<String>) -> Self {
        self.app_name = Arc::new(name.into());
        self
    }

    /// Start a new inhibit request. The returned token releases the inhibit when dropped (RAII, matching
    /// `gtk_application_inhibit` / `gtk_application_uninhibit`'s cookie pair).
    pub fn start(&self, reason: &str, kinds: InhibitKinds) -> InhibitToken {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut state) = self.state.lock() {
            state.live.push(InhibitEntry {
                id,
                reason: reason.to_string(),
                kinds,
            });
        }
        let handle = self.acquire(reason);
        InhibitToken {
            id,
            parent: Arc::clone(&self.state),
            handle,
        }
    }

    /// Count of currently-live inhibit requests.
    pub fn live_count(&self) -> usize {
        self.state.lock().map(|s| s.live.len()).unwrap_or(0)
    }

    /// Snapshot of currently-live reasons (in insertion order).
    pub fn reasons(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|s| s.live.iter().map(|e| e.reason.clone()).collect())
            .unwrap_or_default()
    }

    #[cfg(target_os = "linux")]
    fn acquire(&self, reason: &str) -> PlatformHandle {
        match linux::inhibit(&self.app_name, reason) {
            Ok(inhibit) => PlatformHandle::Linux(inhibit),
            Err(e) => {
                tracing::debug!("lumen-os-power: Inhibit failed (best-effort): {e}");
                PlatformHandle::None
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn acquire(&self, reason: &str) -> PlatformHandle {
        match macos::inhibit(reason) {
            Some(id) => PlatformHandle::MacOs(id),
            None => PlatformHandle::None,
        }
    }

    #[cfg(target_os = "windows")]
    fn acquire(&self, _reason: &str) -> PlatformHandle {
        if windows_backend::inhibit() {
            PlatformHandle::Windows
        } else {
            PlatformHandle::None
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn acquire(&self, _reason: &str) -> PlatformHandle {
        PlatformHandle::None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    //! D-Bus `org.freedesktop.ScreenSaver` backend. Blocking - the daemon RPC completes in microseconds and
    //! the inhibit request itself is not on the render hot path.

    use std::error::Error;

    const DEST: &str = "org.freedesktop.ScreenSaver";
    const PATH: &str = "/org/freedesktop/ScreenSaver";
    const IFACE: &str = "org.freedesktop.ScreenSaver";

    /// A live inhibit: the session-bus connection that owns the daemon-side
    /// inhibit, plus the returned cookie. The daemon releases the inhibit
    /// automatically if this connection drops, so it is held for the token's
    /// whole lifetime and `UnInhibit` is issued on this same connection.
    pub struct Inhibit {
        conn: zbus::blocking::Connection,
        cookie: u32,
    }

    impl Inhibit {
        /// Daemon cookie (for diagnostics).
        pub fn cookie(&self) -> u32 {
            self.cookie
        }
    }

    /// Try to acquire the inhibit. Keeps the connection alive inside the
    /// returned [`Inhibit`] so the daemon does not auto-release it.
    pub fn inhibit(app_name: &str, reason: &str) -> Result<Inhibit, Box<dyn Error>> {
        let conn = zbus::blocking::Connection::session()?;
        let proxy = zbus::blocking::Proxy::new(&conn, DEST, PATH, IFACE)?;
        let cookie: u32 = proxy.call("Inhibit", &(app_name, reason))?;
        Ok(Inhibit { conn, cookie })
    }

    /// Release the inhibit on the SAME connection that acquired it.
    pub fn release(inhibit: &Inhibit) -> Result<(), Box<dyn Error>> {
        let proxy = zbus::blocking::Proxy::new(&inhibit.conn, DEST, PATH, IFACE)?;
        let _: () = proxy.call("UnInhibit", &(inhibit.cookie,))?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    //! IOKit IOPMAssertion backend. Uses `core-foundation` for the CFString reason, then declares the IOKit
    //! symbols extern "C" directly - IOKit ships with the OS and the API surface here is small enough that a
    //! dedicated -sys crate would add more dep churn than it saves.

    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    /// `kIOPMAssertionTypeNoIdleSleep` - prevent system idle sleep. Matches GTK `INHIBIT_SUSPEND`.
    const ASSERTION_TYPE: &str = "NoIdleSleepAssertion";
    /// `kIOPMAssertionLevelOn`.
    const LEVEL_ON: u32 = 255;

    type IoReturn = i32;
    type IoPmAssertionId = u32;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            assertion_level: u32,
            assertion_name: CFStringRef,
            assertion_id: *mut IoPmAssertionId,
        ) -> IoReturn;
        fn IOPMAssertionRelease(assertion_id: IoPmAssertionId) -> IoReturn;
    }

    /// Acquire a NoIdleSleep assertion. Returns the kernel id; `None` when the IOKit call fails.
    pub fn inhibit(reason: &str) -> Option<u32> {
        let kind = CFString::new(ASSERTION_TYPE);
        let name = CFString::new(reason);
        let mut id: IoPmAssertionId = 0;
        let result = unsafe {
            IOPMAssertionCreateWithName(
                kind.as_concrete_TypeRef(),
                LEVEL_ON,
                name.as_concrete_TypeRef(),
                &mut id as *mut _,
            )
        };
        if result == 0 {
            Some(id)
        } else {
            tracing::debug!("lumen-os-power: IOPMAssertionCreateWithName returned {result}");
            None
        }
    }

    /// Release an assertion by id. Failure is best-effort (logged at debug).
    pub fn release(id: u32) {
        let result = unsafe { IOPMAssertionRelease(id) };
        if result != 0 {
            tracing::debug!("lumen-os-power: IOPMAssertionRelease({id}) returned {result}");
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_backend {
    //! `SetThreadExecutionState` backend.
    //!
    //! Two hazards the naive "flip flags on/off per token" approach hits:
    //!
    //! 1. **Thread affinity** - the execution-state flags set by
    //!    `SetThreadExecutionState` are scoped to the *calling thread* and
    //!    cleared when that thread exits. A token acquired on one thread and
    //!    released on another would neither clear the original inhibit nor
    //!    have any effect on release. We therefore serialise every call onto
    //!    one dedicated, long-lived worker thread.
    //! 2. **No refcount** - the flag is a boolean, not a count, so the first
    //!    token's release would clear the inhibit even while other tokens are
    //!    still live. The worker keeps a process-wide count and only clears
    //!    the state when it returns to zero.

    use std::sync::OnceLock;
    use std::sync::mpsc::{Sender, channel};
    use windows::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };

    enum Cmd {
        Inhibit,
        Release,
    }

    /// Lazily-spawned worker that owns all `SetThreadExecutionState` calls so
    /// they stay on a single thread and share one refcount.
    fn sender() -> &'static Sender<Cmd> {
        static TX: OnceLock<Sender<Cmd>> = OnceLock::new();
        TX.get_or_init(|| {
            let (tx, rx) = channel::<Cmd>();
            let spawned = std::thread::Builder::new()
                .name("lumen-os-power/exec-state".to_string())
                .spawn(move || {
                    let mut count: usize = 0;
                    while let Ok(cmd) = rx.recv() {
                        match cmd {
                            Cmd::Inhibit => {
                                count += 1;
                                if count == 1 {
                                    let _ = unsafe {
                                        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)
                                    };
                                }
                            }
                            Cmd::Release => {
                                count = count.saturating_sub(1);
                                if count == 0 {
                                    let _ = unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
                                }
                            }
                        }
                    }
                });
            if let Err(e) = &spawned {
                tracing::debug!("lumen-os-power: exec-state worker spawn failed: {e}");
            }
            tx
        })
    }

    /// Queue an inhibit on the worker. Returns true when the command was
    /// accepted (best-effort - the OS call itself cannot fail in practice).
    pub fn inhibit() -> bool {
        sender().send(Cmd::Inhibit).is_ok()
    }

    /// Queue a release on the worker; the state clears only at refcount zero.
    pub fn release() {
        let _ = sender().send(Cmd::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inhibitor_has_no_live() {
        let i = PowerInhibitor::new();
        assert_eq!(i.live_count(), 0);
    }

    #[test]
    fn start_then_drop_releases() {
        let i = PowerInhibitor::new();
        {
            let _t = i.start("playing video", InhibitKinds::DISPLAY);
            assert_eq!(i.live_count(), 1);
            assert_eq!(i.reasons(), vec!["playing video".to_string()]);
        }
        assert_eq!(i.live_count(), 0);
    }

    #[test]
    fn nested_inhibits_track_independently() {
        let i = PowerInhibitor::new();
        let a = i.start("export", InhibitKinds::SUSPEND);
        let _b = i.start("render", InhibitKinds::DISPLAY);
        assert_eq!(i.live_count(), 2);
        drop(a);
        assert_eq!(i.live_count(), 1);
    }

    #[test]
    fn kinds_union_contains() {
        let k = InhibitKinds::DISPLAY.union(InhibitKinds::SUSPEND);
        assert!(k.contains(InhibitKinds::DISPLAY));
        assert!(k.contains(InhibitKinds::SUSPEND));
        assert!(!k.contains(InhibitKinds::LOGOUT));
    }

    #[test]
    fn with_app_name_overrides_default() {
        let i = PowerInhibitor::new().with_app_name("my-app");
        // Internal field is private; smoke-test by exercising start (no panic, best-effort backend may no-op).
        let _t = i.start("x", InhibitKinds::DISPLAY);
        assert_eq!(i.live_count(), 1);
    }
}
