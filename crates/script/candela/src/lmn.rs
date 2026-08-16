//! The `lmn!` contract: what a markup block in a candela script means.
//!
//! A candela function returns markup by writing `lmn!( ... )`. The block is
//! declarative: tags, attributes, `$name` interpolation, and elements naming
//! another candela function. Everything else is candela around it.
//!
//! Two readers need the same answers about one block. The macro expander runs
//! inside the candela compiler and turns the block into the call that
//! instantiates it. The ahead-of-time extractor runs in `lumenc` and turns the
//! same block into a [`Fragment`](lumen_ir::fragment::Fragment) the artifact
//! carries. They agree because both read this module: [`key_of`] decides the
//! fragment key, [`analyze`] decides which `$name` sites are arguments and
//! which elements are components, and [`slot_name`] decides where a component's
//! node lands.
//!
//! # What a block becomes
//!
//! ```text
//! fn Home(name) { return lmn!(<label text="home for $name"/>); }
//! fn App() { return lmn!(<column><Home name="bob"/></column>); }
//! ```
//!
//! `Home`'s block is a fragment with one parameter, `name`. `App`'s block is a
//! fragment whose body has a `<slot>` where `<Home/>` stood; the expansion
//! calls `Home("bob")` first and passes the node it returns in as that slot's
//! content, so the applier never calls back into the script.
//!
//! # Naming a component from markup
//!
//! A `.lmn` file writes `<Home name="bob"/>` too. Markup compiles with no
//! script host in the loop, so that use site instantiates the block `Home`
//! returns instead of calling `Home`. [`component_at`] decides which functions
//! markup may name: the name starts with a capital, and the body is one
//! `return lmn!(...)`, so there is a single block to stand in for the call.
//!
//! # Restrictions
//!
//! - A block has exactly one root element.
//! - `$name` reads a candela value once, when the instance is built. Something
//!   that changes while the app runs is a `bind-*` attribute inside the block.
//! - `{name}` keeps its markup meaning, a signal reference, and is not an
//!   argument. Write `$name` for an argument and `{name}` for a signal.
//! - A component element takes no markup children.
//! - Names this module generates start with [`RESERVED_PREFIX`].

use std::collections::BTreeMap;
use std::ops::Range;

/// The macro name a markup block is written under.
pub const MACRO_NAME: &str = "lmn";

/// Prefix of every name this module generates. Markup must not write one.
pub const RESERVED_PREFIX: &str = "lmn-";

/// How many hex characters a fragment key has.
pub const KEY_LEN: usize = 16;

/// The slot a body's `index`-th component element leaves behind.
#[must_use]
pub fn slot_name(index: usize) -> String {
    format!("{RESERVED_PREFIX}child-{index}")
}

/// Why a block is not a block.
///
/// `offset` is a byte offset into the block body, which both readers turn into
/// a position in the file: the expander hands it to candela as a
/// [`MacroError`](candela::macros::MacroError), and the extractor adds it to
/// the region's own start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmnError {
    /// What is wrong, in one sentence.
    pub message: String,
    /// Byte offset into the body the message is about.
    pub offset: usize,
}

impl LmnError {
    fn new(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset,
        }
    }
}

impl std::fmt::Display for LmnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LmnError {}

/// An element in a block that names a candela function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    /// The candela function the element calls.
    pub name: String,
    /// The slot its node fills in the enclosing fragment.
    pub slot: String,
    /// Props as written, in source order: attribute name and raw value.
    pub props: Vec<(String, String)>,
    /// Byte offset of the element in the body.
    pub offset: usize,
}

/// One block, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Lookup key of the fragment the block declares.
    pub key: String,
    /// The body as markup: `$name` rewritten to the marker a fragment
    /// parameter uses, and every component element replaced by its slot.
    pub markup: String,
    /// Argument names the body reads, in first-appearance order.
    pub args: Vec<String>,
    /// Component elements, in source order.
    pub components: Vec<Component>,
    /// Whether the body is one component element and nothing else, in which
    /// case the block is that component's call and declares no fragment.
    pub lone_component: bool,
}

