//! Writing a page's element tree as HTML.
//!
//! The tree is written with no whitespace of its own. An element's text is
//! the one text node inside it, so an indented document would put text
//! where the markup had none, and the browser runtime that adopts the
//! document would find a node the app does not have.
//!
//! A `<for>` block's children are the row template, never content. What goes
//! inside it is one instance of that template per row of the array signal the
//! page is rendered with, with the row's own values substituted in.
//!
//! An element is written with the values its `bind-*` attributes hold in that
//! same state, which [`crate::bindings`] resolves; what the state answers
//! nothing for keeps the fallback the markup carries.
//!
//! A `{name}` placeholder is resolved the same way, through the resolver both
//! halves of the web target share, so the page carries the value rather than
//! the braces and the runtime that builds the same element arrives at the same
//! string.
//!
//! Text carrying a `format` is written for the locale this tree is emitted
//! in, the way `translatable` text is: the document a browser loads already
//! says the formatted string, and nothing re-formats it there.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use lumen_html::contract::{
    DATA_LM, DATA_LM_HIDDEN, DATA_LM_SELECTED, DIALOG_OPEN, NodePath, NodeSeed,
};
use lumen_html::style::{Emission, rewrite_property, style_value};
use lumen_html::{escape_attr, escape_text, html_attrs, html_tag_for};
use lumen_i18n::{LanguageIdentifier, LocaleFormatter};
use lumen_ir::css::computed_style_map;
use lumen_ir::fragment::FRAGMENT_TAG;
use lumen_ir::interpolate::{Scope, substitute_attrs, substitute_element};
use lumen_ir::layout_ir::{Attributes, Element, IfModeSpec};

use crate::error::EmitError;
use crate::snapshot::NodeState;
use crate::spec::{CssMode, PageSpec, RowFills, SignalEnv, SiteSpec};
use crate::{bindings, urls};

/// What the walk needs to know that is not the element itself.
struct Walk<'a> {
    page: &'a str,
    signals: &'a SignalEnv,
    /// Whether the walk is inside a `<for>` row.
    ///
    /// A row is written from an instance the row walk already resolved
    /// against the row record and the globals together, so there is nothing
    /// left for this walk to resolve there. Resolving again would read a
    /// value that arrived from a row field as a placeholder of its own.
    in_row: bool,
    /// What the components inside this page's `<for>` rows rendered, by node
    /// path. Read only inside a row: outside one the tree already carries the
    /// body, because the build inlined it there.
    fills: &'a RowFills,
    /// Whether the block being emitted lost its fills, because the state the
    /// page is written with is not the state they came from.
    fills_dropped: bool,
    /// Row components already reported as unfilled, so a block of forty rows
    /// says it once.
    unfilled: BTreeSet<String>,
    css_mode: CssMode,
    /// Site base path, which is what the shared files hang off.
    base: String,
    /// Base path of this locale's documents, which a link hangs off.
    tree: String,
    /// Page keys, longest first, for resolving a link.
    keys: Vec<String>,
    entry: &'a str,
    seen: BTreeSet<String>,
    /// What the app wrote onto each node while it ran, by node path.
    nodes: &'a BTreeMap<String, NodeState>,
    /// The difference between that and what the markup says, which is what
    /// the document carries so the runtime does not write the markup's own
    /// values back over it on the first tick.
    seeded: BTreeMap<String, NodeSeed>,
    /// True once a node path was found to name a different node in each
    /// half, after which no override is written: every path from there on
    /// belongs to a different node in the run than in the tree.
    diverged: bool,
    /// The locale this tree is emitted in, which is the locale a `format`
    /// renders for.
    locale: &'a str,
    /// The formatters for that locale, built the first time an element
    /// asks for them. A page that formats nothing loads no ICU data.
    formatter: &'a OnceCell<LocaleFormatter>,
    /// Where the page could not be written the way the app meant it.
    warnings: &'a mut Vec<String>,
}

impl<'a> Walk<'a> {
    /// The locale's formatters. A locale tag that does not parse formats
    /// as `en-US`, which is the locale `LocaleFormatter` itself falls back
    /// to when its data will not load.
    fn formatter(&self) -> &'a LocaleFormatter {
        self.formatter.get_or_init(|| {
            let lang: LanguageIdentifier = self
                .locale
                .parse()
                .unwrap_or_else(|_| "en-US".parse().expect("en-US is valid"));
            LocaleFormatter::new(lang)
        })
    }
}

