//! `lumenc lint --signals <app-dir>` - static signal lint.
//!
//! Scans the app's markup entry and its script (`main.cdl`, `main.rhai`, or
//! `main.lua`), both under `<app-dir>/src`, against the optional `[signals]`
//! schema declared in `<app-dir>/lumen.toml` and reports four classes of
//! finding:
//!
//! - **Untyped write** - `signal_set("count", "5")` reaches the
//!   string-typed sink and bypasses the typed PropertyStore variant.
//!   Migration prompt: prefer the signal handle, which carries the
//!   type - `signal<int>("count").set(5)` in candela, chained
//!   `signals.count.set(5)` in rhai and lua - whenever the value fits
//!   a PropertyValue scalar. Skipped when the schema declares the
//!   signal as `string`: there is no `signal_set_string` to migrate
//!   to, so `signal_set` is already the typed sink.
//! - **Bare interpolation ambiguity** - `<text>{count}</text>` reads
//!   the loop / template scope; the global variant is `{$count}`
//!   (or `{$self.field}` inside a component instance). Info-level
//!   nudge so the renderer's resolution path is explicit.
//! - **Schema mismatch** - `lumen.toml` declares `count = "i64"` but
//!   the script writes `signal_set("count", "hello")`. Hard error
//!   even in non-strict mode.
//! - **Untracked signal** - markup binds `theme` but no schema
//!   entry and no script write - likely a dead binding.
//! - **Orphan write** - script writes `foo` but no markup reads it -
//!   likely dead code.
//!
//! The scanner is **substring-based**: it does not parse the script
//! AST and only recognizes top-level call sites of the form
//! `signal_set("name", ...)` (and the four typed variants), the
//! handle forms `signal<int>("name").set(...)`, the bare
//! `signal("name").set(...)` whose unpinned type parameter is `any`,
//! and `signals.name.set(...)`, with or without a host prefix such as
//! candela's `lumen::`. Limits:
//!
//! - Comments containing literal `signal_set("...")` are still flagged
//!   (false positives in commented-out code).
//! - String literals or `#{}` map values containing a substring like
//!   `"signal_set("` will trip the scanner - extremely rare in
//!   practice but worth noting.
//! - Nested calls like `signal_set("a", signal_get("b"))` only
//!   recognise the outermost `signal_set` (the inner read is fine,
//!   we just don't classify it as a typed read).
//! - Multi-line string literals across newlines are not honoured;
//!   the scanner treats the first `"` after the comma as the value
//!   start.
//!
//! The lint is intentionally pessimistic on legacy code: the goal is
//! to drive the migration off untyped `signal_set` toward the typed
//! handle, so warnings on existing apps are expected.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;

use crate::app_layout::AppLayout;
use crate::config::{LumenToml, SignalType, SignalsCfg};
use crate::layout_ir::{LintFinding, LintKind, LintSeverity};

/// Convert a parser-emitted [`LintFinding`] (anchored to its source
/// file) into a CLI [`Finding`] so the two pipelines can share the
/// same emission code. Round-8 wave B uses this to fold structured
/// parser findings into the lint stream instead of relying on the
/// substring scan.
///
/// Fails for parser findings outside this command's remit; the signals
/// lint reports on signals, so a markup rule such as
/// [`LintKind::BooleanAttribute`] is dropped rather than reshaped into
/// a signal finding. `lumenc check` surfaces those.
impl TryFrom<(&LintFinding, &Path)> for Finding {
    type Error = LintKind;

    fn try_from((f, path): (&LintFinding, &Path)) -> Result<Self, Self::Error> {
        let kind = match f.kind {
            LintKind::BareInterpolation => FindingKind::BareInterpolation,
            other => return Err(other),
        };
        let severity = Severity::from(f.severity);
        // Recover the signal name from the suggestion (`{$name}`) when
        // possible; fall back to empty if the suggest is missing.
        let signal = f
            .suggest
            .as_deref()
            .and_then(|s| s.strip_prefix("{$"))
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or("")
            .to_string();
        Ok(Finding {
            file: path.to_path_buf(),
            line: f.line,
            col: f.col,
            signal,
            kind,
            severity,
            message: f.message.clone(),
            suggestion: f.suggest.clone().unwrap_or_default(),
        })
    }
}

impl From<LintSeverity> for Severity {
    fn from(s: LintSeverity) -> Self {
        match s {
            LintSeverity::Error => Severity::Error,
            LintSeverity::Warn => Severity::Warn,
            LintSeverity::Info => Severity::Info,
            LintSeverity::Hint => Severity::Hint,
        }
    }
}

/// Severity tier for a lint finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Definitive bug - hard fail.
    Error,
    /// Probably wrong - flagged in `--strict`, advisory otherwise.
    Warn,
    /// Stylistic / migration nudge.
    Info,
    /// Lowest priority - likely dead code.
    Hint,
}

impl From<Severity> for &'static str {
    fn from(s: Severity) -> &'static str {
        match s {
            Severity::Error => "error",
            Severity::Warn => "warn",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }
}

/// Lint finding category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    /// String-typed `signal_set` reached when a typed variant would fit.
    UntypedWrite,
    /// Schema-declared signal written with the wrong runtime type.
    SchemaMismatch,
    /// `{name}` interpolation without `$` prefix.
    BareInterpolation,
    /// Markup-bound signal that is never declared / never written.
    UntrackedSignal,
    /// Signal written but no markup binding / read.
    OrphanWrite,
}

impl From<FindingKind> for &'static str {
    fn from(k: FindingKind) -> &'static str {
        match k {
            FindingKind::UntypedWrite => "untyped-write",
            FindingKind::SchemaMismatch => "schema-mismatch",
            FindingKind::BareInterpolation => "bare-interpolation",
            FindingKind::UntrackedSignal => "untracked-signal",
            FindingKind::OrphanWrite => "orphan-write",
        }
    }
}

/// One lint finding.
#[derive(Debug, Clone)]
pub struct Finding {
    /// Absolute or app-relative path of the file where the finding lives.
    pub file: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column.
    pub col: usize,
    /// Signal name the finding refers to (may be empty for bare
    /// interpolation when the name was unparseable).
    pub signal: String,
    /// Category bucket.
    pub kind: FindingKind,
    /// Severity tier.
    pub severity: Severity,
    /// Human-readable description.
    pub message: String,
    /// Suggested fix.
    pub suggestion: String,
}