/// Fold line endings and trim the outer whitespace, so the same markup written
/// on two machines keys the same.
#[must_use]
pub fn normalize(body: &str) -> String {
    body.replace("\r\n", "\n").trim().to_string()
}

/// The fragment key a body claims: a content hash of [`normalize`]'s output,
/// as [`KEY_LEN`] hex characters.
#[must_use]
pub fn key_of(body: &str) -> String {
    let normalized = normalize(body);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in normalized.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// One `lmn!( ... )` invocation in a candela source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmnRegion<'a> {
    /// The block body, the text between the parentheses.
    pub body: &'a str,
    /// Byte offset of `body` in the source it was scanned from.
    pub body_start: usize,
    /// Byte range of the whole invocation, `lmn!` through the closing
    /// parenthesis.
    pub span: Range<usize>,
}

/// Every `lmn!( ... )` invocation in a candela source, in source order.
///
/// The scan is candela's own, so a region written inside a string literal or a
/// comment is not one. It is also the compiler's, which makes this the one
/// item in the module a build without the compiler does not carry.
#[cfg(feature = "compiler")]
#[must_use]
pub fn regions(src: &str) -> Vec<LmnRegion<'_>> {
    candela::macros::scan_regions(src, MACRO_NAME)
        .into_iter()
        .map(|region| LmnRegion {
            body: region.body,
            body_start: region.body_start,
            span: region.span,
        })
        .collect()
}

/// A candela function markup may name as a tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFn {
    /// The function's name, as markup writes it.
    pub name: String,
    /// Whether markup can instantiate the block through the name.
    pub inlinable: bool,
}

/// The component the function around `span` declares, if markup may name it.
///
/// A candela function is a component when its name starts with a capital, the
/// way a component element in a block is spelled. Markup compiles with no
/// script host in the loop, so a use site instantiates the block the function
/// returns rather than calling the function, and
/// [`inlinable`](ComponentFn::inlinable) is set only for a body that is one
/// `return lmn!(...)`: the one shape with a single block to stand in for the
/// call, and the one where nothing the function would have run is dropped.
///
/// A function of any other shape still yields its name, with `inlinable`
/// clear, so a use site naming it can be told why rather than being told the
/// tag is unknown.
#[must_use]
pub fn component_at(src: &str, span: &Range<usize>, index: &FnIndex) -> Option<ComponentFn> {
    let (name, body) = index.enclosing(span.start)?;
    if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let before = src.get(body.start..span.start)?.trim();
    let after = src.get(span.end..body.end)?.trim();
    Some(ComponentFn {
        name: name.to_string(),
        inlinable: before == "return" && matches!(after, "" | ";"),
    })
}

/// Read one block.
///
/// # Errors
///
/// [`LmnError`] when the body writes a reserved name, gives a component
/// element markup children, or leaves a component element unclosed.
pub fn analyze(body: &str) -> Result<Block, LmnError> {
    let scan = scan(body)?;
    Ok(Block {
        key: key_of(body),
        markup: scan.markup,
        args: scan.args,
        lone_component: scan.lone_component,
        components: scan.components,
    })
}

/// The candela source one block expands to: a call that instantiates the
/// fragment, or, for a body that is one component element, that component's
/// own call.
///
/// The result is a single line. candela parses an expansion as its own
/// expression and reports every span it produces against the invocation, so
/// the file's line numbering is the reader's throughout.
///
/// # Errors
///
/// [`LmnError`] from [`analyze`], plus a component naming a function `index`
/// does not know or a prop naming a parameter that function does not declare.
pub fn expand(body: &str, index: &FnIndex) -> Result<String, LmnError> {
    let block = analyze(body)?;
    if block.lone_component {
        return call_of(&block.components[0], index);
    }
    let mut args = Vec::with_capacity(block.args.len() * 2);
    for name in &block.args {
        args.push(format!("\"{name}\""));
        args.push(format!("str({name})"));
    }
    let mut children = Vec::with_capacity(block.components.len());
    for component in &block.components {
        children.push(call_of(component, index)?);
    }
    Ok(format!(
        "lumen::fragment_spawn(\"{}\", [{}], [{}])",
        block.key,
        args.join(", "),
        children.join(", ")
    ))
}