/// Write the page's element tree, starting at the page root, and the node
/// seed that goes with it.
pub fn emit_tree(
    page: &PageSpec,
    spec: &SiteSpec,
    warnings: &mut Vec<String>,
) -> Result<(String, BTreeMap<String, NodeSeed>), EmitError> {
    let mut out = String::new();
    let base = urls::normalize_base(&spec.web.base_path);
    let formatter = OnceCell::new();
    let mut walk = Walk {
        page: &page.key,
        signals: &page.signals,
        in_row: false,
        fills: &page.fills,
        fills_dropped: false,
        unfilled: BTreeSet::new(),
        css_mode: spec.web.css_mode,
        tree: urls::join(&base, &spec.locale.prefix()),
        base,
        keys: spec.keys(),
        entry: &spec.web.entry,
        seen: BTreeSet::new(),
        nodes: &page.nodes,
        seeded: BTreeMap::new(),
        diverged: false,
        locale: &spec.locale.locale,
        formatter: &formatter,
        warnings,
    };
    emit_element(&mut out, &page.ir.root, &NodePath::root(), &mut walk)?;
    Ok((out, walk.seeded))
}

fn emit_element(
    out: &mut String,
    element: &Element,
    path: &NodePath,
    walk: &mut Walk<'_>,
) -> Result<(), EmitError> {
    // A component whose body the build could stand in for is already the body
    // by the time the tree gets here. What is left carrying a use site is a
    // component that has to run, and the element is the marker the runtime
    // replaces with what the call returns. Inside a `<for>` row the build read
    // what that call produced and it goes in below; anywhere else the node is
    // not knowable here, so the marker is written as the empty box it is. What
    // the use site wrote inside it goes with the marker when the replacement
    // lands, so writing that would put content in the page the app never has.
    let marker = element.frag_use.is_some();
    let path_text = path.to_string();
    // Inside a row the marker is not the last word: what the component
    // rendered for this row was read off the app that ran it, and the body
    // goes where the box would have. The walk continues into the body at the
    // marker's own path, so the document numbers its nodes the way the
    // runtime numbers the subtree it builds there.
    if let Some(use_site) = &element.frag_use
        && walk.in_row
        && !walk.fills_dropped
    {
        let fills = walk.fills;
        if let Some(body) = fills.body(&path_text) {
            return emit_element(out, body, path, walk);
        }
        let name = use_site.key.clone();
        if walk.unfilled.insert(name.clone()) {
            walk.warnings.push(format!(
                "page `{}`: `{name}` is written inside a `<for>` and the build read no body for \
                 it, so the page carries an empty box the browser fills on load",
                walk.page
            ));
        }
    }
    let ir_tag = if marker {
        FRAGMENT_TAG
    } else {
        element.tag.as_str()
    };
    let tag = html_tag_for(ir_tag).ok_or_else(|| EmitError::UnknownTag {
        page: walk.page.to_string(),
        tag: element.tag.clone(),
    })?;
    if !walk.seen.insert(path_text.clone()) {
        return Err(EmitError::DuplicateNodePath {
            page: walk.page.to_string(),
            path: path_text,
        });
    }
    // A `{name}` in the markup names a global signal, and the page is written
    // with the value the state holds for it, the same way the browser reads it
    // when it builds the same element. A name the state has nothing for keeps
    // its braces, so an authoring typo reads as one.
    let filled = if walk.in_row {
        None
    } else {
        substitute_attrs(
            &element.attrs,
            &element.interpolations,
            &Scope::new(walk.signals),
        )
    };
    let own = filled.as_ref().unwrap_or(&element.attrs);
    // What the element's bindings hold in the state this page is rendered
    // with. An element whose bindings the state answers nothing for is emitted
    // from its own attributes, which is what leaves the authored fallback in
    // the page.
    let formatter = own.format.as_ref().map(|_| walk.formatter());
    let bound = bindings::resolved(ir_tag, own, walk.signals, formatter);
    let resolved = bound.as_ref().unwrap_or(own);
    // What the app wrote onto this node while it ran. The markup does not
    // say it, so the document carries it, and the seed says the document did.
    let written = node_overrides(element, resolved, &path_text, walk);
    let attrs = written.attrs.as_ref().unwrap_or(resolved);

    let mut hidden = false;
    let mut open = false;
    let mut children_are_content = !marker;
    match ir_tag {
        "if" => match element.attrs.if_mode {
            // A hidden branch stays in the document, so the runtime has
            // something to show when the signal turns true.
            IfModeSpec::Hide => hidden = !branch_taken(element, walk.signals),
            IfModeSpec::Render => children_are_content = branch_taken(element, walk.signals),
        },
        // `<dialog open="signal">` is an `<if mode="hide">` that is also a
        // real dialog: `open` names the signal, not the state. The state is
        // what a browser reads, so it is resolved here. A dialog with no
        // signal is always showing, which is what it does on the desktop.
        // Whether it shows is the `open` attribute alone; a browser hides a
        // closed dialog itself and the reset makes that rule one an author
        // sheet cannot outrank.
        "dialog" => {
            open = element
                .attrs
                .if_signal
                .as_ref()
                .is_none_or(|_| branch_taken(element, walk.signals));
        }
        "for" => children_are_content = false,
        _ => {}
    }

    out.push('<');
    out.push_str(tag.name);
    for (name, value) in tag.fixed {
        write_attr(out, name, value);
    }
    for (name, value) in html_attrs(ir_tag, attrs) {
        // A link and an asset reference are written as the IR holds them,
        // which is a page key and a path relative to the site root. Both
        // become URLs here, where the site's base path and page set are
        // known.
        match name {
            "href" => write_attr(
                out,
                name,
                &urls::page_href(&value, &walk.tree, &walk.keys, walk.entry),
            ),
            "src" => write_attr(out, name, &urls::asset_src(&value, &walk.base)),
            _ => write_attr(out, name, &value),
        }
    }
    for (name, value) in &written.extra {
        write_attr(out, name, value);
    }
    write_attr(out, DATA_LM, &path_text);
    let mut style = if walk.css_mode == CssMode::Computed {
        computed_style(attrs)
    } else {
        String::new()
    };
    if !written.style.is_empty() {
        if !style.is_empty() {
            style.push(';');
        }
        style.push_str(&style_value(&written.style));
    }
    if !style.is_empty() {
        write_attr(out, "style", &style);
    }
    if open {
        write_attr(out, DIALOG_OPEN, "");
    }
    // Which tab is current is a signal, and the strip button that matches it
    // is the one Lumen calls `:selected`. The runtime maintains this mark; the
    // page needs it too, or the current tab is unmarked until the runtime
    // loads and unmarked forever without it.
    if let Some((signal, value)) = &attrs.tab_strip
        && walk.signals.global(signal) == Some(value.as_str())
    {
        write_attr(out, DATA_LM_SELECTED, "");
    }
    if hidden {
        write_attr(out, DATA_LM_HIDDEN, "");
    }
    out.push('>');

    if tag.void {
        return Ok(());
    }
    if let Some(text) = &attrs.text
        && !text.is_empty()
        && !marker
    {
        out.push_str(&escape_text(text));
    }
    if children_are_content {
        for (index, child) in element.children.iter().enumerate() {
            emit_element(out, child, &path.child(index as u32), walk)?;
        }
    } else if ir_tag == "for" {
        emit_rows(out, element, path, walk)?;
    }
    out.push_str("</");
    out.push_str(tag.name);
    out.push('>');
    Ok(())
}