/// Entry point: `lumenc lint --signals <app-dir> [--json] [--strict]`.
pub fn cmd_lint_signals(args: impl Iterator<Item = String>) -> ExitCode {
    const LINT_SIGNALS_USAGE: &str = "lumenc lint --signals - offline signal lint

USAGE:
    lumenc lint --signals [<app-dir>] [--json] [--strict]

Reads <app-dir>/src/main.lmn, the app script (src/main.cdl / src/main.rhai
/ src/main.lua), and the optional [signals] schema in lumen.toml, then flags
untyped writes, bare {name} interpolation ambiguities, schema mismatches,
untracked binds, and orphan writes.

    --json            One JSON object per finding.
    --strict          Upgrade warnings to errors.";
    let mut dir: Option<PathBuf> = None;
    let mut as_json = false;
    let mut strict = false;
    let args = args.peekable();
    for a in args {
        match a.as_str() {
            h if crate::is_help_flag(h) => {
                println!("{LINT_SIGNALS_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--json" => as_json = true,
            "--strict" => strict = true,
            s if !s.starts_with("--") && dir.is_none() => dir = Some(PathBuf::from(s)),
            other => {
                eprintln!("lumenc lint --signals: unknown arg `{other}`");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("lumenc lint --signals: missing <app-dir>");
        return ExitCode::from(2);
    };
    if !dir.is_dir() {
        eprintln!(
            "lumenc lint --signals: {} is not a directory",
            dir.display()
        );
        return ExitCode::from(2);
    }

    let cfg = match LumenToml::load_or_default(&dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lumenc lint --signals: lumen.toml: {e}");
            return ExitCode::FAILURE;
        }
    };

    let layout = match AppLayout::resolve(&dir, &cfg) {
        Ok(layout) => layout,
        Err(e) => {
            eprintln!("lumenc lint --signals: {e}");
            return ExitCode::FAILURE;
        }
    };
    let lmn_path = layout.entry_path;
    let lmn_src = std::fs::read_to_string(&lmn_path).unwrap_or_default();
    // The app's script, whichever host it targets. The scanner matches call
    // sites by substring, so a candela `lumen::signal_set_int(...)` and a Rhai
    // `signal_set_int(...)` both land; only the file name differs. With no
    // script present at all the path names the default host's file, which is
    // the one an author would go on to create.
    let script_path = ["main.cdl", "main.rhai", "main.lua"]
        .iter()
        .map(|f| layout.src_dir.join(f))
        .find(|p| p.is_file())
        .unwrap_or_else(|| layout.src_dir.join("main.cdl"));
    let script_src = std::fs::read_to_string(&script_path).unwrap_or_default();

    let report = analyze(&cfg.signals, &lmn_src, &lmn_path, &script_src, &script_path);

    emit(&report, as_json, strict)
}

/// Pure analysis pass - given the schema and both source texts,
/// returns the full finding list. Split out so the unit tests can
/// exercise it without touching the filesystem.
///
/// `BareInterpolation` findings are preferentially sourced from the
/// markup parser's structured [`LintFinding`] output (round-8 wave B);
/// the substring-based scanner only steps in if the parse failed.
/// Parse-side findings carry accurate line / column numbers tied to
/// the actual brace positions, which the legacy substring scan can
/// drift on for multi-line text nodes.
pub fn analyze(
    schema: &SignalsCfg,
    lmn_src: &str,
    lmn_path: &Path,
    script_src: &str,
    script_path: &Path,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Try to parse the markup first; on success, use the structured
    // `lint_findings` for the bare-interpolation rule. We pass the
    // success flag down to `scan_markup` so it skips its own bare-
    // interpolation pass and avoids duplicate findings.
    // The structured parser pass is only available when the markup parser
    // is compiled in (`runtime-parse`). Without it, `scan_markup` falls back
    // to its own substring bare-interpolation scan.
    #[cfg(feature = "runtime-parse")]
    let parser_findings: Option<Vec<LintFinding>> = crate::parse::html::parse_html(lmn_src)
        .ok()
        .map(|ir| ir.lint_findings);
    #[cfg(not(feature = "runtime-parse"))]
    let parser_findings: Option<Vec<LintFinding>> = None;
    if let Some(pf) = &parser_findings {
        findings.extend(
            pf.iter()
                .filter_map(|f| Finding::try_from((f, lmn_path)).ok()),
        );
    }
    let skip_bare_substring_scan = parser_findings.is_some();

    let markup_refs = scan_markup(lmn_src, lmn_path, &mut findings, skip_bare_substring_scan);
    let script_writes = scan_script(script_src, script_path, schema, &mut findings);
    let script_reads = scan_script_reads(script_src);

    // Untracked signal: markup bind refers to a name with no schema
    // entry AND no script write.
    let mut untracked: BTreeSet<&str> = BTreeSet::new();
    for r in &markup_refs {
        let name = r.signal.as_str();
        if !schema.fields.contains_key(name)
            && !script_writes.iter().any(|w| w.name == name)
            && !is_template_field(name, lmn_src)
        {
            untracked.insert(name);
        }
    }
    for name in untracked {
        // Find the first markup ref for line/col anchoring.
        if let Some(r) = markup_refs.iter().find(|x| x.signal == name) {
            findings.push(Finding {
                file: lmn_path.to_path_buf(),
                line: r.line,
                col: r.col,
                signal: name.to_string(),
                kind: FindingKind::UntrackedSignal,
                severity: Severity::Warn,
                message: format!(
                    "signal `{name}` is bound from markup but never declared in [signals] nor written from any script"
                ),
                suggestion: format!(
                    "declare `{name} = \"...\"` in lumen.toml [signals] or write it from the app script"
                ),
            });
        }
    }

    // Orphan write: script writes a signal but no markup bind / read /
    // schema entry covers it.
    let mut orphans: BTreeSet<&str> = BTreeSet::new();
    for w in &script_writes {
        let name = w.name.as_str();
        // Skip framework-internal "__menu_open:..." / "valid:..." names.
        if name.starts_with("__") || name.starts_with("valid:") {
            continue;
        }
        if !markup_refs.iter().any(|r| r.signal == name)
            && !schema.fields.contains_key(name)
            && !script_reads.contains(name)
        {
            orphans.insert(name);
        }
    }
    for name in orphans {
        if let Some(w) = script_writes.iter().find(|w| w.name == name) {
            findings.push(Finding {
                file: script_path.to_path_buf(),
                line: w.line,
                col: w.col,
                signal: name.to_string(),
                kind: FindingKind::OrphanWrite,
                severity: Severity::Hint,
                message: format!(
                    "signal `{name}` is written from a script but no markup binding, schema entry, or read references it"
                ),
                suggestion: format!(
                    "remove the write if dead, or add `bind-text=\"{name}\"` markup / a [signals] entry"
                ),
            });
        }
    }

    // Deterministic ordering - by file, line, then column, then kind.
    findings.sort_by(|a, b| {
        let ak: &'static str = a.kind.into();
        let bk: &'static str = b.kind.into();
        (a.file.as_path(), a.line, a.col, ak).cmp(&(b.file.as_path(), b.line, b.col, bk))
    });
    findings
}

#[derive(Debug, Clone)]
struct MarkupRef {
    signal: String,
    line: usize,
    col: usize,
}

#[derive(Debug, Clone)]
struct ScriptWrite {
    name: String,
    line: usize,
    col: usize,
    /// Typed-variant tag (`Some(I64)` for `signal_set_int`, etc.).
    /// `None` for the untyped `signal_set` legacy form and the
    /// chained `signals.<name>.set(...)` / `signal("name", ...)` /
    /// `signal_array("name")` builders. Read by tests / future
    /// rule expansions; kept on the struct so the writes list
    /// stays the single source of truth.
    #[allow(dead_code)]
    typed: Option<SignalType>,
}

/// Scan `.lmn` markup for signal references.
///
/// Recognized shapes (per the markup grammar):
///
/// - `bind-text="<name>"` / `bind-checked="<name>"` /
///   `bind-value="<name>"` - direct binds.
/// - `{<name>}` and `{$<name>}` - text interpolation. Bare `{name}`
///   gets a [`FindingKind::BareInterpolation`] info-level nudge when
///   `skip_bare_lint` is false; when true (the markup parser already
///   produced structured findings), we only collect refs.
fn scan_markup(
    src: &str,
    path: &Path,
    out: &mut Vec<Finding>,
    skip_bare_lint: bool,
) -> Vec<MarkupRef> {
    let mut refs = Vec::new();
    for (lineno, line) in src.lines().enumerate() {
        let lineno = lineno + 1;
        // bind-* attributes
        for attr in ["bind-text=", "bind-checked=", "bind-value="] {
            let mut search_idx = 0;
            while let Some(pos) = line[search_idx..].find(attr) {
                let start = search_idx + pos + attr.len();
                if let Some(name) = read_quoted(&line[start..]) {
                    let col = start + 1;
                    refs.push(MarkupRef {
                        signal: name,
                        line: lineno,
                        col,
                    });
                }
                search_idx = start;
            }
        }
        // {name} / {$name} interpolations inside text="..." or
        // bare text content. We walk the whole line - the bare-name
        // variant emits an Info finding (unless `skip_bare_lint` is
        // true, in which case the parser has already done so).
        scan_interpolations(line, lineno, path, out, &mut refs, skip_bare_lint);
    }
    refs
}

/// Walk a single line for `{...}` interpolation tokens; push refs and
/// (unless `skip_bare_lint`) emit BareInterpolation findings for the
/// no-`$` variant.
fn scan_interpolations(
    line: &str,
    lineno: usize,
    path: &Path,
    out: &mut Vec<Finding>,
    refs: &mut Vec<MarkupRef>,
    skip_bare_lint: bool,
) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // Find matching `}`.
            if let Some(end_off) = line[i + 1..].find('}') {
                let inner = &line[i + 1..i + 1 + end_off];
                let trimmed = inner.trim();
                // Skip Rhai-style `{` blocks (e.g. `{ block }`) by
                // requiring the content be a single ident-like token
                // (optionally prefixed by `$`).
                if is_interpolation_token(trimmed) {
                    let col = i + 1;
                    let (name, is_global) = if let Some(stripped) = trimmed.strip_prefix('$') {
                        (stripped.trim_start_matches("self.").to_string(), true)
                    } else {
                        (trimmed.to_string(), false)
                    };
                    if !is_global && !skip_bare_lint {
                        out.push(Finding {
                            file: path.to_path_buf(),
                            line: lineno,
                            col,
                            signal: name.clone(),
                            kind: FindingKind::BareInterpolation,
                            severity: Severity::Info,
                            message: format!(
                                "`{{{name}}}` reads the loop / template scope; use `{{${name}}}` for a global signal"
                            ),
                            suggestion: format!("write `{{${name}}}` (global) or `{{$self.{name}}}` (component scope)"),
                        });
                    }
                    refs.push(MarkupRef {
                        signal: name,
                        line: lineno,
                        col,
                    });
                }
                i = i + 1 + end_off + 1;
                continue;
            }
        }
        i += 1;
    }
}