/// The call one component element stands for. Props map to the function's
/// parameters by name; a parameter no prop names is passed the empty string,
/// which is what an omitted fragment argument resolves to.
fn call_of(component: &Component, index: &FnIndex) -> Result<String, LmnError> {
    let Some(params) = index.params(&component.name) else {
        return Err(LmnError::new(
            format!(
                "component <{}> names no candela function; declare `fn {}(...)` in this script",
                component.name, component.name
            ),
            component.offset,
        ));
    };
    for (prop, _) in &component.props {
        if !params.iter().any(|p| p == prop) {
            return Err(LmnError::new(
                format!(
                    "component <{}> has no parameter `{}`; it declares ({})",
                    component.name,
                    prop,
                    params.join(", ")
                ),
                component.offset,
            ));
        }
    }
    let args: Vec<String> = params
        .iter()
        .map(
            |param| match component.props.iter().find(|(p, _)| p == param) {
                Some((_, value)) => value_expr(value),
                None => "\"\"".to_string(),
            },
        )
        .collect();
    Ok(format!("{}({})", component.name, args.join(", ")))
}

/// A prop value as a candela expression. A value that is one `$name` and
/// nothing else passes that value through with its own type; anything else is
/// text, with each `$name` in it rendered by `str`.
fn value_expr(value: &str) -> String {
    let parts = split_refs(value);
    if let [Piece::Ref(name)] = parts.as_slice() {
        return name.clone();
    }
    if parts.is_empty() {
        return "\"\"".to_string();
    }
    parts
        .iter()
        .map(|piece| match piece {
            Piece::Text(text) => quote(text),
            Piece::Ref(name) => format!("str({name})"),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// A candela string literal holding `text`.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// A prop value split into literal text and `$name` references.
#[derive(Debug, PartialEq, Eq)]
enum Piece {
    Text(String),
    Ref(String),
}

fn split_refs(value: &str) -> Vec<Piece> {
    let bytes = value.as_bytes();
    let mut out: Vec<Piece> = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            if bytes.get(i + 1) == Some(&b'$') {
                text.push('$');
                i += 2;
                continue;
            }
            if let Some(end) = ident_end(bytes, i + 1) {
                if !text.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut text)));
                }
                out.push(Piece::Ref(value[i + 1..end].to_string()));
                i = end;
                continue;
            }
        }
        let ch = value[i..].chars().next().unwrap_or('\0');
        text.push(ch);
        i += ch.len_utf8();
    }
    if !text.is_empty() {
        out.push(Piece::Text(text));
    }
    out
}

/// End of the identifier starting at `start`, or `None` when nothing there
/// starts one.
fn ident_end(bytes: &[u8], start: usize) -> Option<usize> {
    let first = *bytes.get(start)?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut end = start + 1;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    Some(end)
}

/// What one pass over a body produces.
struct Scan {
    markup: String,
    args: Vec<String>,
    components: Vec<Component>,
    lone_component: bool,
}

