//! The error banner drawn across the top of the window.
//!
//! A markup or CSS file that stops parsing on a hot reload, and a script that
//! does not load, are reported here as well as on stderr, so the failure is
//! visible where the author is looking. The banner holds the whole report in
//! a read-only text field, which the user can select from and copy, and a
//! Copy button that puts the whole report on the clipboard. When the error
//! names a place in a file, the report reads `file:line:col` and shows that
//! line of the file under it.
//!
//! The banner is a root of its own beside the document, lifted into the top
//! paint band and kept out of the scripting DOM. It is styled once, when it
//! appears, against the app's combined stylesheet: the user-agent sheet
//! carries its rules and the `--lumen-error-*` tokens they read, so a skin or
//! the app restyles it like any other element.

use std::fmt;

use bevy_ecs::hierarchy::Children;
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::*;
use bevy_ecs::system::{Commands, NonSend, Res, ResMut};
use lumen_core::components::{DomHidden, LumenClasses, LumenTag, ReadOnly};
use lumen_core::input::{ClickEvent, Key, KeyPressed, NamedKey};
use lumen_core::render_world::OverlayLayer;
use lumen_core::warn_line;
use lumen_ir::css::apply_css_with_media;
use lumen_ir::layout_ir::{Attributes, Element, LayoutIR};
use lumen_os_clipboard::ClipboardHost;
use lumen_scene::spawn::{DocumentRoot, Placeholders, spawn_subtree};
use lumen_script::ScriptError;

use super::{LastMediaContext, RuntimeStylesheet};

/// Class on the banner's root. The user-agent sheet styles it and the two
/// classes below.
const BANNER_CLASS: &str = "lumen-error";
/// Class on the read-only field that holds the report.
const TEXT_CLASS: &str = "lumen-error-text";
/// Class on the Copy button.
const COPY_CLASS: &str = "lumen-error-copy";

/// The error the window shows, if any. Setting it shows the banner, a new
/// value replaces what it shows, and `None` takes it down. Esc clears it.
#[derive(Resource, Default, Debug)]
pub struct ErrorBanner(pub Option<ErrorReport>);

/// Marker on the banner's root entity.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ErrorBannerMarker;

/// Marker on the banner's Copy button.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ErrorBannerCopy;

/// A place in a source file: 1-based line and column. A column of `0` means
/// the error names the line only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorLocation {
    /// The file, as the error named it.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column, or `0` when unknown.
    pub col: u32,
}

impl fmt::Display for ErrorLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.col > 0 {
            write!(f, "{}:{}:{}", self.file, self.line, self.col)
        } else {
            write!(f, "{}:{}", self.file, self.line)
        }
    }
}

/// One error as the banner shows it. Its [`Display`](fmt::Display) form is
/// the banner's text and what the Copy button copies:
///
/// ```text
/// script load failed: unknown function `missing`
///  --> /app/src/main.cdl:4:5
///   |
/// 4 |     missing(n);
///   |     ^
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorReport {
    /// What went wrong, on one line.
    pub message: String,
    /// Where, when the error names a place.
    pub location: Option<ErrorLocation>,
    /// The text of the line [`Self::location`] names, when the file could be
    /// read.
    pub source_line: Option<String>,
}

impl ErrorReport {
    /// A report with no place in a file.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    /// Point the report at `location`, reading the line it names from the
    /// file. A file that cannot be read (a shipped app carries no sources)
    /// leaves the location without its line.
    #[must_use]
    pub fn at(mut self, location: ErrorLocation) -> Self {
        self.source_line = read_line(&location.file, location.line);
        self.location = Some(location);
        self
    }

    /// The report for a script host's error, prefixed with `context`. A
    /// compile error that knows its line points at it.
    pub fn from_script(context: &str, error: &ScriptError) -> Self {
        match error {
            ScriptError::Compile {
                uri,
                line,
                col,
                message,
            } if *line > 0 => Self::new(format!("{context}: {message}")).at(ErrorLocation {
                file: uri.clone(),
                line: *line,
                col: *col,
            }),
            other => Self::new(format!("{context}: {other}")),
        }
    }

    /// The report for a script that failed to load at startup.
    pub fn from_load_failure(failure: &lumen_script::ScriptLoadFailure) -> Self {
        const CONTEXT: &str = "script load failed";
        match &failure.1 {
            Some(error) => Self::from_script(CONTEXT, error),
            None => Self::new(format!("{CONTEXT}: {}", failure.0)),
        }
    }
}

