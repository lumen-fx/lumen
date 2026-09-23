//! What the server says, and the line it writes for every request.
//!
//! Access lines go to standard output and everything else to standard error,
//! so a log collector can tell traffic from trouble without parsing either.
//! A line that cannot be written is dropped rather than ending the process
//! in the middle of answering somebody.
//!
//! No header is written into an access line. `Cookie`, `Authorization` and
//! `Proxy-Authorization` carry credentials, and the rest are not worth the
//! risk of the next one that does.

use std::fmt;
use std::io::Write;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};

use serde_json::json;

use crate::time::rfc3339;

/// How a line is written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogFormat {
    /// A line a person reads.
    #[default]
    Text,
    /// One JSON object per line, for a collector.
    Json,
}

impl std::str::FromStr for LogFormat {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text.trim() {
            "text" => Ok(LogFormat::Text),
            "json" => Ok(LogFormat::Json),
            other => Err(format!("the log format is text or json, got `{other}`")),
        }
    }
}

impl fmt::Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LogFormat::Text => "text",
            LogFormat::Json => "json",
        })
    }
}

/// How serious a message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something the operator may want to know.
    Info,
    /// Something that went wrong without stopping anything.
    Warn,
    /// Something that failed.
    Error,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// One request, as its access line records it.
#[derive(Debug, Clone)]
pub struct Access<'a> {
    /// The visitor's address, when there is one.
    pub client: Option<IpAddr>,
    /// The method.
    pub method: &'a str,
    /// The target as it arrived, without a fragment.
    pub target: &'a str,
    /// The status sent.
    pub status: u16,
    /// The body bytes sent.
    pub bytes: u64,
    /// From the first byte of the request to the last byte of the response.
    pub took: Duration,
}

/// Where the server's lines go.
#[derive(Debug, Clone)]
pub struct Log {
    format: LogFormat,
    /// What a text line starts with, such as `lumen-server`.
    name: String,
}

impl Log {
    /// A log writing `format`, whose text lines start with `name`.
    pub fn new(format: LogFormat, name: &str) -> Self {
        Self {
            format,
            name: name.to_string(),
        }
    }

    /// Say `message` at `level`, to standard error.
    pub fn say(&self, level: Level, message: &str) {
        let line = match self.format {
            LogFormat::Text => match level {
                Level::Info => format!("{}: {message}", self.name),
                other => format!("{}: {}: {message}", self.name, other.name()),
            },
            LogFormat::Json => json!({
                "time": rfc3339(SystemTime::now()),
                "level": level.name(),
                "pid": std::process::id(),
                "message": message,
            })
            .to_string(),
        };
        // One write per line: workers share the stream, and a line written
        // in pieces can be split by another process's.
        let _ = std::io::stderr()
            .lock()
            .write_all(format!("{line}\n").as_bytes());
    }

    /// [`Self::say`] at [`Level::Info`].
    pub fn info(&self, message: &str) {
        self.say(Level::Info, message);
    }

    /// [`Self::say`] at [`Level::Warn`].
    pub fn warn(&self, message: &str) {
        self.say(Level::Warn, message);
    }

    /// [`Self::say`] at [`Level::Error`].
    pub fn error(&self, message: &str) {
        self.say(Level::Error, message);
    }

    /// Write the access line for one request, to standard output.
    pub fn access(&self, entry: &Access<'_>) {
        let line = self.access_line(entry, SystemTime::now());
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(format!("{line}\n").as_bytes());
        let _ = out.flush();
    }

    fn access_line(&self, entry: &Access<'_>, at: SystemTime) -> String {
        let millis = entry.took.as_secs_f64() * 1000.0;
        match self.format {
            LogFormat::Text => format!(
                "{} {} \"{} {}\" {} {} {millis:.1}ms",
                rfc3339(at),
                entry
                    .client
                    .map(|client| client.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                entry.method,
                entry.target.escape_debug(),
                entry.status,
                entry.bytes,
            ),
            LogFormat::Json => json!({
                "time": rfc3339(at),
                "pid": std::process::id(),
                "client": entry.client.map(|client| client.to_string()),
                "method": entry.method,
                "target": entry.target,
                "status": entry.status,
                "bytes": entry.bytes,
                "duration_ms": (millis * 10.0).round() / 10.0,
            })
            .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    fn entry() -> Access<'static> {
        Access {
            client: Some("203.0.113.9".parse().expect("an address")),
            method: "GET",
            target: "/user/42?tab=posts",
            status: 200,
            bytes: 512,
            took: Duration::from_micros(12_345),
        }
    }

    #[test]
    fn an_access_line_reads_as_text_or_as_json() {
        let at = UNIX_EPOCH + Duration::from_secs(784_111_777);
        let text = Log::new(LogFormat::Text, "lumen-server").access_line(&entry(), at);
        assert_eq!(
            text,
            "1994-11-06T08:49:37.000Z 203.0.113.9 \"GET /user/42?tab=posts\" 200 512 12.3ms"
        );
        let line = Log::new(LogFormat::Json, "lumen-server").access_line(&entry(), at);
        let json: serde_json::Value = serde_json::from_str(&line).expect("one JSON object");
        assert_eq!(json["status"], 200);
        assert_eq!(json["target"], "/user/42?tab=posts");
        assert_eq!(json["client"], "203.0.113.9");
        assert_eq!(json["duration_ms"], 12.3);
    }

    #[test]
    fn a_log_format_is_named_text_or_json() {
        assert_eq!("json".parse::<LogFormat>(), Ok(LogFormat::Json));
        assert_eq!("text".parse::<LogFormat>(), Ok(LogFormat::Text));
        assert!("yaml".parse::<LogFormat>().is_err());
    }
}
