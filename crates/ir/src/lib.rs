//! `lumen-ir` - the runtime-shared intermediate representation for Lumen.
//!
//! This crate owns the data model that both the compiler (`lumenc`) and the
//! parser-free runtime consume:
//!
//! - [`layout_ir`] - [`LayoutIR`](layout_ir::LayoutIR), the tree of styled
//!   elements decoupled from ECS spawning, plus the `From` impls that convert
//!   each IR spec into its `lumen_core` / `lumen_primitives` runtime type.
//! - [`css`] - the CSS AST ([`Stylesheet`](css::Stylesheet)) and the
//!   Cascade-5 application / re-application logic. The hand-rolled front-end
//!   that *parses* CSS text into a [`Stylesheet`](css::Stylesheet) still lives
//!   in `lumenc::parser_css`; this crate owns the data + the cascade.
//! - [`fragment`] - [`Fragment`](fragment::Fragment), the named reusable
//!   markup subtree, and the [`FragmentTable`](fragment::FragmentTable) an
//!   app declares.
//! - [`values`] - shared attribute/property value parsers.
//! - [`css_vars`] - the `var(--name [, fallback])` resolver.
//! - [`artifact`] - the AOT compiled-app container (`lumenc build` output).
//!
//! `lumenc` re-exports every module here so existing `lumenc::...` paths stay
//! valid.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod artifact;
pub mod css;
pub mod css_vars;
pub mod fragment;
pub mod layout_ir;
pub mod values;

/// IR -> runtime-component `From` conversions (orphan-rule home for the
/// `Attributes`/value-spec -> `lumen_core`/`lumen_primitives` impls).
mod convert;
pub use convert::typography_role_to_px;
