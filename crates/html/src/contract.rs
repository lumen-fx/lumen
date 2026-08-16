//! Names, versions and file shapes both halves of the web target read.
//!
//! The emitter writes these; the browser runtime reads them back. A change
//! to anything in this module changes the wire between them, which is what
//! [`LM_CONTRACT_VERSION`] exists to make visible.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use lumen_core::components::Color;
use lumen_core::property_store::PropertyValue;
use lumen_ir::css::WebNames;
use serde::{Deserialize, Serialize};

/// Version of the emitter/runtime contract: node paths, `data-lm-*`
/// attributes, manifest fields, seed shape.
///
/// It travels three ways so a mismatch is caught wherever it is noticed
/// first: the [`DATA_LM_CONTRACT`] attribute on the document element, the
/// `contract_version` field of the [`Manifest`], and the same field on a
/// [`Seed`]. A runtime that reads a version it does not implement refuses
/// the document rather than guessing.
pub const LM_CONTRACT_VERSION: u32 = 1;

/// Node identity: the [`NodePath`] of the IR node this element came from.
pub const DATA_LM: &str = "data-lm";

/// The `<for>` row key, when the block is keyed.
pub const DATA_LM_KEY: &str = "data-lm-key";

/// Marks an element the emitter added for presentation only. It stands for
/// no IR node, and a walk that binds entities to elements skips it.
pub const DATA_LM_AUX: &str = "data-lm-aux";

/// Marks the single element that stands for a whole authored widget, so the
/// runtime drives it through a widget adapter instead of walking the parts
/// the parser desugared it into.
pub const DATA_LM_WIDGET: &str = "data-lm-widget";

/// The page key this document was emitted for.
pub const DATA_LM_PAGE: &str = "data-lm-page";

/// The site's base path, so the runtime can build URLs without the manifest.
pub const DATA_LM_BASE: &str = "data-lm-base";

/// The locale this document was emitted for.
pub const DATA_LM_LOCALE: &str = "data-lm-locale";

/// The contract version this document was emitted against.
pub const DATA_LM_CONTRACT: &str = "data-lm-contract";

/// Mirror of the `Selected` marker, so CSS can match what Lumen calls
/// `:selected` and the browser has no selector for.
pub const DATA_LM_SELECTED: &str = "data-lm-selected";

/// Mirror of the checked state for elements that are not a real
/// `<input type=checkbox>` and so have no `:checked` of their own.
pub const DATA_LM_CHECKED: &str = "data-lm-checked";

/// Mirror of the disabled state for elements that take no HTML `disabled`
/// attribute.
pub const DATA_LM_DISABLED: &str = "data-lm-disabled";

/// Mirror of the drop-target hover state Lumen calls `:drag-over`.
pub const DATA_LM_DRAG_OVER: &str = "data-lm-drag-over";

/// Present on a branch that is mounted but not shown, which is what an
/// `<if mode="hide">` does.
pub const DATA_LM_HIDDEN: &str = "data-lm-hidden";

/// The attribute a `<dialog>` carries while it is showing. Lumen writes the
/// name of a signal there and the browser wants the state, so the emitter
/// resolves the signal and writes this into the page. From then on the
/// attribute is the browser's: the runtime shows and closes the element and
/// the element maintains it.
pub const DIALOG_OPEN: &str = "open";

/// `id` of the inline `<script type="application/json">` block holding the
/// page's [`Seed`].
pub const SEED_SCRIPT_ID: &str = "lm-seed";

/// File name of the site manifest.
pub const DEFAULT_MANIFEST_FILE: &str = "lumen.web.json";

/// File name of the compiled app artifact the runtime loads.
pub const DEFAULT_ARTIFACT_FILE: &str = "app.lmna";

/// File name of the emitted stylesheet.
pub const DEFAULT_CSS_FILE: &str = "styles.css";

/// File name of the prebuilt wasm runtime shipped with the toolchain.
pub const DEFAULT_WASM_FILE: &str = "lumen-web.wasm";

/// File name of the JavaScript module that loads the wasm runtime.
pub const DEFAULT_JS_FILE: &str = "lumen-web.js";

