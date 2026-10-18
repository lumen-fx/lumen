//! The readable crash report: what it holds and how it reads.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::protocol::{Hello, PanicMessage};

/// A panic in a report.
pub type Panic = PanicMessage;

/// A native fault: a signal on Linux and macOS, a structured exception on
/// Windows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fault {
    /// What happened, in words: `SIGSEGV (segmentation fault)`,
    /// `EXCEPTION_ACCESS_VIOLATION`.
    pub description: String,
    /// The address the fault names: the bad access, or the instruction.
    pub address: Option<u64>,
    /// The module the faulting instruction is in, with its offset there.
    pub module: Option<String>,
    /// The faulting thread.
    pub thread: Option<String>,
    /// The crashing thread's stack, innermost first, as far as it reads.
    pub stack: Vec<Frame>,
}

/// One address on a stack.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frame {
    /// The address.
    pub address: u64,
    /// The module it falls in and the offset into it, when one claims it.
    pub module: Option<(String, u64)>,
}

/// A crash report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// When the crash happened.
    pub time: SystemTime,
    /// Who crashed.
    pub app: Hello,
    /// The operating system, in words.
    pub os: String,
    /// The renderer and its device, once the app had drawn a frame.
    pub gpu: Option<String>,
    /// The panic that ended the app, when one did: the first one it did not
    /// survive.
    pub panic: Option<Panic>,
    /// One line for each panic after that one, oldest first.
    pub later_panics: Vec<String>,
    /// The native fault that ended the app, when one did. A panic that
    /// aborts ends in one too, so a report can carry both.
    pub fault: Option<Fault>,
    /// The minidump's file name, beside this report, when one was written.
    pub minidump: Option<String>,
    /// The last log lines, oldest first.
    pub logs: Vec<String>,
}

impl Report {
    /// An empty report for `app` at `time`.
    pub fn new(app: Hello, time: SystemTime) -> Self {
        Self {
            time,
            app,
            os: String::new(),
            gpu: None,
            panic: None,
            later_panics: Vec::new(),
            fault: None,
            minidump: None,
            logs: Vec::new(),
        }
    }

    /// The one line that says what happened.
    pub fn headline(&self) -> String {
        let name = if self.app.app_name.is_empty() {
            "The app"
        } else {
            &self.app.app_name
        };
        match (&self.panic, &self.fault) {
            (Some(_), _) => format!("{name} stopped because of a panic."),
            (None, Some(fault)) => {
                format!(
                    "{name} stopped because of a native fault: {}.",
                    fault.description
                )
            }
            (None, None) => format!("{name} stopped unexpectedly."),
        }
    }

    /// The report as its `.txt` file reads.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "Lumen crash report");
        let _ = writeln!(out);
        let _ = writeln!(out, "{}", self.headline());
        let _ = writeln!(out);
        let field = |out: &mut String, key: &str, value: &str| {
            if !value.is_empty() {
                let _ = writeln!(out, "{key:<12}{value}");
            }
        };
        field(&mut out, "Time:", &crate::time::display(self.time));
        let app = match (self.app.app_name.as_str(), self.app.app_id.as_str()) {
            ("", id) => id.to_string(),
            (name, id) if name == id || id.is_empty() => name.to_string(),
            (name, id) => format!("{name} ({id})"),
        };
        field(&mut out, "App:", &app);
        field(&mut out, "Executable:", &self.app.exe);
        if self.app.pid != 0 {
            field(&mut out, "Process:", &self.app.pid.to_string());
        }
        field(&mut out, "Lumen:", &self.app.lumen_version);
        field(&mut out, "OS:", &self.os);
        field(
            &mut out,
            "GPU:",
            self.gpu
                .as_deref()
                .unwrap_or("not known (no frame was drawn)"),
        );
        if let Some(dump) = &self.minidump {
            field(&mut out, "Minidump:", dump);
        }

        if let Some(panic) = &self.panic {
            section(&mut out, "Panic");
            let _ = writeln!(out, "{}", panic.message);
            let _ = writeln!(out);
            field(&mut out, "Location:", &panic.location);
            field(&mut out, "Thread:", &panic.thread);
            if !panic.backtrace.trim().is_empty() {
                section(&mut out, "Backtrace");
                let _ = writeln!(out, "{}", panic.backtrace.trim_end());
            }
        }
        if !self.later_panics.is_empty() {
            section(&mut out, "Later panics");
            for line in &self.later_panics {
                let _ = writeln!(out, "{line}");
            }
        }