/// What one node wears that its markup does not say.
#[derive(Default)]
struct Overrides {
    /// The element's attributes with the app's class list and text in them,
    /// when either differs from the markup's.
    attrs: Option<Attributes>,
    /// Attributes with no place in the IR bag: `role`, `aria-*`, and
    /// whatever else a script set by name.
    extra: BTreeMap<String, String>,
    /// The inline style the app set.
    style: Vec<(String, String)>,
}

/// The difference between what the app wrote onto a node and what the markup
/// says, recorded in the page's seed on the way past.
///
/// A path that names one node in the run and another in the tree means the
/// app changed the shape of the tree while it ran, which renumbers every
/// sibling after the change. Nothing later can be trusted to name the same
/// node in both halves, so the overrides stop there and the page is written
/// from its markup alone.
fn node_overrides(
    element: &Element,
    attrs: &Attributes,
    path: &str,
    walk: &mut Walk<'_>,
) -> Overrides {
    if walk.diverged {
        return Overrides::default();
    }
    let Some(record) = walk.nodes.get(path) else {
        return Overrides::default();
    };
    if record.tag != element.tag {
        walk.diverged = true;
        walk.warnings.push(format!(
            "page `{}`: the app changed the shape of the tree while it ran, so what it wrote \
             onto nodes is not written into the document",
            walk.page
        ));
        return Overrides::default();
    }

    let mut over = Overrides::default();
    let mut seed = NodeSeed::default();
    let mut own = attrs.clone();
    let mut replaced = false;
    if record.classes != attrs.classes {
        seed.classes = Some(record.classes.clone());
        own.classes = record.classes.clone();
        replaced = true;
    }
    // A translatable element keeps the text the catalogue gave it. The app
    // runs once and every locale is emitted from that one run, so its text
    // is the default locale's and writing it here would put that string in
    // every other locale's document.
    //
    // A node the run holds no text for is left alone rather than emptied: an
    // element whose text lives somewhere other than a text node, such as a
    // form control's value, has none to record.
    if let Some(text) = &record.text
        && element.attrs.translatable.is_none()
        && text != attrs.text.as_deref().unwrap_or_default()
    {
        seed.text = Some(text.clone());
        own.text = Some(text.clone());
        replaced = true;
    }
    // The spawner writes neither of these, so whatever a node carries after a
    // run is the app's, whole.
    if !record.attrs.is_empty() {
        seed.attrs = record.attrs.clone();
        over.extra = record.attrs.clone();
    }
    if !record.style.is_empty() {
        seed.style = record.style.clone();
        over.style = record.style.clone();
    }
    if replaced {
        over.attrs = Some(own);
    }
    if !seed.is_empty() {
        walk.seeded.insert(path.to_string(), seed);
    }
    over
}