/// The names a selector written for a browser needs, gathered for
/// [`lumen_ir::css::selector_to_web`].
///
/// A stylesheet reaches the browser through `lumen-ir`, which cannot
/// depend on this crate and so cannot know that a `<row>` is `.lm-row` or
/// that `:selected` is an attribute here. Hand it this rather than writing
/// the names out a second time.
pub fn web_names() -> WebNames<'static> {
    WebNames {
        tag_class_prefix: crate::tags::LM_CLASS_PREFIX,
        selected: DATA_LM_SELECTED,
        checked: DATA_LM_CHECKED,
        disabled: DATA_LM_DISABLED,
        drag_over: DATA_LM_DRAG_OVER,
    }
}

/// One step of a [`NodePath`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathStep {
    /// A 0-based child index, written `.n` (or bare at the start of a path).
    Child(u32),
    /// A 0-based `<for>` row index, written `::n`.
    Row(u32),
}

/// Identity of one node inside one page, stable across emitter and runtime.
///
/// A path is the chain of 0-based child indices from the page root, joined
/// with `.`; the page root itself is `0`. A `<for>` row extends its block's
/// path with `::n`, and indices inside the row continue after it, so the
/// second child of the third row of a block at `0.2` is `0.2::2.1`.
///
/// Indices count children in the order the IR lists them, which is the order
/// the native spawner visits them in. Presentational elements the emitter
/// adds carry [`DATA_LM_AUX`] instead of a path, so both halves walk the same
/// nodes.
///
/// [`Display`](fmt::Display) writes the canonical form and [`FromStr`] reads
/// it back.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodePath(Vec<PathStep>);

impl NodePath {
    /// The page root, `0`.
    pub fn root() -> Self {
        Self(vec![PathStep::Child(0)])
    }

    /// The path of this node's `index`-th child.
    pub fn child(&self, index: u32) -> Self {
        let mut steps = self.0.clone();
        steps.push(PathStep::Child(index));
        Self(steps)
    }

    /// The path of this block's `index`-th `<for>` row.
    pub fn row(&self, index: u32) -> Self {
        let mut steps = self.0.clone();
        steps.push(PathStep::Row(index));
        Self(steps)
    }

    /// The steps this path is made of, outermost first.
    pub fn steps(&self) -> &[PathStep] {
        &self.0
    }

    /// True when this is the page root.
    pub fn is_root(&self) -> bool {
        self.0.len() == 1
    }

    /// The path of the node this one hangs off, or `None` at the page root.
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        let mut steps = self.0.clone();
        steps.pop();
        Some(Self(steps))
    }
}

impl fmt::Display for NodePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, step) in self.0.iter().enumerate() {
            match step {
                PathStep::Child(n) => {
                    if i > 0 {
                        f.write_str(".")?;
                    }
                    write!(f, "{n}")?;
                }
                PathStep::Row(n) => write!(f, "::{n}")?,
            }
        }
        Ok(())
    }
}

impl FromStr for NodePath {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(PathError::Empty);
        }
        let mut steps = Vec::new();
        let mut rest = s;
        let mut is_row = false;
        loop {
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let (digits, tail) = rest.split_at(end);
            let index: u32 = digits
                .parse()
                .map_err(|_| PathError::BadIndex(digits.to_string()))?;
            steps.push(if is_row {
                PathStep::Row(index)
            } else {
                PathStep::Child(index)
            });
            if tail.is_empty() {
                return Ok(Self(steps));
            }
            if let Some(next) = tail.strip_prefix("::") {
                is_row = true;
                rest = next;
            } else if let Some(next) = tail.strip_prefix('.') {
                is_row = false;
                rest = next;
            } else {
                let c = tail.chars().next().unwrap_or_default();
                return Err(PathError::UnexpectedChar(c));
            }
            if rest.is_empty() {
                return Err(PathError::TrailingSeparator);
            }
        }
    }
}

impl From<&NodePath> for String {
    fn from(path: &NodePath) -> Self {
        path.to_string()
    }
}

/// Why a string is not a [`NodePath`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// The string was empty.
    Empty,
    /// A step was not a decimal index.
    BadIndex(String),
    /// A character appeared where `.` or `::` was expected.
    UnexpectedChar(char),
    /// The string ended on a separator.
    TrailingSeparator,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Empty => f.write_str("node path is empty"),
            PathError::BadIndex(s) => write!(f, "node path step `{s}` is not an index"),
            PathError::UnexpectedChar(c) => write!(f, "unexpected `{c}` in node path"),
            PathError::TrailingSeparator => f.write_str("node path ends on a separator"),
        }
    }
}