        if let Some(fault) = &self.fault {
            section(&mut out, "Native fault");
            field(&mut out, "Fault:", &fault.description);
            if let Some(address) = fault.address {
                field(&mut out, "Address:", &format!("{address:#018x}"));
            }
            field(&mut out, "Module:", fault.module.as_deref().unwrap_or(""));
            field(&mut out, "Thread:", fault.thread.as_deref().unwrap_or(""));
            if !fault.stack.is_empty() {
                section(&mut out, "Stack");
                for (i, frame) in fault.stack.iter().enumerate() {
                    let _ = match &frame.module {
                        Some((module, offset)) => writeln!(
                            out,
                            "{i:>4}: {:#018x}  {module} + {offset:#x}",
                            frame.address
                        ),
                        None => writeln!(out, "{i:>4}: {:#018x}", frame.address),
                    };
                }
            }
        }

        section(&mut out, "Last log lines");
        if self.logs.is_empty() {
            let _ = writeln!(out, "(none)");
        }
        for line in &self.logs {
            let _ = writeln!(out, "{line}");
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "This report stays on this computer; nothing was sent anywhere."
        );
        out
    }

    /// Write the report into `dir` as `<stem>.txt`, creating `dir` when
    /// missing. Returns the file's path.
    pub fn write(&self, dir: &Path, stem: &str) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{stem}.txt"));
        std::fs::write(&path, self.render())?;
        Ok(path)
    }
}

fn section(out: &mut String, title: &str) {
    let _ = writeln!(out);
    let _ = writeln!(out, "{title}");
    let _ = writeln!(out, "{}", "-".repeat(title.len()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn app() -> Hello {
        Hello {
            app_name: "pixl".into(),
            app_id: "pixl".into(),
            exe: "/opt/pixl/pixl".into(),
            lumen_version: "0.0.11".into(),
            pid: 7,
            crash_dir: PathBuf::from("/tmp/crashes"),
            dialog: false,
        }
    }

    #[test]
    fn a_panic_report_names_the_panic_and_its_context() {
        let mut report = Report::new(app(), UNIX_EPOCH + Duration::from_secs(1_760_796_202));
        report.os = "Linux 6.1".into();
        report.gpu = Some("gpu: Radeon RX 7600 (Vulkan)".into());
        report.panic = Some(Panic {
            message: "boom".into(),
            location: "src/main.rs:3:5".into(),
            thread: "main".into(),
            backtrace: "   0: pixl::main\n".into(),
        });
        report.logs = vec!["lumen: one".into(), "lumen: two".into()];
        report.later_panics = vec!["echo (at src/main.rs:9:1, thread main)".into()];
        report.minidump = Some("2025-10-18_14-03-22.dmp".into());
        let text = report.render();
        assert!(text.starts_with("Lumen crash report\n\npixl stopped because of a panic.\n"));
        for needle in [
            "Time:       2025-10-18 14:03:22 UTC",
            "App:        pixl",
            "Lumen:      0.0.11",
            "OS:         Linux 6.1",
            "GPU:        gpu: Radeon RX 7600 (Vulkan)",
            "Minidump:   2025-10-18_14-03-22.dmp",
            "boom",
            "Location:   src/main.rs:3:5",
            "   0: pixl::main",
            "Later panics\n------------\necho (at src/main.rs:9:1, thread main)",
            "lumen: one\nlumen: two\n",
            "nothing was sent anywhere",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }
    }

    #[test]
    fn a_fault_report_lists_the_fault_and_the_stack() {
        let mut report = Report::new(app(), UNIX_EPOCH);
        report.fault = Some(Fault {
            description: "SIGSEGV (segmentation fault)".into(),
            address: Some(0x10),
            module: Some("libvulkan_radeon.so + 0x1234".into()),
            thread: Some("4243".into()),
            stack: vec![
                Frame {
                    address: 0x7f00_0000_1234,
                    module: Some(("libvulkan_radeon.so".into(), 0x1234)),
                },
                Frame {
                    address: 0x5555,
                    module: None,
                },
            ],
        });
        let text = report.render();
        assert!(text.contains("pixl stopped because of a native fault: SIGSEGV"));
        assert!(text.contains("Address:    0x0000000000000010"));
        assert!(text.contains("Module:     libvulkan_radeon.so + 0x1234"));
        assert!(text.contains("   0: 0x00007f0000001234  libvulkan_radeon.so + 0x1234"));
        assert!(text.contains("   1: 0x0000000000005555\n"));
        assert!(text.contains("GPU:        not known (no frame was drawn)"));
        assert!(text.contains("(none)"));
    }

    #[test]
    fn writing_creates_the_folder() {
        let dir = std::env::temp_dir()
            .join(format!("lumen-crash-write-{}", std::process::id()))
            .join("crashes");
        let path = Report::new(app(), UNIX_EPOCH).write(&dir, "x").unwrap();
        assert_eq!(path, dir.join("x.txt"));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("pixl stopped unexpectedly.")
        );
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