impl fmt::Display for ErrorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        let Some(location) = &self.location else {
            return Ok(());
        };
        let number = location.line.to_string();
        let gutter = " ".repeat(number.len());
        write!(f, "\n{gutter}--> {location}")?;
        let Some(source) = &self.source_line else {
            return Ok(());
        };
        write!(f, "\n{gutter} |\n{number} | {source}")?;
        if location.col > 0 {
            // Tabs before the column stay tabs, so the caret lands under the
            // column however wide the reader's tabs are.
            let lead: String = source
                .chars()
                .take(location.col as usize - 1)
                .map(|c| if c == '\t' { '\t' } else { ' ' })
                .collect();
            write!(f, "\n{gutter} | {lead}^")?;
        }
        Ok(())
    }
}

/// Line `line` (1-based) of the file at `path`, without its line ending.
fn read_line(path: &str, line: u32) -> Option<String> {
    let index = usize::try_from(line).ok()?.checked_sub(1)?;
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .nth(index)
        .map(|l| l.trim_end_matches('\r').to_string())
}

/// Show what [`ErrorBanner`] holds: when it changes, the banner on screen is
/// replaced with one for the new report, or taken down when there is none.
///
/// Runs in `TickStage::Systems`; the swap is queued, so it lands before the
/// layout pass of the same tick.
pub fn reconcile_error_banner(mut commands: Commands, banner: Res<ErrorBanner>) {
    if !banner.is_changed() {
        return;
    }
    let report = banner.0.clone();
    commands.queue(move |world: &mut World| show(world, report.as_ref()));
}

/// Replace the banner on screen with one showing `report`, or remove it.
fn show(world: &mut World, report: Option<&ErrorReport>) {
    let shown: Vec<Entity> = world
        .query_filtered::<Entity, With<ErrorBannerMarker>>()
        .iter(world)
        .collect();
    for entity in shown {
        world.despawn(entity);
    }
    let Some(report) = report else {
        return;
    };
    let banner = banner_element(&report.to_string());
    // Styled where it sits: under a root carrying the document root's tag and
    // classes, so `:root` tokens and a theme class on the root reach it.
    let (tag, classes) = document_root(world);
    let mut staged = LayoutIR {
        root: Element {
            tag,
            attrs: Box::new(Attributes {
                classes,
                ..Attributes::default()
            }),
            children: vec![banner],
            ..Default::default()
        },
        ..LayoutIR::default()
    };
    if let Some(sheet) = world.get_resource::<RuntimeStylesheet>() {
        let media = world
            .get_resource::<LastMediaContext>()
            .map(|m| m.0)
            .unwrap_or_default();
        if let Err(e) = apply_css_with_media(&mut staged, &sheet.0, &media) {
            tracing::warn!("error banner: the stylesheet failed to apply: {e}");
        }
    }
    let Some(banner) = staged.root.children.pop() else {
        return;
    };
    // The report is text, not markup: braces in it are not placeholders.
    let root = spawn_subtree(world, &banner, None, Placeholders::Resolved);
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        if let Some(children) = world.get::<Children>(entity) {
            stack.extend(children.iter());
        }
        let has = |class: &str| {
            world
                .get::<LumenClasses>(entity)
                .is_some_and(|c| c.has(class))
        };
        let (text, copy) = (has(TEXT_CLASS), has(COPY_CLASS));
        let mut e = world.entity_mut(entity);
        e.insert(DomHidden);
        if text {
            e.insert(ReadOnly);
        }
        if copy {
            e.insert(ErrorBannerCopy);
        }
    }
    world
        .entity_mut(root)
        .insert((ErrorBannerMarker, OverlayLayer));
}