/// Is `s` a single `[$]?ident(\.ident)*` token? Used to filter out
/// Rhai map-literal braces like `#{ k: v }` and CSS-token braces.
fn is_interpolation_token(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let s = s.strip_prefix('$').unwrap_or(s);
    if s.is_empty() {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// True when `name` is referenced as a template / loop field via
/// `<for each="..." key="...">`: those bare `{field}` tokens are fine.
/// We look for `each="<list>"` AND assume any `{<field>}` inside the
/// surrounding `<for>` is the per-row field, not a global signal.
/// Conservative: returns true if any `<for>` block in the source
/// mentions `name` as a field.
fn is_template_field(name: &str, lmn_src: &str) -> bool {
    // Cheap heuristic: if a `<for ...>` tag exists at all AND `name`
    // doesn't appear in any `bind-*=` attribute, we treat it as a
    // template field. Markup that names a global signal would
    // typically also bind it explicitly somewhere.
    if !lmn_src.contains("<for") {
        return false;
    }
    let binds = [
        format!("bind-text=\"{name}\""),
        format!("bind-checked=\"{name}\""),
        format!("bind-value=\"{name}\""),
    ];
    !binds.iter().any(|b| lmn_src.contains(b.as_str()))
}

/// The spelling this script's host writes a typed signal in.
///
/// candela carries the type on the handle (`signal<int>("count")`); rhai and
/// lua have no type arguments, so their handle is the chained
/// `signals.count` form. The suggestion names the one the author can paste.
fn typed_write_hint(path: &Path, name: &str, inferred: Option<&InferredType>) -> String {
    let candela = path.extension().is_some_and(|e| e == "cdl");
    let ty = match inferred {
        Some(InferredType::Int) => Some("int"),
        Some(InferredType::Float) => Some("float"),
        Some(InferredType::Bool) => Some("bool"),
        Some(InferredType::Color) => Some("Color"),
        Some(InferredType::Str) => Some("string"),
        _ => None,
    };
    match (candela, ty) {
        (true, Some(ty)) => format!("use `signal<{ty}>(\"{name}\").set(...)`"),
        (true, None) => format!(
            "use the typed handle `signal<int>(\"{name}\").set(...)` (or `float` / `bool` / `Color`) when the value fits a scalar"
        ),
        (false, Some(_)) => format!("use chained `signals.{name}.set(...)`"),
        (false, None) => {
            format!("use chained `signals.{name}.set(...)` when the value fits a scalar")
        }
    }
}

/// The candela type argument a `signal<T>("name")` handle carries, as a
/// schema type. `any` and `string` both reach the string sink, which the
/// schema calls `Str`; an unknown argument is not classified.
fn handle_type_arg(arg: &str) -> Option<SignalType> {
    match arg {
        "int" => Some(SignalType::I64),
        "float" => Some(SignalType::F64),
        "bool" => Some(SignalType::Bool),
        "string" => Some(SignalType::Str),
        "Color" => Some(SignalType::Color),
        _ => None,
    }
}

/// One `signal<T>("name")` handle found in a script, and how the script uses
/// it.
struct TypedHandle {
    /// The signal the handle names.
    name: String,
    /// The type argument it carries, as written.
    arg: String,
    line: usize,
    col: usize,
    /// A `set(...)` reaches this handle.
    writes: bool,
    /// A `get(...)` reaches this handle.
    reads: bool,
}

/// Every `signal<T>("name")` handle in `src`, plus the bare `signal("name")`
/// form, whose unpinned type parameter is `any`.
///
/// A handle is used either straight away (`signal<int>("n").set(0)`) or
/// through a `let`, which is the form a read-modify-write takes:
///
/// ```candela
/// let clicks = signal<int>("clicks");
/// clicks.set(clicks.get() + 1);
/// ```
///
/// so a bound handle is credited with whichever of `.set(` / `.get(` the
/// binding's name reaches anywhere in the script. The scanner does not track
/// scopes, so two functions binding the same variable name share their uses.
fn typed_handles(src: &str) -> Vec<TypedHandle> {
    const CTOR: &str = "signal";
    let mut out = Vec::new();
    let mut idx = 0;
    while let Some(pos) = src[idx..].find(CTOR) {
        let abs = idx + pos;
        let Some((arg, name, after_close)) = read_handle(src, abs) else {
            idx = abs + CTOR.len();
            continue;
        };
        idx = after_close;
        let tail = src[after_close..].trim_start();
        let mut writes = tail.starts_with(".set(");
        let mut reads = tail.starts_with(".get(");
        if !writes && !reads {
            if let Some(binding) = let_binding_before(src, abs) {
                writes = src.contains(&format!("{binding}.set("));
                reads = src.contains(&format!("{binding}.get("));
            }
        }
        let (line, col) = line_col_of(src, abs);
        out.push(TypedHandle {
            name,
            arg,
            line,
            col,
            writes,
            reads,
        });
    }
    out
}

/// The variable a handle at `at` is bound to, for the `let x = signal<..>(..)`
/// form. `None` when the handle is not the value of a `let`.
fn let_binding_before(src: &str, at: usize) -> Option<String> {
    let head = src[..at].trim_end().strip_suffix('=')?.trim_end();
    let ident_start = head
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    let ident = &head[ident_start..];
    if ident.is_empty() {
        return None;
    }
    let before = head[..ident_start].trim_end();
    let kw = before.strip_suffix("let")?;
    if kw.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(ident.to_string())
}

/// Split a handle constructor at `at` (the offset of `signal`) into its type
/// argument, the signal name, and the offset just past the closing `)`, so the
/// caller can see which method the handle is used with.
///
/// Both spellings land here: `signal<int>("n")` carries its type argument, and
/// the bare `signal("n")` pins nothing, which candela reads as `any`. A call
/// with a second argument is the rhai / lua `signal(name, default)` builder and
/// is not a candela handle, so it is left to the scanner that owns it.
fn read_handle(src: &str, at: usize) -> Option<(String, String, usize)> {
    // `my_signal(...)` and `signal_array(...)` merely contain the word.
    if src[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let after_name = at + "signal".len();
    if src.get(after_name..)?.starts_with('(') {
        let args_at = after_name + 1;
        let name = read_quoted(src.get(args_at..)?)?;
        let close_paren = find_close_paren(src.get(args_at..)?)?;
        // One argument and one only: anything past the name is another form.
        if read_string_then_comma(src.get(args_at..)?).is_some() {
            return None;
        }
        return Some(("any".to_owned(), name, args_at + close_paren + 1));
    }
    if !src.get(after_name..)?.starts_with('<') {
        return None;
    }
    let after_angle = after_name + 1;
    let rest = src.get(after_angle..)?;
    let close = rest.find('>')?;
    let arg = rest[..close].trim();
    if arg.is_empty() || !arg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let after_arg = &rest[close + 1..];
    let open = after_arg.len() - after_arg.trim_start().len();
    if !after_arg[open..].starts_with('(') {
        return None;
    }
    let args_at = after_angle + close + 1 + open + 1;
    let name = read_quoted(src.get(args_at..)?)?;
    let close_paren = find_close_paren(src.get(args_at..)?)?;
    Some((arg.to_string(), name, args_at + close_paren + 1))
}

/// Scan `.rhai` source for signal writes. Returns the list and pushes
/// `UntypedWrite` / `SchemaMismatch` findings as it goes.
fn scan_script(
    src: &str,
    path: &Path,
    schema: &SignalsCfg,
    out: &mut Vec<Finding>,
) -> Vec<ScriptWrite> {
    let mut writes = Vec::new();
    // (prefix, optional-typed-variant)
    let prefixes: &[(&str, Option<SignalType>)] = &[
        ("signal_set_int(", Some(SignalType::I64)),
        ("signal_set_float(", Some(SignalType::F64)),
        ("signal_set_bool(", Some(SignalType::Bool)),
        ("signal_set_color(", Some(SignalType::Color)),
        ("signal_set(", None),
    ];
    for (prefix, typed) in prefixes {
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let abs = idx + pos;
            // Reject `signal_set(` matches that are actually the
            // longer typed variants - `signal_set_int(` will already
            // have been seen by the first iteration; we want to skip
            // them here.
            if *prefix == "signal_set(" {
                let after = abs + "signal_set".len();
                if src.as_bytes().get(after) == Some(&b'_') {
                    idx = abs + prefix.len();
                    continue;
                }
            }
            let after = abs + prefix.len();
            // First arg is the name string literal.
            let Some((name, value_start)) = read_string_then_comma(&src[after..]) else {
                idx = after;
                continue;
            };
            let (line, col) = line_col_of(src, abs);
            // Second arg - for the untyped `signal_set`, classify
            // its literal shape to suggest the right typed variant.
            let inferred = if typed.is_none() {
                Some(infer_value_type(&src[after + value_start..]))
            } else {
                None
            };
            // Emit UntypedWrite for the bare `signal_set(...)` form,
            // unless the schema declares this signal as `string`:
            // there is no `signal_set_string` to migrate to, so
            // `signal_set` already is the typed sink for a
            // string-declared signal.
            let declared_str = matches!(schema.fields.get(&name), Some(SignalType::Str));
            if typed.is_none() && !declared_str {
                let suggestion = typed_write_hint(path, &name, inferred.as_ref());
                out.push(Finding {
                    file: path.to_path_buf(),
                    line,
                    col,
                    signal: name.clone(),
                    kind: FindingKind::UntypedWrite,
                    severity: Severity::Warn,
                    message: format!(
                        "`signal_set(\"{name}\", ...)` reaches the string-typed sink and bypasses the typed PropertyStore variant"
                    ),
                    suggestion,
                });
            }
            // SchemaMismatch - when the schema declares a type AND
            // the typed variant disagrees (or the bare write's
            // inferred literal type disagrees).
            if let Some(declared) = schema.fields.get(&name) {
                let written = typed
                    .clone()
                    .or_else(|| inferred.as_ref().map(SignalType::from));
                if let Some(actual) = written {
                    if !types_compatible(declared, &actual) {
                        out.push(Finding {
                            file: path.to_path_buf(),
                            line,
                            col,
                            signal: name.clone(),
                            kind: FindingKind::SchemaMismatch,
                            severity: Severity::Error,
                            message: format!(
                                "signal `{name}` declared as `{}` in lumen.toml but written as `{}`",
                                type_name(declared),
                                type_name(&actual)
                            ),
                            suggestion: format!(
                                "either change the [signals] declaration to `{name} = \"{}\"` or fix the write to match the declared type",
                                type_name(&actual)
                            ),
                        });
                    }
                }
            }
            writes.push(ScriptWrite {
                name,
                line,
                col,
                typed: typed.clone(),
            });
            idx = after;
        }
    }
    // Also recognise the chained `signals.<name>.set(...)` style as a
    // write, so we don't false-positive UntrackedSignal on it. We
    // don't classify the inferred type for the chained form.
    {
        let prefix = "signals.";
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let abs = idx + pos;
            let after = abs + prefix.len();
            // Read ident until `.set(` or `.get(`.
            let rest = &src[after..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if end == 0 {
                idx = after;
                continue;
            }
            let name = &rest[..end];
            let after_name = &rest[end..];
            if after_name.starts_with(".set(") {
                let (line, col) = line_col_of(src, abs);
                writes.push(ScriptWrite {
                    name: name.to_string(),
                    line,
                    col,
                    typed: None,
                });
            }
            idx = after + end;
        }
    }
    // The candela handle `signal<int>("name").set(...)`: the type argument
    // is the declared write type, so a schema mismatch is caught on it the
    // same way it is on `signal_set_int(...)`.
    for handle in typed_handles(src) {
        if !handle.writes {
            continue;
        }
        let TypedHandle {
            name, line, col, ..
        } = handle;
        let written = handle_type_arg(&handle.arg);
        if let (Some(declared), Some(actual)) = (schema.fields.get(&name), written.as_ref()) {
            if !types_compatible(declared, actual) {
                out.push(Finding {
                    file: path.to_path_buf(),
                    line,
                    col,
                    signal: name.clone(),
                    kind: FindingKind::SchemaMismatch,
                    severity: Severity::Error,
                    message: format!(
                        "signal `{name}` declared as `{}` in lumen.toml but written as `{}`",
                        type_name(declared),
                        type_name(actual)
                    ),
                    suggestion: format!(
                        "either change the [signals] declaration to `{name} = \"{}\"` or fix the write to match the declared type",
                        type_name(actual)
                    ),
                });
            }
        }
        writes.push(ScriptWrite {
            name,
            line,
            col,
            typed: written,
        });
    }
    // And the `signal("name", default).set(...)` / `signal("name",
    // default).get()` builder - common in widget-garden /
    // todo. Treat the named handle as a write site, with no
    // inferred type.
    {
        let prefix = "signal(";
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let abs = idx + pos;
            let after = abs + prefix.len();
            if let Some((name, _)) = read_string_then_comma(&src[after..]) {
                let (line, col) = line_col_of(src, abs);
                writes.push(ScriptWrite {
                    name,
                    line,
                    col,
                    typed: None,
                });
            }
            idx = after;
        }
    }
    // `signal_array("name")` - count the named array as both a write
    // and a read.
    {
        let prefix = "signal_array(";
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let abs = idx + pos;
            let after = abs + prefix.len();
            if let Some(name) = read_quoted(&src[after..]) {
                let (line, col) = line_col_of(src, abs);
                writes.push(ScriptWrite {
                    name,
                    line,
                    col,
                    typed: None,
                });
            }
            idx = after;
        }
    }
    // `derive("name", deps, fn)` registers a computed signal - count
    // the name as a write so markup binds to it don't false-positive
    // as untracked.
    {
        let prefix = "derive(";
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let abs = idx + pos;
            let after = abs + prefix.len();
            if let Some((name, _)) = read_string_then_comma(&src[after..]) {
                let (line, col) = line_col_of(src, abs);
                writes.push(ScriptWrite {
                    name,
                    line,
                    col,
                    typed: None,
                });
            }
            idx = after;
        }
    }
    writes
}