/// Rewrite the body into markup and collect what the emission needs.
///
/// One pass. `$name` outside a `{...}` marker is an argument and becomes the
/// marker a fragment parameter uses; `$$` is a literal `$`. An element whose
/// tag starts with a capital is a component, and leaves a `<slot>` behind.
/// Comments pass through untouched, so nothing inside one is a site.
#[allow(clippy::too_many_lines)]
fn scan(body: &str) -> Result<Scan, LmnError> {
    if let Some(at) = body.find(RESERVED_PREFIX) {
        return Err(LmnError::new(
            format!(
                "`{RESERVED_PREFIX}` is a reserved name prefix and belongs to what the block generates"
            ),
            at,
        ));
    }
    let bytes = body.as_bytes();
    let mut markup = String::with_capacity(body.len());
    let mut args: Vec<String> = Vec::new();
    let mut components: Vec<Component> = Vec::new();
    let mut elements = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if body[i..].starts_with("<!--") {
            let end = body[i..].find("-->").map_or(bytes.len(), |at| i + at + 3);
            markup.push_str(&body[i..end]);
            i = end;
            continue;
        }
        if bytes[i] == b'{' {
            let end = body[i..].find('}').map_or(bytes.len(), |at| i + at + 1);
            markup.push_str(&body[i..end]);
            i = end;
            continue;
        }
        if bytes[i] == b'$' {
            if bytes.get(i + 1) == Some(&b'$') {
                markup.push('$');
                i += 2;
                continue;
            }
            if let Some(end) = ident_end(bytes, i + 1) {
                let name = &body[i + 1..end];
                if !args.iter().any(|a| a == name) {
                    args.push(name.to_string());
                }
                markup.push('{');
                markup.push_str(name);
                markup.push('}');
                i = end;
                continue;
            }
        }
        if bytes[i] == b'<' && bytes.get(i + 1).is_some_and(u8::is_ascii_uppercase) {
            let element = read_component(body, i, components.len())?;
            elements += 1;
            markup.push_str(&format!("<slot name=\"{}\"/>", element.component.slot));
            components.push(element.component);
            i = element.end;
            continue;
        }
        if bytes[i] == b'<' && bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic) {
            elements += 1;
        }
        let ch = body[i..].chars().next().unwrap_or('\0');
        markup.push(ch);
        i += ch.len_utf8();
    }
    let lone_component = components.len() == 1
        && elements == 1
        && normalize(&markup) == format!("<slot name=\"{}\"/>", components[0].slot);
    Ok(Scan {
        markup,
        args,
        components,
        lone_component,
    })
}

/// One component element, read.
struct ReadComponent {
    component: Component,
    /// Byte offset just past the element.
    end: usize,
}

/// Read the component element that opens at `start`.
fn read_component(body: &str, start: usize, index: usize) -> Result<ReadComponent, LmnError> {
    let bytes = body.as_bytes();
    let name_end = ident_end(bytes, start + 1)
        .ok_or_else(|| LmnError::new("a component element needs a name", start))?;
    let name = body[start + 1..name_end].to_string();
    let tag = read_tag(body, name_end, start)?;
    let props = parse_props(&body[name_end..tag.attrs_end], name_end)?;
    let component = Component {
        name: name.clone(),
        slot: slot_name(index),
        props,
        offset: start,
    };
    if tag.self_closing {
        return Ok(ReadComponent {
            component,
            end: tag.end,
        });
    }
    let close = format!("</{name}");
    let at = body[tag.end..]
        .find(&close)
        .ok_or_else(|| LmnError::new(format!("component <{name}> is never closed"), start))?;
    if !body[tag.end..tag.end + at].trim().is_empty() {
        return Err(LmnError::new(
            format!(
                "component <{name}> takes no markup children; pass what it renders as a prop instead"
            ),
            start,
        ));
    }
    let rest = tag.end + at + close.len();
    let close_end = body[rest..]
        .find('>')
        .map_or(body.len(), |at| rest + at + 1);
    Ok(ReadComponent {
        component,
        end: close_end,
    })
}

/// Where a start tag ends, and whether it closed itself.
struct Tag {
    /// Byte offset of the tag's `>`, one past the last attribute.
    attrs_end: usize,
    /// Byte offset just past the `>`.
    end: usize,
    self_closing: bool,
}

/// Find the end of the start tag whose attributes begin at `from`. Quoted
/// attribute values may hold `>`, so the scan tracks them.
fn read_tag(body: &str, from: usize, start: usize) -> Result<Tag, LmnError> {
    let bytes = body.as_bytes();
    let mut i = from;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        match (quote, bytes[i]) {
            (Some(open), b) if b == open => quote = None,
            (Some(_), _) => {}
            (None, b @ (b'"' | b'\'')) => quote = Some(b),
            (None, b'>') => {
                let self_closing = i > from && bytes[i - 1] == b'/';
                return Ok(Tag {
                    attrs_end: if self_closing { i - 1 } else { i },
                    end: i + 1,
                    self_closing,
                });
            }
            (None, _) => {}
        }
        i += 1;
    }
    Err(LmnError::new("a tag is never closed", start))
}

