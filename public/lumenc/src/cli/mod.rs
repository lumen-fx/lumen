//! CLI subcommand handlers: everything `main.rs` dispatches to that is not
//! `run` / `web` / `package` / `link-kit` itself.

/// `lumenc build` - parse an app once and emit an AOT [`crate::artifact`].
/// Requires the source parser (`runtime-parse`) AND the runtime (`dev-run`):
/// it drives `compile_app` + `app_kind`, both of which live in `lumen-runtime`.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod build;
/// `lumenc bundle` - pack an app dir into a `.lpak` archive. Uses lumen-assets
/// (which pulls vello), so it is gated behind the default-on `bundle` feature.
#[cfg(feature = "bundle")]
pub mod bundle;
/// `lumenc add` / `remove` / `fetch` / `update` - the app's registry
/// dependencies from the command line. Gated with the registry client it
/// drives and the `lumen.toml` reader it edits.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod deps;
/// `lumenc i18n extract` - scan an app's sources for translatable keys and
/// write its catalogue. Gated with `dev-run`: the source language it defaults
/// to is `[app] fallback_locale`, which it reads through the runtime's
/// `lumen.toml`, and a thin build has no runtime to read it with.
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod i18n;
/// Static signal lint - walks the source parser (`runtime-parse`) and reads
/// `lumen.toml` config (`lumen-runtime`, `dev-run`).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod lint_signals;
/// MCP CLI handlers - read `lumen.toml` config (`dev-run`) and defer the
/// `--signals` lint to [`lint_signals`] (`runtime-parse`).
#[cfg(all(feature = "runtime-parse", feature = "dev-run"))]
pub mod mcp;
pub mod scaffold;