/// Write one instance of a `<for>` block's row template per row of the array
/// it iterates.
///
/// A row element's identity is its FLAT position in the block's child list,
/// not the row number: the reconciler spawns one entity per template element
/// per row as flat siblings, and the runtime numbers those siblings by
/// position when it looks for the element each one belongs to. A block whose
/// template is two elements therefore starts its second row at slot 2.
///
/// The rows are not put through the cascade again. The browser has its own
/// over the same stylesheet, which is why the reconciler leaves a row
/// unresolved in a page too.
fn emit_rows(
    out: &mut String,
    element: &Element,
    path: &NodePath,
    walk: &mut Walk<'_>,
) -> Result<(), EmitError> {
    let Some(name) = &element.attrs.each else {
        return Ok(());
    };
    // The signal environment outlives the walk, so reading the rows out of it
    // does not stand in the way of writing the document.
    let signals = walk.signals;
    let Some(rows) = signals.rows(name).filter(|rows| !rows.is_empty()) else {
        return Ok(());
    };
    let body = &element.children;
    if body.is_empty() {
        return Ok(());
    }
    // Which rows a virtualized block mounts comes from the offset of the
    // `<scroll>` it sits in, which a build machine cannot know. A guessed
    // prefix would be markup the runtime takes straight back out.
    if element.attrs.virtualized {
        walk.warnings.push(format!(
            "page `{}`: the virtualized `<for each=\"{name}\">` is emitted with no rows, \
             because which rows are in view is not known until the page is scrolled",
            walk.page
        ));
        return Ok(());
    }

    // The bodies were read off an app holding one list; a page written from a
    // different one would put a card built for a row it does not show. Only
    // reachable under `prerender = "seeds"`, where the pages are written with
    // the declared state and the app may have rewritten the list as it
    // settled.
    let mismatch = match walk.fills.block(&path.to_string()) {
        Some((array, count)) => array != name.as_str() || count != rows.len(),
        None => false,
    };
    if mismatch && !walk.fills_dropped {
        walk.warnings.push(format!(
            "page `{}`: `<for each=\"{name}\">` is written with {} rows and the build read the \
             components of a different list, so its rows carry empty boxes the browser fills on \
             load",
            walk.page,
            rows.len()
        ));
    }
    let dropped = walk.fills_dropped;
    walk.fills_dropped = dropped || mismatch;

    let missing: RefCell<BTreeSet<String>> = RefCell::new(BTreeSet::new());
    let report = |field: &str| {
        missing.borrow_mut().insert(field.to_string());
    };
    let outside = std::mem::replace(&mut walk.in_row, true);
    for (index, item) in rows.iter().enumerate() {
        let scope = Scope::new(signals)
            .with_row(item, index)
            .reporting_to(&report);
        for (offset, template) in body.iter().enumerate() {
            let slot = index * body.len() + offset;
            let instance = substitute_element(template, &scope);
            emit_element(out, &instance, &path.row(slot as u32), walk)?;
        }
    }
    walk.in_row = outside;
    walk.fills_dropped = dropped;
    for field in missing.into_inner() {
        walk.warnings.push(format!(
            "page `{}`: `<for each=\"{name}\">` reads row field `{field}`, which its records do \
             not carry; it renders empty",
            walk.page
        ));
    }
    Ok(())
}

/// What Lumen's cascade resolved for this element, as an inline style.
///
/// Only what a browser can be told this way survives: a value that stands
/// for a state (a hover fill) or for a knob with no CSS property behind it
/// has nowhere to land on an element.
fn computed_style(attrs: &Attributes) -> String {
    let mut out = String::new();
    for (name, value) in computed_style_map(attrs) {
        let Emission::Plain(decls) = rewrite_property(&name, &value) else {
            continue;
        };
        for decl in decls {
            if !out.is_empty() {
                out.push(';');
            }
            out.push_str(&decl.name);
            out.push(':');
            out.push_str(&decl.value);
        }
    }
    out
}

/// Whether an `<if>` block's condition holds in the state being rendered.
///
/// With `eq` the signal has to equal that value; without it, any truthy
/// value will do. This is the rule the desktop reconciler applies.
fn branch_taken(element: &Element, signals: &SignalEnv) -> bool {
    let Some(signal) = &element.attrs.if_signal else {
        return false;
    };
    match &element.attrs.if_eq {
        Some(expected) => signals.global(signal) == Some(expected.as_str()),
        None => signals.is_truthy(signal),
    }
}

fn write_attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    out.push_str(&escape_attr(value));
    out.push('"');
}
