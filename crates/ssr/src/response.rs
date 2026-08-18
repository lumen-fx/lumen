//! What a render answers with, and what a script is allowed to put in it.
//!
//! A page rendered per request answers with more than markup. A page for a
//! record nobody has is a 404, a form that rejects what it was sent is a 422,
//! and a page that decides the visitor belongs somewhere else is a redirect
//! with no document at all. A script says so with the three response
//! commands, and this is where they land.
//!
//! A script writing a response header is a script writing part of an HTTP
//! message, so two kinds of value never make it through: one carrying a line
//! break, which would let a script append headers of its own or a body of its
//! own, and one framing the body, which is the server's to decide and not the
//! page's. Both are refused where the write happens, and the refusal is
//! reported with the render rather than passed on in silence.

use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use lumen_script::ScriptCommand;
use lumen_script::runtime::ScriptCommandEvent;

/// The status a document is answered with when nothing said otherwise.
pub const DEFAULT_STATUS: u16 = 200;

/// The status a redirect is answered with when nothing said otherwise.
///
/// A page that sends a visitor elsewhere is saying so about this request, not
/// about the address for good, so the temporary one is the safe default.
pub const REDIRECT_STATUS: u16 = 302;

/// Names a script may not set, because they say how the body is framed and
/// the server that sends it decides that.
const FRAMING: &[&str] = &["content-length", "transfer-encoding"];

/// What the app has said about the response so far.
#[derive(Resource, Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseState {
    /// Status the app asked for.
    pub status: Option<u16>,
    /// Headers the app set, in the order it set them.
    pub headers: Vec<(String, String)>,
    /// Where the app is sending the visitor instead of answering with a
    /// document.
    pub redirect: Option<String>,
    /// Writes that were refused, said in full so a caller can report them.
    pub refused: Vec<String>,
}

impl ResponseState {
    /// Set a header, replacing any value already set under that name.
    ///
    /// A name or value that would break the response is refused and recorded.
    pub fn set_header(&mut self, name: &str, value: &str) {
        if let Err(refusal) = header_is_sendable(name, value) {
            self.refused.push(refusal);
            return;
        }
        if let Some(existing) = self
            .headers
            .iter_mut()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        {
            existing.1 = value.to_string();
            return;
        }
        self.headers.push((name.to_string(), value.to_string()));
    }

    /// Answer with a redirect to `location` rather than with a document.
    pub fn redirect_to(&mut self, location: &str) {
        if let Err(refusal) = header_is_sendable("location", location) {
            self.refused.push(refusal);
            return;
        }
        self.redirect = Some(location.to_string());
    }
}

/// Whether a header can be sent as written, and why not when it cannot.
fn header_is_sendable(name: &str, value: &str) -> Result<(), String> {
    if name.is_empty() || !name.bytes().all(is_token_byte) {
        return Err(format!(
            "the response header `{}` was not set: a header name is a token, and this one is not",
            name.escape_debug()
        ));
    }
    if value
        .bytes()
        .any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
    {
        return Err(format!(
            "the response header `{name}` was not set: its value holds a line break, which would \
             end the header and start something else"
        ));
    }
    if FRAMING
        .iter()
        .any(|framing| name.eq_ignore_ascii_case(framing))
    {
        return Err(format!(
            "the response header `{name}` was not set: how the body is framed is the server's to \
             say, not the page's"
        ));
    }
    Ok(())
}

/// The bytes a header name may be made of, which is HTTP's token rule.
fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// Read the response commands the app's scripts emitted into [`ResponseState`].
///
/// Installed by the renderer, because a response is only a thing where a
/// request is: a desktop app and a browser page both carry these commands and
/// neither has anywhere to put them.
pub fn apply_response_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut response: ResMut<ResponseState>,
) {
    for event in events.read() {
        match &event.0 {
            ScriptCommand::SetResponseStatus { status } => response.status = Some(*status),
            ScriptCommand::SetResponseHeader { name, value } => response.set_header(name, value),
            ScriptCommand::Redirect { location } => response.redirect_to(location),
            _ => {}
        }
    }
}

/// One rendered response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SsrResponse {
    /// Status code.
    pub status: u16,
    /// Response headers, in the order to send them.
    pub headers: Vec<(String, String)>,
    /// The document, empty for a redirect.
    pub body: String,
    /// What the render could not do the way the app asked. Log these: a page
    /// that renders without the data it wanted still renders.
    pub warnings: Vec<String>,
}

impl SsrResponse {
    /// The value of a header, if it is set.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_with_a_line_break_in_it_is_refused() {
        let mut response = ResponseState::default();
        response.set_header("X-Greeting", "hello\r\nSet-Cookie: admin=1");
        assert!(response.headers.is_empty());
        assert_eq!(response.refused.len(), 1);
        assert!(
            response.refused[0].contains("line break"),
            "{:?}",
            response.refused
        );
    }

    #[test]
    fn a_header_name_that_is_not_a_name_is_refused() {
        let mut response = ResponseState::default();
        response.set_header("X-Bad Name", "1");
        response.set_header("", "1");
        assert!(response.headers.is_empty());
        assert_eq!(response.refused.len(), 2);
    }

    #[test]
    fn the_framing_of_the_body_stays_the_servers() {
        let mut response = ResponseState::default();
        response.set_header("Content-Length", "0");
        response.set_header("transfer-encoding", "chunked");
        assert!(response.headers.is_empty());
        assert_eq!(response.refused.len(), 2);
    }

    #[test]
    fn setting_a_header_twice_replaces_it() {
        let mut response = ResponseState::default();
        response.set_header("Cache-Control", "no-store");
        response.set_header("cache-control", "max-age=60");
        assert_eq!(
            response.headers,
            vec![("Cache-Control".to_string(), "max-age=60".to_string())]
        );
    }

    #[test]
    fn a_redirect_to_a_line_break_is_refused() {
        let mut response = ResponseState::default();
        response.redirect_to("/next\r\nX-Sneaky: 1");
        assert!(response.redirect.is_none());
        assert_eq!(response.refused.len(), 1);
    }
}
