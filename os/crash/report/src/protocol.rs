//! The messages an app sends its helper.
//!
//! Each message is one `minidumper` user message: a kind and a byte payload.
//! A payload that carries several values is a list of `key`, `value` pairs,
//! each one terminated by a NUL byte; text is UTF-8 and a NUL inside a value
//! is dropped on the way in. A crash dump is not a message of its own; the
//! app asks for one through `minidumper`'s request, and the helper pairs it
//! with what it was told.

use std::path::PathBuf;

/// Who the app is and where its reports go. The first message on a
/// connection.
pub const HELLO: u32 = 1;
/// The renderer and the device it draws on, one line. Sent again when it
/// changes.
pub const GPU: u32 = 2;
/// One log line.
pub const LOG: u32 = 3;
/// A panic: [`Message::Panic`]. Sent from the panic hook, before the app
/// asks for a dump of the panicking moment.
pub const PANIC: u32 = 4;
/// The app is ending normally. Whatever panic it reported was survived.
pub const EXIT: u32 = 5;

/// What the app tells the helper first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hello {
    /// The app's name, for the dialog and the report.
    pub app_name: String,
    /// The app's stable id, `[app] id` or the one its directory gives.
    pub app_id: String,
    /// The running executable.
    pub exe: String,
    /// The Lumen version the app runs on.
    pub lumen_version: String,
    /// The app's process id.
    pub pid: u32,
    /// The folder the reports go in.
    pub crash_dir: PathBuf,
    /// Whether to show the dialog after a crash.
    pub dialog: bool,
}

/// A panic as the hook saw it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PanicMessage {
    /// The panic's message.
    pub message: String,
    /// `file:line:column`, when the panic carries one.
    pub location: String,
    /// The panicking thread's name, or its id when it has none.
    pub thread: String,
    /// The backtrace, symbolized when the binary carries symbols.
    pub backtrace: String,
}

/// One message, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// [`HELLO`].
    Hello(Hello),
    /// [`GPU`].
    Gpu(String),
    /// [`LOG`].
    Log(String),
    /// [`PANIC`]. The flag says whether a dump request for this panic
    /// follows.
    Panic(PanicMessage, bool),
    /// [`EXIT`].
    Exit,
}

impl Message {
    /// The kind and payload to send.
    pub fn encode(&self) -> (u32, Vec<u8>) {
        match self {
            Self::Hello(h) => (
                HELLO,
                pairs(&[
                    ("app_name", &h.app_name),
                    ("app_id", &h.app_id),
                    ("exe", &h.exe),
                    ("lumen_version", &h.lumen_version),
                    ("pid", &h.pid.to_string()),
                    ("crash_dir", &h.crash_dir.to_string_lossy()),
                    ("dialog", if h.dialog { "1" } else { "0" }),
                ]),
            ),
            Self::Gpu(line) => (GPU, line.as_bytes().to_vec()),
            Self::Log(line) => (LOG, line.as_bytes().to_vec()),
            Self::Panic(p, dump) => (
                PANIC,
                pairs(&[
                    ("message", &p.message),
                    ("location", &p.location),
                    ("thread", &p.thread),
                    ("backtrace", &p.backtrace),
                    ("dump", if *dump { "1" } else { "0" }),
                ]),
            ),
            Self::Exit => (EXIT, Vec::new()),
        }
    }

    /// Read a message back. `None` for a kind this side does not know, so
    /// a newer app talking to an older helper loses only what is new.
    pub fn decode(kind: u32, payload: &[u8]) -> Option<Self> {
        let text = || String::from_utf8_lossy(payload).into_owned();
        Some(match kind {
            HELLO => {
                let f = Fields::parse(payload);
                Self::Hello(Hello {
                    app_name: f.get("app_name"),
                    app_id: f.get("app_id"),
                    exe: f.get("exe"),
                    lumen_version: f.get("lumen_version"),
                    pid: f.get("pid").parse().unwrap_or(0),
                    crash_dir: PathBuf::from(f.get("crash_dir")),
                    dialog: f.get("dialog") == "1",
                })
            }
            GPU => Self::Gpu(text()),
            LOG => Self::Log(text()),
            PANIC => {
                let f = Fields::parse(payload);
                Self::Panic(
                    PanicMessage {
                        message: f.get("message"),
                        location: f.get("location"),
                        thread: f.get("thread"),
                        backtrace: f.get("backtrace"),
                    },
                    f.get("dump") == "1",
                )
            }
            EXIT => Self::Exit,
            _ => return None,
        })
    }
}

/// Encode `key`, `value` pairs.
fn pairs(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (key, value) in fields {
        for part in [key, value] {
            out.extend(part.bytes().filter(|b| *b != 0));
            out.push(0);
        }
    }
    out
}

/// Decoded `key`, `value` pairs.
struct Fields(Vec<(String, String)>);

impl Fields {
    fn parse(payload: &[u8]) -> Self {
        let mut parts = payload
            .split(|b| *b == 0)
            .map(|p| String::from_utf8_lossy(p).into_owned());
        let mut fields = Vec::new();
        while let (Some(key), Some(value)) = (parts.next(), parts.next()) {
            fields.push((key, value));
        }
        Self(fields)
    }

    /// The value under `key`, empty when absent.
    fn get(&self, key: &str) -> String {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: Message) -> Message {
        let (kind, payload) = message.encode();
        Message::decode(kind, &payload).expect("a known kind")
    }

    #[test]
    fn every_message_reads_back_as_it_was_sent() {
        let hello = Message::Hello(Hello {
            app_name: "pixl".into(),
            app_id: "pixl".into(),
            exe: "C:\\Apps\\pixl\\pixl.exe".into(),
            lumen_version: "0.0.11".into(),
            pid: 4242,
            crash_dir: PathBuf::from("/home/u/.local/share/lumen/pixl/crashes"),
            dialog: true,
        });
        assert_eq!(round_trip(hello.clone()), hello);
        let panic = Message::Panic(
            PanicMessage {
                message: "index out of bounds: the len is 3 but the index is 7".into(),
                location: "src/main.rs:10:5".into(),
                thread: "main".into(),
                backtrace: "   0: pixl::main\n   1: core::ops::function::FnOnce::call_once\n"
                    .into(),
            },
            true,
        );
        assert_eq!(round_trip(panic.clone()), panic);
        assert_eq!(
            round_trip(Message::Gpu("gpu: Radeon".into())),
            Message::Gpu("gpu: Radeon".into())
        );
        assert_eq!(
            round_trip(Message::Log("a line".into())),
            Message::Log("a line".into())
        );
        assert_eq!(round_trip(Message::Exit), Message::Exit);
    }

    #[test]
    fn a_nul_in_a_value_cannot_shift_the_fields_after_it() {
        let (kind, payload) = Message::Panic(
            PanicMessage {
                message: "bad\0value".into(),
                location: "here".into(),
                ..PanicMessage::default()
            },
            false,
        )
        .encode();
        let Some(Message::Panic(p, false)) = Message::decode(kind, &payload) else {
            panic!("not a panic");
        };
        assert_eq!(p.message, "badvalue");
        assert_eq!(p.location, "here");
    }

    #[test]
    fn an_unknown_kind_is_skipped() {
        assert_eq!(Message::decode(99, b"anything"), None);
    }
}
