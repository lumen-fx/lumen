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
//! which elements are components, and [`component_at`] decides whether a
//! component's block can stand in for calling it.
//!
//! # What a block becomes
//!
//! ```text
//! fn Home(name) { return lmn!(<label text="home for $name"/>); }
//! fn App() { return lmn!(<column><Home name="bob"/></column>); }
//! ```
//!
//! `Home`'s block is a fragment with one parameter, `name`. `App`'s block is a
//! fragment whose body holds `<Home name="bob"/>` as a use site naming `Home`.
//!
//! # Bake, then fill
//!
//! Nothing parses markup while an app runs, so every block in the app is a
//! compiled fragment before it starts. What a use site becomes depends on
//! whether the function it names has anything left to do:
//!
//! - **The build can stand in for the call.** `Home` returns its block and
//!   nothing else, and every value in that block is one its caller passed, so
//!   instantiating the fragment with those arguments is what calling `Home`
//!   would have produced. The build does exactly that, and `App`'s body holds
//!   the label from the first frame. No call.
//! - **The function has to run.** It works a value out, or picks between
//!   blocks. Every block it may return is still compiled, and what stands at
//!   the use site is a marker the runtime fills by calling the function and
//!   putting the node it returns in the marker's place. That happens on the
//!   first tick, before the tree is drawn.
//!
//! [`component_at`] draws that line. Either way the tree is baked ahead and
//! only values are worked out while the app runs.
//!
//! # Naming a component from markup
//!
//! A `.lmn` file writes `<Home name="bob"/>` too, and it means the same use
//! site. Markup carries no candela scope, so a prop there is text.
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

/// The slot `lumen::fragment_spawn` gives its `index`-th positional child.
///
/// The builtin takes slot content as a list of nodes and a fragment declares
/// its slots by name, so the two meet on this naming. A `<template>` writing
/// `<slot/>` names the default slot instead, which markup fills by nesting.
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
    /// The candela function the element names.
    pub name: String,
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
    /// parameter uses. A component element stays where it stands, as an
    /// element naming the function, which is what makes it a use site.
    pub markup: String,
    /// Argument names the body reads, in first-appearance order.
    pub args: Vec<String>,
    /// Component elements, in source order.
    pub components: Vec<Component>,
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
    /// The function's parameters, in declaration order. Props reach them by
    /// name, and a call passes them in this order.
    pub params: Vec<String>,
    /// Whether instantiating the block is the same as calling the function.
    pub inlinable: bool,
}

/// The component the function around `span` declares, if markup may name it.
///
/// A candela function is a component when its name starts with a capital, the
/// way a component element in a block is spelled.
///
/// [`inlinable`](ComponentFn::inlinable) is whether the block can stand in for
/// the call outright, which holds when two things do:
///
/// - the function's whole body is that one `return lmn!(...)`, so nothing else
///   it would have run is skipped; and
/// - every `$name` the block reads is one of the function's own parameters, so
///   every value in the block is one the caller passed rather than one the
///   function worked out.
///
/// Both together mean the block with its arguments bound is exactly what the
/// call returns, so the build can put it in the tree and no call is needed.
/// A function that fails either still yields its name: markup names it the
/// same way, and what stands in the tree is filled by calling it.
#[must_use]
pub fn component_at(
    src: &str,
    span: &Range<usize>,
    index: &FnIndex,
    reads: &[String],
) -> Option<ComponentFn> {
    let (name, body) = index.enclosing(span.start)?;
    if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let params: Vec<String> = index.params(name).unwrap_or_default().to_vec();
    let before = src.get(body.start..span.start)?.trim();
    let after = src.get(span.end..body.end)?.trim();
    let whole_body = before == "return" && matches!(after, "" | ";");
    let forwarded = reads.iter().all(|read| params.contains(read));
    Some(ComponentFn {
        name: name.to_string(),
        params,
        inlinable: whole_body && forwarded,
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
        components: scan.components,
    })
}