/// The tag and classes of the document's root, for the banner to be styled
/// under. `root` with no classes before a document exists.
fn document_root(world: &World) -> (String, Vec<String>) {
    let root = world.get_resource::<DocumentRoot>().map(|r| r.0);
    let tag = root
        .and_then(|r| world.get::<LumenTag>(r))
        .map_or_else(|| "root".to_string(), |t| t.0.to_string());
    let classes = root
        .and_then(|r| world.get::<LumenClasses>(r))
        .map(|c| c.0.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    (tag, classes)
}

/// The banner's tree: the report in a read-only field, and a Copy button
/// beside it.
fn banner_element(text: &str) -> Element {
    let element = |tag: &str, class: &str| {
        let mut el = Element {
            tag: tag.to_string(),
            ..Default::default()
        };
        el.attrs.classes = vec![class.to_string()];
        el
    };
    let mut field = element("textarea", TEXT_CLASS);
    field.attrs.text = Some(text.to_string());
    let mut copy = element("button", COPY_CLASS);
    copy.attrs.text = Some("Copy".to_string());
    copy.attrs.tab_index = Some(0);
    let mut banner = element("row", BANNER_CLASS);
    banner.children = vec![field, copy];
    banner
}

/// A click on the banner's Copy button puts the whole report on the
/// clipboard, through the same clipboard host the text fields and the
/// `clipboard_write` builtin use.
pub fn copy_error_banner_on_click(
    mut clicks: MessageReader<ClickEvent>,
    banner: Res<ErrorBanner>,
    buttons: Query<(), With<ErrorBannerCopy>>,
    clipboard: Option<NonSend<ClipboardHost>>,
) {
    for click in clicks.read() {
        if !buttons.contains(click.entity) {
            continue;
        }
        let Some(report) = &banner.0 else {
            continue;
        };
        match clipboard.as_deref() {
            Some(clip) => {
                if !clip.write_text(&report.to_string()) {
                    warn_line!("error banner: the clipboard refused the text");
                }
            }
            None => warn_line!("error banner: no clipboard backend to copy to"),
        }
    }
}

/// Esc clears the error banner. Wired alongside the dialog Esc
/// handler so a single keystroke dismisses any blocking surface.
pub fn dismiss_error_banner_on_escape(
    mut keys: MessageReader<KeyPressed>,
    mut banner: ResMut<ErrorBanner>,
) {
    if banner.0.is_none() {
        keys.read().for_each(drop);
        return;
    }
    let pressed = keys
        .read()
        .any(|k| matches!(&k.key, Key::Named(NamedKey::Escape)));
    if pressed {
        banner.0 = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(line: u32, col: u32) -> ErrorLocation {
        ErrorLocation {
            file: "/app/src/main.cdl".to_string(),
            line,
            col,
        }
    }

    /// The report reads `file:line:col` under the message, then the line
    /// with a caret under the column.
    #[test]
    fn a_located_report_shows_the_place_and_the_line() {
        let report = ErrorReport {
            message: "script load failed: unknown function".to_string(),
            location: Some(location(12, 5)),
            source_line: Some("    missing(n);".to_string()),
        };
        assert_eq!(
            report.to_string(),
            "script load failed: unknown function\n  \
             --> /app/src/main.cdl:12:5\n   |\n\
             12 |     missing(n);\n   |     ^"
        );
    }

    /// A tab before the column stays a tab in the caret line.
    #[test]
    fn the_caret_keeps_the_tabs_before_it() {
        let report = ErrorReport {
            message: "m".to_string(),
            location: Some(location(1, 3)),
            source_line: Some("\tx y".to_string()),
        };
        assert!(report.to_string().ends_with("\n  | \t ^"), "{report}");
    }

    /// With no column the place is `file:line` and there is no caret; with
    /// no readable file there is no source line.
    #[test]
    fn a_report_says_only_what_it_knows() {
        let line_only = ErrorReport {
            message: "m".to_string(),
            location: Some(location(2, 0)),
            source_line: Some("text".to_string()),
        };
        assert_eq!(
            line_only.to_string(),
            "m\n --> /app/src/main.cdl:2\n  |\n2 | text"
        );
        let unread = ErrorReport::new("m").at(ErrorLocation {
            file: "/nonexistent/lumen/file.cdl".to_string(),
            line: 1,
            col: 1,
        });
        assert_eq!(
            unread.to_string(),
            "m\n --> /nonexistent/lumen/file.cdl:1:1"
        );
        assert_eq!(ErrorReport::new("just this").to_string(), "just this");
    }

    /// A compile error points at its file and reads the line from it; any
    /// other error is its message.
    #[test]
    fn a_script_compile_error_reads_its_line() {
        let dir = std::env::temp_dir().join(format!("lumen_error_banner_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.cdl");
        std::fs::write(&file, "fn main() {\r\n  oops();\r\n}\r\n").unwrap();
        let error = ScriptError::Compile {
            uri: file.display().to_string(),
            line: 2,
            col: 3,
            message: "unknown function `oops`".to_string(),
        };
        let report = ErrorReport::from_script("hot-reload", &error);
        assert_eq!(report.message, "hot-reload: unknown function `oops`");
        assert_eq!(report.source_line.as_deref(), Some("  oops();"));
        assert!(
            report
                .to_string()
                .contains(&format!("--> {}:2:3", file.display())),
            "{report}"
        );
        let runtime = ErrorReport::from_script("ctx", &ScriptError::Runtime("boom".into()));
        assert_eq!(runtime.to_string(), "ctx: script runtime error: boom");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