/// Read `name="value"` pairs out of a start tag's attribute text. `base` is
/// where that text sits in the body, so an error points at the file.
fn parse_props(text: &str, base: usize) -> Result<Vec<(String, String)>, LmnError> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let Some(name_end) = ident_end(bytes, i).map(|end| extend_attr_name(bytes, end)) else {
            i += 1;
            continue;
        };
        let name = text[i..name_end].to_string();
        let mut j = name_end;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j) != Some(&b'=') {
            return Err(LmnError::new(
                format!("prop `{name}` needs a value"),
                base + i,
            ));
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let Some(open) = bytes.get(j).copied().filter(|b| *b == b'"' || *b == b'\'') else {
            return Err(LmnError::new(
                format!("prop `{name}` needs a quoted value"),
                base + i,
            ));
        };
        let value_start = j + 1;
        let end = text[value_start..]
            .find(open as char)
            .map(|at| value_start + at)
            .ok_or_else(|| {
                LmnError::new(format!("prop `{name}` has no closing quote"), base + i)
            })?;
        out.push((name, text[value_start..end].to_string()));
        i = end + 1;
    }
    Ok(out)
}

/// Markup attribute names carry `-` (`bind-text`), which is not part of a
/// candela identifier; extend past the identifier the scanner stopped at.
fn extend_attr_name(bytes: &[u8], mut end: usize) -> usize {
    while end < bytes.len()
        && (bytes[end] == b'-'
            || bytes[end] == b':'
            || bytes[end].is_ascii_alphanumeric()
            || bytes[end] == b'_')
    {
        end += 1;
    }
    end
}

/// One `fn` declaration, as the index reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FnDecl {
    /// Parameter names, in order.
    params: Vec<String>,
    /// Byte range of the body, between the braces. Empty when the
    /// declaration has no body the scan could delimit.
    body: Range<usize>,
}

/// The `fn NAME(params) { body }` declarations of one candela source.
///
/// A component element is a call to one of these, and props map to parameters
/// by name, so the emission needs each function's parameter list in order.
/// Markup names one as a tag, and whether it may depends on the shape of the
/// body, so the body's extent is read here too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FnIndex {
    fns: BTreeMap<String, FnDecl>,
}

impl FnIndex {
    /// Read every top-level `fn` declaration in `src`.
    ///
    /// Text inside a string literal or after `//` declares nothing, matching
    /// what candela's own lexer skips.
    #[must_use]
    pub fn scan(src: &str) -> Self {
        let bytes = src.as_bytes();
        let mut fns: BTreeMap<String, FnDecl> = BTreeMap::new();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => {
                    i = skip_string(bytes, i);
                    continue;
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                        i += 1;
                    }
                    continue;
                }
                _ => {}
            }
            let Some(word_end) = ident_end(bytes, i) else {
                i += 1;
                continue;
            };
            if &src[i..word_end] != "fn" || (i > 0 && is_ident_byte(bytes[i - 1])) {
                i = word_end;
                continue;
            }
            let mut j = word_end;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let Some(name_end) = ident_end(bytes, j) else {
                i = word_end;
                continue;
            };
            let name = src[j..name_end].to_string();
            let mut k = name_end;
            while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            if bytes.get(k) != Some(&b'(') {
                i = word_end;
                continue;
            }
            let Some(close) = src[k..].find(')').map(|at| k + at) else {
                break;
            };
            let body = body_span(bytes, close + 1);
            fns.insert(
                name,
                FnDecl {
                    params: split_params(&src[k + 1..close]),
                    body,
                },
            );
            i = close + 1;
        }
        Self { fns }
    }

    /// The parameters `name` declares, in order.
    #[must_use]
    pub fn params(&self, name: &str) -> Option<&[String]> {
        self.fns.get(name).map(|decl| decl.params.as_slice())
    }

    /// The function whose body holds `offset`, with that body's byte range.
    ///
    /// A declaration written inside another one is indexed too, so the
    /// narrowest body wins: that is the function the offset is written in.
    #[must_use]
    pub fn enclosing(&self, offset: usize) -> Option<(&str, Range<usize>)> {
        self.fns
            .iter()
            .filter(|(_, decl)| decl.body.contains(&offset))
            .min_by_key(|(_, decl)| decl.body.len())
            .map(|(name, decl)| (name.as_str(), decl.body.clone()))
    }
}