/// The candela source one block expands to: the call that instantiates the
/// fragment the block declares.
///
/// A component element inside the block is not part of that call. It is a use
/// site in the fragment's body, which the build resolves against the component
/// it names: a component the build can stand in for is already in the body,
/// and one that has to run is filled where it stands when the app runs.
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
    for component in &block.components {
        check_props(component, index)?;
    }
    let mut args = Vec::with_capacity(block.args.len() * 2);
    for name in &block.args {
        args.push(format!("\"{name}\""));
        args.push(format!("str({name})"));
    }
    Ok(format!(
        "lumen::fragment_spawn(\"{}\", [{}], [])",
        block.key,
        args.join(", ")
    ))
}

/// Check a component element against the function it names.
///
/// Props reach the function by parameter name, so one naming a parameter the
/// function does not declare would go nowhere. Caught here, where the writer
/// is looking at the block.
fn check_props(component: &Component, index: &FnIndex) -> Result<(), LmnError> {
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
    Ok(())
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
}

/// Rewrite the body into markup and collect what the emission needs.
///
/// One pass. `$name` outside a `{...}` marker is an argument and becomes the
/// marker a fragment parameter uses; `$$` is a literal `$`. An element whose
/// tag starts with a capital is a component: it is read for its props and left
/// standing, so the markup carries an element naming the function and the
/// fragment gets a use site there. Comments pass through untouched, so nothing
/// inside one is a site.
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
        // A component element is read for its props and its shape, then left
        // for the walk to copy like any other element: what the markup needs
        // is the element itself, so the fragment gets a use site naming the
        // function, and `$name` inside a prop is rewritten on the way past.
        if bytes[i] == b'<' && bytes.get(i + 1).is_some_and(u8::is_ascii_uppercase) {
            components.push(read_component(body, i)?);
        }
        let ch = body[i..].chars().next().unwrap_or('\0');
        markup.push(ch);
        i += ch.len_utf8();
    }
    Ok(Scan {
        markup,
        args,
        components,
    })
}

