//! The message box that tells the user their app crashed.
//!
//! Each platform's own: `MessageBoxW` on Windows, a `CFUserNotification`
//! alert on macOS, a GTK message dialog on Linux (GTK is what Lumen's file
//! dialogs already run on there), with `zenity` and `kdialog` behind it. The
//! GTK library is opened only when there is a dialog to show, so the helper
//! does not carry it while it waits.
//!
//! The box names the report and offers to open the folder it is in.

use std::path::Path;

/// The dialog's title.
fn title(app: &str) -> String {
    if app.is_empty() {
        "Crash report".to_string()
    } else {
        format!("{app} crashed")
    }
}

/// The dialog's text.
fn text(app: &str, report: &Path) -> String {
    let app = if app.is_empty() { "The app" } else { app };
    format!(
        "{app} stopped unexpectedly.\n\nA crash report was saved to:\n{}\n\nNothing was sent anywhere.",
        report.display()
    )
}

/// The label of the button that opens the reports' folder. Windows' message
/// box cannot relabel its buttons and asks a Yes/No question instead.
#[cfg(not(windows))]
const OPEN: &str = "Open folder";
/// The label of the button that dismisses the dialog.
#[cfg(not(windows))]
const CLOSE: &str = "Close";

/// Show the dialog for the report at `report`; open its folder when asked.
pub fn show(app: &str, report: &Path) {
    if imp::ask(&title(app), &text(app, report))
        && let Some(dir) = report.parent()
    {
        let _ = opener::open(dir);
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONERROR, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MessageBoxW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A Yes/No box: the question is spelled out in the text, since the
    /// standard box cannot relabel its buttons.
    pub fn ask(title: &str, text: &str) -> bool {
        let text = format!("{text}\n\nOpen the folder that holds the report?");
        let (title, text) = (wide(title), wide(&text));
        // SAFETY: both strings are NUL-terminated UTF-16 that outlive the
        // call; no owner window.
        let answer = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
            )
        };
        answer == IDYES
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;

    type CFStringRef = *const c_void;
    type CFOptionFlags = usize;

    const UTF8: u32 = 0x0800_0100;
    const STOP_ALERT: CFOptionFlags = 0;
    const ALTERNATE_RESPONSE: CFOptionFlags = 1;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
        fn CFUserNotificationDisplayAlert(
            timeout: f64,
            flags: CFOptionFlags,
            icon: *const c_void,
            sound: *const c_void,
            localization: *const c_void,
            header: CFStringRef,
            message: CFStringRef,
            default_button: CFStringRef,
            alternate_button: CFStringRef,
            other_button: CFStringRef,
            response: *mut CFOptionFlags,
        ) -> i32;
    }

    fn cf(s: &str) -> CFStringRef {
        // SAFETY: the bytes are valid UTF-8 for the length given.
        unsafe { CFStringCreateWithBytes(std::ptr::null(), s.as_ptr(), s.len() as isize, UTF8, 0) }
    }

    pub fn ask(title: &str, text: &str) -> bool {
        let strings = [cf(title), cf(text), cf(super::CLOSE), cf(super::OPEN)];
        if strings.iter().any(|s| s.is_null()) {
            return false;
        }
        let mut response: CFOptionFlags = 0;
        // SAFETY: every string is a live CFString; the call blocks until the
        // user answers and writes the answer to `response`.
        let status = unsafe {
            CFUserNotificationDisplayAlert(
                0.0,
                STOP_ALERT,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                strings[0],
                strings[1],
                strings[2],
                strings[3],
                std::ptr::null(),
                &mut response,
            )
        };
        for s in strings {
            // SAFETY: each was created above and is released once.
            unsafe { CFRelease(s) };
        }
        status == 0 && response & 3 == ALTERNATE_RESPONSE
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::process::{Command, Stdio};

    pub fn ask(title: &str, text: &str) -> bool {
        if let Some(answer) = gtk(title, text) {
            return answer;
        }
        if let Some(answer) = run(Command::new("zenity").args([
            "--question",
            "--icon-name=dialog-error",
            &format!("--title={title}"),
            &format!("--text={text}"),
            &format!("--ok-label={}", super::OPEN),
            &format!("--cancel-label={}", super::CLOSE),
        ])) {
            return answer;
        }
        run(Command::new("kdialog").args([
            "--title",
            title,
            "--yes-label",
            super::OPEN,
            "--no-label",
            super::CLOSE,
            "--warningyesno",
            text,
        ]))
        .unwrap_or(false)
    }

    /// Whether the dialog program said yes; `None` when it could not run.
    fn run(command: &mut Command) -> Option<bool> {
        let status = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        match status.code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        }
    }

    type GtkInitCheck = unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> c_int;
    type GtkMessageDialogNew =
        unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, *const c_char, ...) -> *mut c_void;
    type GtkDialogAddButton =
        unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> *mut c_void;
    type GtkWindowSetTitle = unsafe extern "C" fn(*mut c_void, *const c_char);
    type GtkDialogRun = unsafe extern "C" fn(*mut c_void) -> c_int;
    type GtkWidgetDestroy = unsafe extern "C" fn(*mut c_void);
    type GtkEventsPending = unsafe extern "C" fn() -> c_int;
    type GtkMainIteration = unsafe extern "C" fn() -> c_int;

    const GTK_MESSAGE_ERROR: c_int = 3;
    const GTK_BUTTONS_NONE: c_int = 0;
    const OPEN_RESPONSE: c_int = 1;
    const CLOSE_RESPONSE: c_int = 2;

    /// The answer from a GTK 3 message dialog, `None` when GTK is not there
    /// or has no display to open.
    fn gtk(title: &str, text: &str) -> Option<bool> {
        // SAFETY: the library is opened by its soname and each symbol is
        // looked up by its documented name and called through the C
        // signature GTK 3 declares for it. Every string handed over is a
        // NUL-terminated CString that outlives the call.
        unsafe {
            let lib = libc::dlopen(c"libgtk-3.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if lib.is_null() {
                return None;
            }
            let sym = |name: &std::ffi::CStr| {
                let p = libc::dlsym(lib, name.as_ptr());
                (!p.is_null()).then_some(p)
            };
            let init: GtkInitCheck = std::mem::transmute(sym(c"gtk_init_check")?);
            let new: GtkMessageDialogNew = std::mem::transmute(sym(c"gtk_message_dialog_new")?);
            let add: GtkDialogAddButton = std::mem::transmute(sym(c"gtk_dialog_add_button")?);
            let set_title: GtkWindowSetTitle = std::mem::transmute(sym(c"gtk_window_set_title")?);
            let run: GtkDialogRun = std::mem::transmute(sym(c"gtk_dialog_run")?);
            let destroy: GtkWidgetDestroy = std::mem::transmute(sym(c"gtk_widget_destroy")?);
            let pending: GtkEventsPending = std::mem::transmute(sym(c"gtk_events_pending")?);
            let iterate: GtkMainIteration = std::mem::transmute(sym(c"gtk_main_iteration")?);
            if init(std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                return None;
            }
            let text = CString::new(text.replace('\0', "")).ok()?;
            let title = CString::new(title.replace('\0', "")).ok()?;
            let (open, close) = (
                CString::new(super::OPEN).ok()?,
                CString::new(super::CLOSE).ok()?,
            );
            let dialog = new(
                std::ptr::null_mut(),
                0,
                GTK_MESSAGE_ERROR,
                GTK_BUTTONS_NONE,
                c"%s".as_ptr(),
                text.as_ptr(),
            );
            if dialog.is_null() {
                return None;
            }
            set_title(dialog, title.as_ptr());
            add(dialog, close.as_ptr(), CLOSE_RESPONSE);
            add(dialog, open.as_ptr(), OPEN_RESPONSE);
            let answer = run(dialog);
            destroy(dialog);
            while pending() != 0 {
                iterate();
            }
            Some(answer == OPEN_RESPONSE)
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    pub fn ask(_title: &str, _text: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_names_the_app_and_the_report() {
        let t = text("pixl", Path::new("/data/crashes/x.txt"));
        assert!(t.starts_with("pixl stopped unexpectedly."));
        assert!(t.contains("/data/crashes/x.txt"));
        assert_eq!(title("pixl"), "pixl crashed");
        assert!(text("", Path::new("r")).starts_with("The app stopped"));
    }
}