/// Byte range between the braces of the body that opens at or after `from`,
/// or an empty range when there is no body there.
///
/// Braces inside a string literal or a `//` comment do not nest, matching what
/// [`FnIndex::scan`] itself skips.
fn body_span(bytes: &[u8], from: usize) -> Range<usize> {
    let mut i = from;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&b'{') {
        return from..from;
    }
    let start = i + 1;
    let mut depth = 0u32;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = skip_string(bytes, i);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return start..i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    from..from
}

/// Parameter names out of a declaration's argument text, dropping any type
/// annotation.
fn split_params(text: &str) -> Vec<String> {
    text.split(',')
        .filter_map(|part| {
            let name = part.split(':').next().unwrap_or("").trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

/// Byte index just past the string literal opening at `start`, or the end of
/// the input when it is never closed.
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(src: &str) -> FnIndex {
        FnIndex::scan(src)
    }

    #[test]
    fn a_key_is_sixteen_hex_characters_of_the_normalized_body() {
        let key = key_of("  <label text=\"hi\"/>\r\n  ");
        assert_eq!(key.len(), KEY_LEN);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(key, key_of("<label text=\"hi\"/>"));
    }

    /// Pinned: the key travels in the artifact and in generated candela, so a
    /// change to the hash is a change to every built app.
    #[test]
    fn the_key_of_a_known_body_is_pinned() {
        assert_eq!(key_of("<label text=\"hi\"/>"), "79114ba6b591efb1");
    }

    #[test]
    fn different_bodies_key_apart() {
        assert_ne!(key_of("<label text=\"a\"/>"), key_of("<label text=\"b\"/>"));
    }

    #[test]
    fn a_dollar_name_becomes_an_argument_marker() {
        let block = analyze("<label text=\"home for $name\"/>").expect("a block");
        assert_eq!(block.args, ["name"]);
        assert_eq!(block.markup, "<label text=\"home for {name}\"/>");
    }

    #[test]
    fn a_doubled_dollar_is_a_literal_one() {
        let block = analyze("<label text=\"$$5 and $cost\"/>").expect("a block");
        assert_eq!(block.args, ["cost"]);
        assert_eq!(block.markup, "<label text=\"$5 and {cost}\"/>");
    }

    #[test]
    fn a_lone_dollar_stays_text() {
        let block = analyze("<label text=\"100 $\"/>").expect("a block");
        assert!(block.args.is_empty());
        assert_eq!(block.markup, "<label text=\"100 $\"/>");
    }

    #[test]
    fn a_signal_marker_is_not_an_argument() {
        let block = analyze("<label text=\"{count} and {$total}\"/>").expect("a block");
        assert!(block.args.is_empty(), "{{name}} stays a signal reference");
        assert_eq!(block.markup, "<label text=\"{count} and {$total}\"/>");
    }

    #[test]
    fn a_comment_holds_no_sites() {
        let block = analyze("<column><!-- $draft <Ghost/> --></column>").expect("a block");
        assert!(block.args.is_empty());
        assert!(block.components.is_empty());
        assert_eq!(block.markup, "<column><!-- $draft <Ghost/> --></column>");
    }

    #[test]
    fn an_argument_is_listed_once_in_source_order() {
        let block = analyze("<label text=\"$b $a $b\"/>").expect("a block");
        assert_eq!(block.args, ["b", "a"]);
    }

    #[test]
    fn a_reserved_name_is_refused() {
        let err = analyze("<column><slot name=\"lmn-child-0\"/></column>").expect_err("reserved");
        assert!(err.message.contains("reserved"), "{}", err.message);
    }

    #[test]
    fn a_component_becomes_a_slot() {
        let block = analyze("<column><Home name=\"bob\"/></column>").expect("a block");
        assert_eq!(
            block.markup,
            "<column><slot name=\"lmn-child-0\"/></column>"
        );
        assert_eq!(block.components.len(), 1);
        assert_eq!(block.components[0].name, "Home");
        assert_eq!(block.components[0].slot, "lmn-child-0");
        assert_eq!(
            block.components[0].props,
            [("name".to_string(), "bob".to_string())]
        );
        assert!(!block.lone_component);
    }

    #[test]
    fn a_body_that_is_one_component_is_that_component() {
        let block = analyze("<Home name=\"bob\"/>").expect("a block");
        assert!(block.lone_component);
    }

    #[test]
    fn components_number_their_slots_in_source_order() {
        let block = analyze("<column><A/><B/></column>").expect("a block");
        let slots: Vec<&str> = block.components.iter().map(|c| c.slot.as_str()).collect();
        assert_eq!(slots, ["lmn-child-0", "lmn-child-1"]);
    }

    #[test]
    fn a_component_with_children_is_refused() {
        let err = analyze("<column><Card><label/></Card></column>").expect_err("children");
        assert!(
            err.message.contains("no markup children"),
            "{}",
            err.message
        );
    }

    #[test]
    fn an_empty_component_body_is_the_same_as_self_closing() {
        let block = analyze("<column><Card></Card></column>").expect("a block");
        assert_eq!(
            block.markup,
            "<column><slot name=\"lmn-child-0\"/></column>"
        );
    }

    #[test]
    fn a_quoted_angle_bracket_does_not_end_a_tag() {
        let block = analyze("<column><Home label=\"a > b\"/></column>").expect("a block");
        assert_eq!(
            block.components[0].props,
            [("label".to_string(), "a > b".to_string())]
        );
    }

    #[test]
    fn an_expansion_passes_arguments_by_name() {
        let source = expand("<label text=\"home for $name\"/>", &index("")).expect("expands");
        assert_eq!(
            source,
            format!(
                "lumen::fragment_spawn(\"{}\", [\"name\", str(name)], [])",
                key_of("<label text=\"home for $name\"/>")
            )
        );
        assert!(!source.contains('\n'), "an expansion is one line");
    }

    #[test]
    fn an_expansion_calls_a_child_component_first() {
        let index = index("fn Home(name) { return 0; }");
        let source = expand("<column><Home name=\"bob\"/></column>", &index).expect("expands");
        assert!(source.contains("[Home(\"bob\")]"), "{source}");
    }

    #[test]
    fn a_body_that_is_one_component_expands_to_its_call() {
        let index = index("fn Home(name) { return 0; }");
        assert_eq!(
            expand("<Home name=\"bob\"/>", &index).expect("expands"),
            "Home(\"bob\")"
        );
    }

    #[test]
    fn a_prop_that_is_one_reference_passes_the_value_through() {
        let index = index("fn Home(count) { return 0; }");
        assert_eq!(
            expand("<Home count=\"$n\"/>", &index).expect("expands"),
            "Home(n)"
        );
    }

    #[test]
    fn a_mixed_prop_renders_its_references() {
        let index = index("fn Home(label) { return 0; }");
        assert_eq!(
            expand("<Home label=\"hi $who\"/>", &index).expect("expands"),
            "Home(\"hi \" + str(who))"
        );
    }

    #[test]
    fn a_parameter_no_prop_names_is_passed_empty() {
        let index = index("fn Home(a, b) { return 0; }");
        assert_eq!(
            expand("<Home b=\"x\"/>", &index).expect("expands"),
            "Home(\"\", \"x\")"
        );
    }

    #[test]
    fn props_map_by_name_not_by_position() {
        let index = index("fn Home(first, second) { return 0; }");
        assert_eq!(
            expand("<Home second=\"2\" first=\"1\"/>", &index).expect("expands"),
            "Home(\"1\", \"2\")"
        );
    }

    #[test]
    fn a_component_naming_no_function_is_refused() {
        let err = expand("<Home name=\"bob\"/>", &index("")).expect_err("unknown");
        assert!(err.message.contains("<Home>"), "{}", err.message);
    }

    #[test]
    fn a_prop_naming_no_parameter_is_refused() {
        let index = index("fn Home(name) { return 0; }");
        let err = expand("<Home title=\"bob\"/>", &index).expect_err("unknown prop");
        assert!(err.message.contains("<Home>"), "{}", err.message);
        assert!(err.message.contains("title"), "{}", err.message);
    }

    #[test]
    fn the_index_reads_names_and_parameters() {
        let index = index("fn a(x, y: int) { }\nfn b() { }\n");
        assert_eq!(
            index.params("a"),
            Some(["x".to_string(), "y".to_string()].as_slice())
        );
        assert_eq!(index.params("b"), Some([].as_slice()));
        assert_eq!(index.params("c"), None);
    }

    #[test]
    fn the_index_skips_strings_and_comments() {
        let index = index("// fn commented(x) {}\nlet s = \"fn quoted(y) {}\";\nfn real(z) {}\n");
        assert_eq!(index.params("real"), Some(["z".to_string()].as_slice()));
        assert_eq!(index.params("commented"), None);
        assert_eq!(index.params("quoted"), None);
    }

    #[test]
    fn regions_skip_strings_and_comments() {
        let src = "fn a() { return lmn!(<b/>); }\n\
                   fn b() { let s = \"lmn!(<c/>)\"; }\n\
                   // lmn!(<d/>)\n";
        let found: Vec<&str> = regions(src).into_iter().map(|r| r.body).collect();
        assert_eq!(found, ["<b/>"]);
    }

    #[test]
    fn a_region_offset_points_into_the_source() {
        let src = "fn a() { return lmn!(<b/>); }";
        let region = &regions(src)[0];
        assert_eq!(
            &src[region.body_start..region.body_start + region.body.len()],
            "<b/>"
        );
        assert_eq!(&src[region.span.clone()], "lmn!(<b/>)");
    }

    /// The one shape markup can stand in for: the whole body is the return.
    #[test]
    fn a_single_return_is_the_component_markup_may_name() {
        let src = "fn Home(name) { return lmn!(<label text=\"$name\"/>); }";
        let region = &regions(src)[0];
        assert_eq!(
            component_at(src, &region.span, &index(src)),
            Some(ComponentFn {
                name: "Home".to_string(),
                inlinable: true,
            })
        );
    }

    #[test]
    fn a_return_without_a_trailing_semicolon_is_still_the_whole_body() {
        let src = "fn Home() { return lmn!(<label/>) }";
        let region = &regions(src)[0];
        assert!(
            component_at(src, &region.span, &index(src))
                .expect("Home")
                .inlinable
        );
    }

    #[test]
    fn two_returns_leave_no_single_block_to_inline() {
        let src = "fn Toggle(on) {\n\
                       if on { return lmn!(<label text=\"on\"/>); }\n\
                       return lmn!(<label text=\"off\"/>);\n\
                   }";
        for region in regions(src) {
            let component = component_at(src, &region.span, &index(src)).expect("Toggle");
            assert_eq!(component.name, "Toggle");
            assert!(!component.inlinable, "{region:?}");
        }
    }

    #[test]
    fn a_statement_before_the_return_leaves_no_block_to_inline() {
        let src = "fn Home() { let x = 1; return lmn!(<label/>); }";
        let region = &regions(src)[0];
        assert!(
            !component_at(src, &region.span, &index(src))
                .expect("Home")
                .inlinable
        );
    }

    #[test]
    fn a_lowercase_function_is_not_a_component() {
        let src = "fn home() { return lmn!(<label/>); }";
        let region = &regions(src)[0];
        assert_eq!(component_at(src, &region.span, &index(src)), None);
    }

    #[test]
    fn the_index_reads_the_body_a_declaration_encloses() {
        let src = "fn a() { let s = \"}\"; } // }\nfn b() { }\n";
        let index = index(src);
        let (name, body) = index
            .enclosing(src.find("let").expect("the statement"))
            .expect("a encloses it");
        assert_eq!(name, "a");
        assert_eq!(src[body].trim(), "let s = \"}\";");
        assert_eq!(index.enclosing(0), None);
    }
}