/// All signal names read by the script (used to filter orphan-write
/// noise). Recognises `signal_get*`, `signals.<name>.get(...)`, and
/// `signal_array("name").all()`-style first-arg literals.
fn scan_script_reads(src: &str) -> BTreeSet<String> {
    let mut reads = BTreeSet::new();
    for prefix in [
        "signal_get(",
        "signal_get_int(",
        "signal_get_float(",
        "signal_get_bool(",
        "signal_get_color(",
        "signal_array(",
    ] {
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let after = idx + pos + prefix.len();
            if let Some(name) = read_quoted(&src[after..]) {
                reads.insert(name);
            }
            idx = after;
        }
    }
    // The candela handle `signal<int>("name").get()`.
    for handle in typed_handles(src) {
        if handle.reads {
            reads.insert(handle.name);
        }
    }
    // Chained `signals.<name>.get(...)`.
    {
        let prefix = "signals.";
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let abs = idx + pos;
            let after = abs + prefix.len();
            let rest = &src[after..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if end > 0 && rest[end..].starts_with(".get(") {
                reads.insert(rest[..end].to_string());
            }
            idx = after + end.max(1);
        }
    }
    // `signal("name", default).get(...)` and `signal_array("name").all()`
    // / `.push(...)` / `.len()` builder forms - count the name as a
    // read whenever a method call follows the builder.
    for prefix in ["signal(", "signal_array("] {
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let abs = idx + pos;
            let after = abs + prefix.len();
            if let Some((name, after_first_arg)) = read_string_then_comma(&src[after..])
                .or_else(|| read_quoted(&src[after..]).map(|n| (n, 0)))
            {
                // Find matching `)` at depth 0, then check the next
                // non-whitespace char for `.`.
                let tail = &src[after + after_first_arg..];
                if let Some(close) = find_close_paren(tail) {
                    let after_close = tail[close + 1..].trim_start();
                    if after_close.starts_with('.') {
                        reads.insert(name);
                    }
                }
            }
            idx = after;
        }
    }
    // `derive("name", ...)` produces a derived signal - count both as a
    // write (it lands a signal cell) and a read of its dep names.
    {
        let prefix = "derive(";
        let mut idx = 0;
        while let Some(pos) = src[idx..].find(prefix) {
            let abs = idx + pos;
            let after = abs + prefix.len();
            if let Some((name, _)) = read_string_then_comma(&src[after..]) {
                // Treat derive as both producing the named signal
                // (so it isn't flagged untracked when markup binds
                // to it) and as not orphaning the deps.
                reads.insert(name);
            }
            idx = after;
        }
    }
    reads
}