impl Error for PathError {}

/// Base writing direction of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// Left to right.
    #[default]
    Ltr,
    /// Right to left.
    Rtl,
}

impl Dir {
    /// The value the HTML `dir` attribute takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::Ltr => "ltr",
            Dir::Rtl => "rtl",
        }
    }
}

impl fmt::Display for Dir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a link to another page of the same site is followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NavigationMode {
    /// The runtime intercepts the click and swaps the page in place.
    #[default]
    Soft,
    /// The browser loads the target document.
    Hard,
}

impl NavigationMode {
    /// The value this mode is written as.
    pub fn as_str(self) -> &'static str {
        match self {
            NavigationMode::Soft => "soft",
            NavigationMode::Hard => "hard",
        }
    }
}

impl fmt::Display for NavigationMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The form a script ships in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptFormat {
    /// Compiled candela bytecode.
    Cdlb,
    /// Source text the host compiles at load.
    Source,
}

impl ScriptFormat {
    /// The value this format is written as.
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptFormat::Cdlb => "cdlb",
            ScriptFormat::Source => "source",
        }
    }
}

impl fmt::Display for ScriptFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One script the runtime loads at boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptRef {
    /// Scripting engine that runs it (`candela`, `rhai`, ...).
    pub engine: String,
    /// Path to the file, relative to the site root.
    pub path: String,
    /// The form the file is in.
    pub format: ScriptFormat,
}

/// `lumen.web.json`: what the runtime needs before it has parsed anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Contract this site was emitted against.
    pub contract_version: u32,
    /// URL prefix every path in the site hangs off, with a trailing slash.
    pub base_path: String,
    /// Page key the site opens on.
    pub entry: String,
    /// Compiled app artifact, relative to the site root.
    pub artifact: String,
    /// Stylesheet, relative to the site root.
    pub css: String,
    /// Wasm runtime, relative to the site root.
    pub wasm: String,
    /// JavaScript module that loads the runtime, relative to the site root.
    pub js: String,
    /// Locale this tree was emitted for.
    pub locale: String,
    /// Base writing direction of this tree.
    pub dir: Dir,
    /// Every locale the site was emitted in, this one included.
    pub locales: Vec<String>,
    /// How same-site links are followed.
    pub navigation: NavigationMode,
    /// Page key to the document that page was emitted as.
    pub pages: BTreeMap<String, String>,
    /// Scripts to load at boot, in order.
    pub scripts: Vec<ScriptRef>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            contract_version: LM_CONTRACT_VERSION,
            base_path: "/".to_string(),
            entry: String::new(),
            artifact: DEFAULT_ARTIFACT_FILE.to_string(),
            css: DEFAULT_CSS_FILE.to_string(),
            wasm: DEFAULT_WASM_FILE.to_string(),
            js: DEFAULT_JS_FILE.to_string(),
            locale: String::new(),
            dir: Dir::Ltr,
            locales: Vec::new(),
            navigation: NavigationMode::Soft,
            pages: BTreeMap::new(),
            scripts: Vec::new(),
        }
    }
}

/// A signal value carried across the wire with its type intact.
///
/// The variants are the [`PropertyValue`] ones a signal can hold across a
/// page load. `Vec2` and `Custom` have no place in a document: the first is
/// geometry the layout recomputes, the second is a live Rust value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
pub enum SeedValue {
    /// Text.
    Str(String),
    /// Signed integer.
    I64(i64),
    /// Float.
    F64(f64),
    /// Boolean.
    Bool(bool),
    /// Color as red, green, blue, alpha, each 0 to 1.
    Color([f32; 4]),
}

impl From<&SeedValue> for PropertyValue {
    fn from(value: &SeedValue) -> Self {
        match value {
            SeedValue::Str(s) => PropertyValue::Str(Arc::from(s.as_str())),
            SeedValue::I64(n) => PropertyValue::I64(*n),
            SeedValue::F64(n) => PropertyValue::F64(*n),
            SeedValue::Bool(b) => PropertyValue::Bool(*b),
            SeedValue::Color([r, g, b, a]) => PropertyValue::Color(Color::rgba(*r, *g, *b, *a)),
        }
    }
}

