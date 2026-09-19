//! The markup + CSS front end: parse `.lmn` / `.css` from source, resolve
//! `<include>` / `@import`, fill fragments, and format markup back to text.

pub mod css;
/// Markup formatter - requires `roxmltree`, gated with the parser stack.
#[cfg(feature = "runtime-parse")]
pub mod formatter;
/// Fragment instantiation, gated with the parser stack that produces the
/// use sites it resolves.
#[cfg(feature = "runtime-parse")]
pub mod fragments;
/// Markup (`.lmn`) parser - the `roxmltree`-backed front-end, dropped from
/// parser-free runtime builds via the `runtime-parse` feature.
#[cfg(feature = "runtime-parse")]
pub mod html;
/// Ahead-of-time extraction of `lmn!` markup blocks from candela scripts, so
/// a shipped app carries the fragments they name and parses no markup at run
/// time. Gated with the parser stack it compiles bodies through.
#[cfg(feature = "runtime-parse")]
pub mod lmn;
/// `<include>` / `@import` resolution - parser-side only.
#[cfg(feature = "runtime-parse")]
pub mod resolve;
/// The compiler's implementation of the runtime's injected parser boundary.
/// Needs the source parser (`runtime-parse`) AND the runtime's `SourceParser`
/// trait (`dev-run`).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod source_parser;