/// Find the offset of the matching close paren in `s`, assuming
/// depth=0 at start. Returns `None` if unbalanced. Ignores parens
/// inside double-quoted strings.
fn find_close_paren(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut quote = 0u8;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_str = true;
                quote = b;
            }
            b'(' => depth += 1,
            b')' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Inferred type bucket from a Rhai value literal.
#[derive(Debug, Clone, Copy)]
enum InferredType {
    Int,
    Float,
    Bool,
    Str,
    Color,
    Unknown,
}

impl From<&InferredType> for SignalType {
    fn from(i: &InferredType) -> Self {
        match i {
            InferredType::Int => SignalType::I64,
            InferredType::Float => SignalType::F64,
            InferredType::Bool => SignalType::Bool,
            InferredType::Str => SignalType::Str,
            InferredType::Color => SignalType::Color,
            InferredType::Unknown => SignalType::Str,
        }
    }
}

/// Look at the start of `s` (a value expression) and classify the
/// literal shape. Pure heuristic - anything beyond a leading literal
/// returns `Unknown`.
fn infer_value_type(s: &str) -> InferredType {
    let s = s.trim_start();
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return InferredType::Unknown;
    };
    if first == '"' || first == '\'' {
        // String literal - try to detect a color hex.
        let close = s[1..].find(first);
        if let Some(end) = close {
            let body = &s[1..1 + end];
            if (body.len() == 7 || body.len() == 9)
                && body.starts_with('#')
                && body[1..].chars().all(|c| c.is_ascii_hexdigit())
            {
                return InferredType::Color;
            }
        }
        return InferredType::Str;
    }
    if first == 't' && s.starts_with("true") {
        return InferredType::Bool;
    }
    if first == 'f' && s.starts_with("false") {
        return InferredType::Bool;
    }
    if first == '-' || first.is_ascii_digit() {
        // Number - look for `.` or `e` for float.
        let end = s
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == 'E'))
            .unwrap_or(s.len());
        let lit = &s[..end];
        if lit.contains('.') || lit.contains('e') || lit.contains('E') {
            return InferredType::Float;
        }
        return InferredType::Int;
    }
    InferredType::Unknown
}