/// Read the component element that opens at `start`.
///
/// # Errors
///
/// [`LmnError`] when the element has no name, is never closed, or was given
/// markup children.
fn read_component(body: &str, start: usize) -> Result<Component, LmnError> {
    let bytes = body.as_bytes();
    let name_end = ident_end(bytes, start + 1)
        .ok_or_else(|| LmnError::new("a component element needs a name", start))?;
    let name = body[start + 1..name_end].to_string();
    let tag = read_tag(body, name_end, start)?;
    let component = Component {
        name: name.clone(),
        props: parse_props(&body[name_end..tag.attrs_end], name_end)?,
        offset: start,
    };
    if tag.self_closing {
        return Ok(component);
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
    Ok(component)
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

    /// A component element stays in the markup, which is what makes the
    /// fragment carry a use site naming the function.
    #[test]
    fn a_component_stays_where_it_stands() {
        let block = analyze("<column><Home name=\"bob\"/></column>").expect("a block");
        assert_eq!(block.markup, "<column><Home name=\"bob\"/></column>");
        assert_eq!(block.components.len(), 1);
        assert_eq!(block.components[0].name, "Home");
        assert_eq!(
            block.components[0].props,
            [("name".to_string(), "bob".to_string())]
        );
    }

    /// A prop reading `$name` puts that name on the block, so the enclosing
    /// instance's argument reaches the use site.
    #[test]
    fn a_component_prop_reads_the_block_argument() {
        let block = analyze("<column><Inner t=\"$t\"/></column>").expect("a block");
        assert_eq!(block.markup, "<column><Inner t=\"{t}\"/></column>");
        assert_eq!(block.args, ["t"]);
    }

    /// A body that is one component element is a fragment whose root is that
    /// use site, so markup naming the enclosing function reaches it.
    #[test]
    fn a_body_that_is_one_component_is_a_use_site() {
        let block = analyze("<Home name=\"bob\"/>").expect("a block");
        assert_eq!(block.markup, "<Home name=\"bob\"/>");
        assert_eq!(block.components.len(), 1);
    }

    #[test]
    fn components_are_listed_in_source_order() {
        let block = analyze("<column><A/><B/></column>").expect("a block");
        let names: Vec<&str> = block.components.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["A", "B"]);
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
        assert_eq!(block.components.len(), 1);
        assert_eq!(block.components[0].name, "Card");
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

    /// A component element is a use site in the fragment, not a call in the
    /// expansion: the build resolves it against the component it names.
    #[test]
    fn an_expansion_names_no_child_component() {
        let index = index("fn Home(name) { return 0; }");
        let body = "<column><Home name=\"bob\"/></column>";
        assert_eq!(
            expand(body, &index).expect("expands"),
            format!("lumen::fragment_spawn(\"{}\", [], [])", key_of(body))
        );
    }

    /// A body that is one component element expands to its own fragment, whose
    /// single root is the use site. That is what markup naming the enclosing
    /// function reaches.
    #[test]
    fn a_body_that_is_one_component_expands_to_its_fragment() {
        let index = index("fn Home(name) { return 0; }");
        let body = "<Home name=\"bob\"/>";
        assert_eq!(
            expand(body, &index).expect("expands"),
            format!("lumen::fragment_spawn(\"{}\", [], [])", key_of(body))
        );
    }

    /// A prop reading `$n` puts `n` on the enclosing block, so the value the
    /// caller passed reaches the use site through the instance's arguments.
    #[test]
    fn a_prop_reference_travels_as_a_block_argument() {
        let index = index("fn Home(count) { return 0; }");
        let body = "<Home count=\"$n\"/>";
        assert_eq!(
            expand(body, &index).expect("expands"),
            format!(
                "lumen::fragment_spawn(\"{}\", [\"n\", str(n)], [])",
                key_of(body)
            )
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

    /// One component, read: the block is the whole body and every value in it
    /// came from a parameter, so the build can stand in for the call.
    #[test]
    fn a_forwarded_block_stands_in_for_the_call() {
        let src = "fn Home(name) { return lmn!(<label text=\"$name\"/>); }";
        assert_eq!(
            read_component_fn(src),
            Some(ComponentFn {
                name: "Home".to_string(),
                params: vec!["name".to_string()],
                inlinable: true,
            })
        );
    }

    #[test]
    fn a_return_without_a_trailing_semicolon_is_still_the_whole_body() {
        let src = "fn Home() { return lmn!(<label/>) }";
        assert!(read_component_fn(src).expect("Home").inlinable);
    }

    /// A value the function worked out is not one the caller passed, so the
    /// block cannot stand in for the call and the function has to run.
    #[test]
    fn a_computed_value_needs_the_function_to_run() {
        let src = "fn Greet(n) { let u = upper(n); return lmn!(<label text=\"$u\"/>); }";
        let component = read_component_fn(src).expect("Greet");
        assert_eq!(component.params, ["n"]);
        assert!(!component.inlinable);
    }

    #[test]
    fn two_returns_leave_no_one_block_to_stand_in() {
        let src = "fn Toggle(on) {\n\
                       if on { return lmn!(<label text=\"on\"/>); }\n\
                       return lmn!(<label text=\"off\"/>);\n\
                   }";
        let index = index(src);
        for region in regions(src) {
            let args = analyze(region.body).expect("a block").args;
            let component = component_at(src, &region.span, &index, &args).expect("Toggle");
            assert_eq!(component.name, "Toggle");
            assert!(!component.inlinable, "{region:?}");
        }
    }

    #[test]
    fn a_statement_before_the_return_leaves_no_block_to_stand_in() {
        let src = "fn Home() { let x = 1; return lmn!(<label/>); }";
        assert!(!read_component_fn(src).expect("Home").inlinable);
    }

    #[test]
    fn a_lowercase_function_is_not_a_component() {
        let src = "fn home() { return lmn!(<label/>); }";
        assert_eq!(read_component_fn(src), None);
    }

    /// The component the first block in `src` belongs to.
    fn read_component_fn(src: &str) -> Option<ComponentFn> {
        let region = &regions(src)[0];
        let args = analyze(region.body).expect("a block").args;
        component_at(src, &region.span, &index(src), &args)
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