impl TryFrom<&PropertyValue> for SeedValue {
    type Error = UnsupportedSeedValue;

    fn try_from(value: &PropertyValue) -> Result<Self, Self::Error> {
        match value {
            PropertyValue::Str(s) => Ok(SeedValue::Str(s.to_string())),
            PropertyValue::I64(n) => Ok(SeedValue::I64(*n)),
            PropertyValue::F64(n) => Ok(SeedValue::F64(*n)),
            PropertyValue::Bool(b) => Ok(SeedValue::Bool(*b)),
            PropertyValue::Color(c) => Ok(SeedValue::Color([c.r, c.g, c.b, c.a])),
            PropertyValue::Vec2(_) => Err(UnsupportedSeedValue("vec2")),
            PropertyValue::Custom(_) => Err(UnsupportedSeedValue("custom")),
        }
    }
}

/// A property value that cannot be written into a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedSeedValue(pub &'static str);

impl fmt::Display for UnsupportedSeedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` values cannot be seeded into a page", self.0)
    }
}

impl Error for UnsupportedSeedValue {}

/// The signal state a page was rendered with, inlined into that page.
///
/// The runtime applies it before the first reconcile, so what the markup
/// already shows and what the runtime believes agree. It has to be exactly
/// the state the page was rendered from; anything else shows up as a
/// hydration mismatch.
///
/// Both maps are ordered so the same state always serializes to the same
/// bytes. Rows carry strings because that is what an array signal holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seed {
    /// Contract this seed was written against.
    pub contract_version: u32,
    /// Global signals by name.
    pub globals: BTreeMap<String, SeedValue>,
    /// Array signals by name, each a list of records.
    pub arrays: BTreeMap<String, Vec<BTreeMap<String, String>>>,
}

impl Default for Seed {
    fn default() -> Self {
        Self {
            contract_version: LM_CONTRACT_VERSION,
            globals: BTreeMap::new(),
            arrays: BTreeMap::new(),
        }
    }
}

impl Seed {
    /// An empty seed for the current contract.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when there is nothing to apply.
    pub fn is_empty(&self) -> bool {
        self.globals.is_empty() && self.arrays.is_empty()
    }

    /// Serialize for an inline `<script type="application/json">` block.
    ///
    /// `<` is written as its escape so no seed value can end the block
    /// early, whatever an author put in a signal.
    pub fn to_script_json(&self) -> Result<String, serde_json::Error> {
        Ok(serde_json::to_string(self)?.replace('<', "\\u003c"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_builds_from_the_root_down() {
        let root = NodePath::root();
        assert_eq!(root.to_string(), "0");
        assert!(root.is_root());
        assert_eq!(root.child(1).child(2).to_string(), "0.1.2");
        assert_eq!(root.child(2).row(3).to_string(), "0.2::3");
        assert_eq!(root.child(2).row(3).child(1).to_string(), "0.2::3.1");
    }

    #[test]
    fn path_round_trips_through_its_text_form() {
        for text in ["0", "0.1.2", "0.2::3", "0.2::3.1", "0::0.1::4.2.0"] {
            let path: NodePath = text.parse().expect("parses");
            assert_eq!(path.to_string(), text);
        }
    }

    #[test]
    fn path_steps_keep_rows_apart_from_children() {
        let path: NodePath = "0.2::3.1".parse().expect("parses");
        assert_eq!(
            path.steps(),
            &[
                PathStep::Child(0),
                PathStep::Child(2),
                PathStep::Row(3),
                PathStep::Child(1),
            ]
        );
    }

    #[test]
    fn path_parent_walks_back_up() {
        let path: NodePath = "0.2::3.1".parse().expect("parses");
        assert_eq!(
            path.parent().map(|p| p.to_string()).as_deref(),
            Some("0.2::3")
        );
        assert_eq!(NodePath::root().parent(), None);
    }

    #[test]
    fn path_rejects_malformed_text() {
        assert_eq!("".parse::<NodePath>(), Err(PathError::Empty));
        assert_eq!(
            "::1".parse::<NodePath>(),
            Err(PathError::BadIndex(String::new()))
        );
        assert_eq!(
            "a".parse::<NodePath>(),
            Err(PathError::BadIndex(String::new()))
        );
        assert_eq!("0.".parse::<NodePath>(), Err(PathError::TrailingSeparator));
        assert_eq!("0::".parse::<NodePath>(), Err(PathError::TrailingSeparator));
        assert_eq!(
            "0..1".parse::<NodePath>(),
            Err(PathError::BadIndex(String::new()))
        );
        assert_eq!(
            "0-1".parse::<NodePath>(),
            Err(PathError::UnexpectedChar('-'))
        );
    }

    #[test]
    fn seed_values_carry_their_type() {
        let cases = [
            (SeedValue::Str("hi".into()), r#"{"t":"str","v":"hi"}"#),
            (SeedValue::I64(3), r#"{"t":"i64","v":3}"#),
            (SeedValue::F64(0.5), r#"{"t":"f64","v":0.5}"#),
            (SeedValue::Bool(true), r#"{"t":"bool","v":true}"#),
            (
                SeedValue::Color([1.0, 0.0, 0.5, 1.0]),
                r#"{"t":"color","v":[1.0,0.0,0.5,1.0]}"#,
            ),
        ];
        for (value, json) in cases {
            assert_eq!(serde_json::to_string(&value).expect("serializes"), json);
            let back: SeedValue = serde_json::from_str(json).expect("deserializes");
            assert_eq!(back, value);
        }
    }

    #[test]
    fn seed_values_convert_to_and_from_property_values() {
        let values = [
            SeedValue::Str("hi".into()),
            SeedValue::I64(-2),
            SeedValue::F64(1.25),
            SeedValue::Bool(false),
            SeedValue::Color([0.25, 0.5, 0.75, 1.0]),
        ];
        for value in values {
            let property = PropertyValue::from(&value);
            assert_eq!(SeedValue::try_from(&property), Ok(value));
        }
        let vec2 = PropertyValue::Vec2(Default::default());
        assert!(SeedValue::try_from(&vec2).is_err());
    }

    #[test]
    fn seed_round_trips_and_escapes_its_script_block() {
        let mut seed = Seed::new();
        assert!(seed.is_empty());
        seed.globals
            .insert("title".into(), SeedValue::Str("</script>".into()));
        seed.globals.insert("count".into(), SeedValue::I64(3));
        seed.arrays.insert(
            "rows".into(),
            vec![BTreeMap::from([("id".to_string(), "1".to_string())])],
        );

        let json = serde_json::to_string(&seed).expect("serializes");
        let back: Seed = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, seed);
        assert_eq!(back.contract_version, LM_CONTRACT_VERSION);

        let block = seed.to_script_json().expect("serializes");
        assert!(!block.contains('<'));
        let from_block: Seed = serde_json::from_str(&block).expect("deserializes");
        assert_eq!(from_block, seed);
    }

    /// `lumen-ir` keeps a copy of these names in its own tests, because it
    /// cannot depend on this crate to read them. This is the assertion
    /// that the copy is still the same set.
    #[test]
    fn the_names_a_web_selector_needs_are_the_ones_a_document_carries() {
        let names = web_names();
        assert_eq!(names.tag_class_prefix, "lm-");
        assert_eq!(names.selected, DATA_LM_SELECTED);
        assert_eq!(names.checked, DATA_LM_CHECKED);
        assert_eq!(names.disabled, DATA_LM_DISABLED);
        assert_eq!(names.drag_over, DATA_LM_DRAG_OVER);
        assert_eq!(crate::tags::lm_class("row"), "lm-row");
    }

    #[test]
    fn manifest_round_trips() {
        let manifest = Manifest {
            entry: "index".into(),
            locale: "en-US".into(),
            locales: vec!["en-US".into(), "de-DE".into()],
            pages: BTreeMap::from([
                ("index".to_string(), "index.html".to_string()),
                ("settings".to_string(), "settings.html".to_string()),
            ]),
            scripts: vec![ScriptRef {
                engine: "candela".into(),
                path: "app.cdlb".into(),
                format: ScriptFormat::Cdlb,
            }],
            ..Manifest::default()
        };
        let json = serde_json::to_string(&manifest).expect("serializes");
        assert!(json.contains(r#""format":"cdlb""#));
        assert!(json.contains(r#""dir":"ltr""#));
        assert!(json.contains(r#""navigation":"soft""#));
        let back: Manifest = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, manifest);
        assert_eq!(back.contract_version, LM_CONTRACT_VERSION);
    }
}