/// Cheap type-name renderer for findings.
fn type_name(t: &SignalType) -> &'static str {
    match t {
        SignalType::I64 => "i64",
        SignalType::F64 => "f64",
        SignalType::Bool => "bool",
        SignalType::Str => "string",
        SignalType::Color => "color",
        SignalType::Vec2 => "vec2",
        SignalType::Array { .. } => "array",
        SignalType::Object { .. } => "object",
    }
}

/// Loose type compatibility - `I64` writes accept `F64` declared
/// (lossy upcast) and vice versa for the `signal_set` legacy
/// stringify path. `Str` is the catch-all for the untyped write
/// whose inferred shape is `Unknown`.
fn types_compatible(declared: &SignalType, written: &SignalType) -> bool {
    use SignalType::*;
    if declared == written {
        return true;
    }
    matches!(
        (declared, written),
        // numeric upcast / downcast - the legacy stringify path
        // happily round-trips i64 <-> f64.
        (I64, F64) | (F64, I64)
    )
}

/// Read a leading quoted string. Returns the body without the
/// quotes; `None` if `s` doesn't open with a quote.
fn read_quoted(s: &str) -> Option<String> {
    let s = s.trim_start();
    let mut chars = s.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &s[quote.len_utf8()..];
    let end = rest.find(quote)?;
    let body = &rest[..end];
    if body.is_empty() || body.contains('\n') {
        return None;
    }
    Some(body.to_string())
}

/// Read a leading quoted string and return `(body, offset_after_comma)`.
/// `offset_after_comma` is the index past the trailing `,` in the
/// original slice, so the caller can index back into it to inspect
/// the value argument.
fn read_string_then_comma(s: &str) -> Option<(String, usize)> {
    let trimmed = s.trim_start();
    let lead_skip = s.len() - trimmed.len();
    let mut chars = trimmed.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &trimmed[quote.len_utf8()..];
    let end = rest.find(quote)?;
    let body = &rest[..end];
    if body.is_empty() || body.contains('\n') {
        return None;
    }
    let after_quote = lead_skip + quote.len_utf8() + end + quote.len_utf8();
    // Walk past whitespace and a single `,`.
    let after = &s[after_quote..];
    let trimmed_after = after.trim_start();
    let extra = after.len() - trimmed_after.len();
    if !trimmed_after.starts_with(',') {
        return None;
    }
    Some((body.to_string(), after_quote + extra + 1))
}

/// Convert a byte offset in `src` to a 1-based (line, col).
fn line_col_of(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, c) in src.char_indices() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Print the findings to stdout per the requested mode and return
/// the appropriate exit code.
fn emit(findings: &[Finding], as_json: bool, strict: bool) -> ExitCode {
    if as_json {
        let arr: Vec<_> = findings
            .iter()
            .map(|f| {
                json!({
                    "file": f.file.display().to_string(),
                    "line": f.line,
                    "col": f.col,
                    "signal": f.signal,
                    "kind": <&'static str>::from(f.kind),
                    "severity": effective_severity_str(f.severity, strict),
                    "message": f.message,
                    "suggestion": f.suggestion,
                })
            })
            .collect();
        let summary = summarize(findings, strict);
        let body = json!({
            "findings": arr,
            "summary": summary,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into())
        );
    } else {
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        for f in findings {
            *counts
                .entry(effective_severity_str(f.severity, strict))
                .or_default() += 1;
            println!(
                "{sev:<5} {file}:{line}:{col}  [{kind}] {sig}: {msg}",
                sev = effective_severity_str(f.severity, strict),
                file = f.file.display(),
                line = f.line,
                col = f.col,
                kind = <&'static str>::from(f.kind),
                sig = f.signal,
                msg = f.message,
            );
            if !f.suggestion.is_empty() {
                println!("       hint: {}", f.suggestion);
            }
        }
        if findings.is_empty() {
            println!("# no findings");
        } else {
            let parts: Vec<String> = counts.iter().map(|(k, v)| format!("{v} {k}")).collect();
            println!("# {} finding(s): {}", findings.len(), parts.join(", "));
        }
    }
    let has_error = findings.iter().any(|f| {
        let eff = effective_severity_str(f.severity, strict);
        eff == "error"
    });
    if has_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Strict mode upgrades `warn` -> `error`. `info` / `hint` stay as-is.
fn effective_severity_str(s: Severity, strict: bool) -> &'static str {
    if strict && s == Severity::Warn {
        "error"
    } else {
        s.into()
    }
}

fn summarize(findings: &[Finding], strict: bool) -> HashMap<String, usize> {
    let mut out: HashMap<String, usize> = HashMap::new();
    for f in findings {
        *out.entry(effective_severity_str(f.severity, strict).to_string())
            .or_default() += 1;
    }
    *out.entry("total".to_string()).or_default() = findings.len();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn schema_with(entries: &[(&str, SignalType)]) -> SignalsCfg {
        let mut m = HashMap::new();
        for (k, v) in entries {
            m.insert((*k).to_string(), v.clone());
        }
        SignalsCfg { fields: m }
    }

    #[test]
    fn untyped_write_flagged() {
        let schema = SignalsCfg::default();
        let lmn = r#"<root><label bind-text="count" /></root>"#;
        let rhai = r#"
            fn on_start() {
                signal_set("count", "5");
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            rhai,
            &PathBuf::from("main.rhai"),
        );
        let untyped: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::UntypedWrite)
            .collect();
        assert_eq!(
            untyped.len(),
            1,
            "expected one untyped-write, got {findings:?}"
        );
        assert_eq!(untyped[0].signal, "count");
        assert!(
            untyped[0].suggestion.contains("signals.count.set"),
            "a rhai script is pointed at the chained handle: {}",
            untyped[0].suggestion
        );
    }

    /// A candela script is pointed at the handle that carries the type, not
    /// at a setter that spells it.
    #[test]
    fn untyped_write_in_candela_suggests_the_typed_handle() {
        let schema = SignalsCfg::default();
        let lmn = r#"<root><label bind-text="count" /></root>"#;
        let cdl = r#"
            fn on_start() {
                lumen::signal_set("count", 5);
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            cdl,
            &PathBuf::from("main.cdl"),
        );
        let untyped: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::UntypedWrite)
            .collect();
        assert_eq!(
            untyped.len(),
            1,
            "expected one untyped-write, got {findings:?}"
        );
        assert!(
            untyped[0]
                .suggestion
                .contains("signal<int>(\"count\").set(...)"),
            "got {}",
            untyped[0].suggestion
        );
    }

    /// The candela handle is a typed write: it neither warns as untyped nor
    /// reads as an untracked signal.
    #[test]
    fn typed_handle_write_clean() {
        let schema = schema_with(&[("count", SignalType::I64)]);
        let lmn = r#"<root><label bind-text="count" /></root>"#;
        let cdl = r#"
            fn on_start() {
                signal<int>("count").set(0);
            }
            fn bump(ev) {
                let clicks = signal<int>("count");
                clicks.set(clicks.get() + 1);
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            cdl,
            &PathBuf::from("main.cdl"),
        );
        assert!(
            findings.is_empty(),
            "expected no findings on a typed handle write, got {findings:?}"
        );
    }

    /// A handle with no type argument is the `any` one, so it is a write like
    /// any other handle and the markup that binds it is not an untracked
    /// signal.
    #[test]
    fn a_handle_with_no_type_argument_is_a_write() {
        let schema = SignalsCfg::default();
        let lmn = r#"<root><label bind-text="count" /></root>"#;
        let cdl = r#"
            fn on_start() {
                signal("count").set("0");
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            cdl,
            &PathBuf::from("main.cdl"),
        );
        assert!(
            !findings
                .iter()
                .any(|f| f.kind == FindingKind::UntrackedSignal),
            "the bare handle is the write the binding wanted, got {findings:?}"
        );
    }

    /// A handle bound to a `let` is the read-modify-write form, so its read
    /// counts: the signal is not an orphan write.
    #[test]
    fn a_bound_handle_counts_as_a_read() {
        let schema = SignalsCfg::default();
        let lmn = r#"<root><button id="bump">Bump</button></root>"#;
        let cdl = r#"
            fn on_bump(ev) {
                let clicks = signal<int>("clicks");
                clicks.set(clicks.get() + 1);
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            cdl,
            &PathBuf::from("main.cdl"),
        );
        assert!(
            !findings.iter().any(|f| f.kind == FindingKind::OrphanWrite),
            "a handle read through its binding is not an orphan write, got {findings:?}"
        );
    }

    /// The type argument is what the schema is checked against, so a
    /// `signal<string>` write to an `i64` cell is the same error a
    /// `signal_set("count", "hello")` write is.
    #[test]
    fn typed_handle_schema_mismatch_flagged() {
        let schema = schema_with(&[("count", SignalType::I64)]);
        let lmn = r#"<root><label bind-text="count" /></root>"#;
        let cdl = r#"
            fn on_start() {
                signal<string>("count").set("hello");
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            cdl,
            &PathBuf::from("main.cdl"),
        );
        let mismatches: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::SchemaMismatch)
            .collect();
        assert_eq!(
            mismatches.len(),
            1,
            "expected one schema-mismatch, got {findings:?}"
        );
        assert_eq!(mismatches[0].signal, "count");
        assert_eq!(mismatches[0].severity, Severity::Error);
    }

    #[test]
    fn schema_mismatch_flagged() {
        let schema = schema_with(&[("count", SignalType::I64)]);
        let lmn = r#"<root><label bind-text="count" /></root>"#;
        let rhai = r#"
            fn on_start() {
                signal_set("count", "hello");
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            rhai,
            &PathBuf::from("main.rhai"),
        );
        let mismatches: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::SchemaMismatch)
            .collect();
        assert_eq!(
            mismatches.len(),
            1,
            "expected one schema-mismatch, got {findings:?}"
        );
        assert_eq!(mismatches[0].severity, Severity::Error);
        assert_eq!(mismatches[0].signal, "count");
    }

    #[test]
    fn bare_interpolation_warned() {
        let schema = schema_with(&[("count", SignalType::I64)]);
        let lmn = r#"<root><label text="{count}" /></root>"#;
        let rhai = r#"
            fn on_start() { signal_set_int("count", 0); }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            rhai,
            &PathBuf::from("main.rhai"),
        );
        let bare: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::BareInterpolation)
            .collect();
        assert_eq!(
            bare.len(),
            1,
            "expected one bare interpolation, got {findings:?}"
        );
        assert_eq!(bare[0].severity, Severity::Info);
    }

    #[test]
    fn untracked_signal_warned() {
        let schema = SignalsCfg::default();
        let lmn = r#"<root><label bind-text="theme" /></root>"#;
        let rhai = r#""#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            rhai,
            &PathBuf::from("main.rhai"),
        );
        let untracked: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::UntrackedSignal)
            .collect();
        assert_eq!(
            untracked.len(),
            1,
            "expected one untracked signal, got {findings:?}"
        );
        assert_eq!(untracked[0].signal, "theme");
        assert_eq!(untracked[0].severity, Severity::Warn);
    }

    #[test]
    fn chained_typed_write_clean() {
        let schema = schema_with(&[("count", SignalType::I64)]);
        let lmn = r#"<root><label bind-text="count" /></root>"#;
        let rhai = r#"
            fn on_start() {
                signals.count.set(5);
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            rhai,
            &PathBuf::from("main.rhai"),
        );
        assert!(
            findings.is_empty(),
            "expected no findings on typed chained write, got {findings:?}"
        );
    }

    #[test]
    fn typed_setter_does_not_emit_untyped_warning() {
        let schema = SignalsCfg::default();
        let lmn = r#"<root><label bind-text="count" /></root>"#;
        let rhai = r#"
            fn on_start() {
                signal_set_int("count", 5);
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            rhai,
            &PathBuf::from("main.rhai"),
        );
        let untyped: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::UntypedWrite)
            .collect();
        assert!(
            untyped.is_empty(),
            "typed setter should not be flagged untyped"
        );
    }

    #[test]
    fn schema_string_write_does_not_emit_untyped_warning() {
        let schema = schema_with(&[("theme", SignalType::Str)]);
        let lmn = r#"<root><label bind-text="theme" /></root>"#;
        let rhai = r#"
            fn on_start() {
                signal_set("theme", "dark");
            }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            rhai,
            &PathBuf::from("main.rhai"),
        );
        let untyped: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::UntypedWrite)
            .collect();
        assert!(
            untyped.is_empty(),
            "signal_set on a string-declared signal should not be flagged untyped, got {findings:?}"
        );
    }

    #[test]
    fn dollar_interpolation_clean() {
        let schema = schema_with(&[("count", SignalType::I64)]);
        let lmn = r#"<root><label text="{$count}" /></root>"#;
        let rhai = r#"
            fn on_start() { signal_set_int("count", 0); }
        "#;
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            rhai,
            &PathBuf::from("main.rhai"),
        );
        let bare: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::BareInterpolation)
            .collect();
        assert!(bare.is_empty(), "$-prefixed interpolation should not warn");
    }

    #[test]
    fn schema_mismatch_int_vs_string_literal() {
        let schema = schema_with(&[("count", SignalType::I64)]);
        let rhai = r#"signal_set("count", 5);"#;
        let lmn = "";
        let findings = analyze(
            &schema,
            lmn,
            &PathBuf::from("main.lmn"),
            rhai,
            &PathBuf::from("main.rhai"),
        );
        // int literal is compatible with declared i64 - no mismatch.
        let mismatches: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == FindingKind::SchemaMismatch)
            .collect();
        assert!(
            mismatches.is_empty(),
            "int literal should match i64 schema; got {findings:?}"
        );
    }
}
