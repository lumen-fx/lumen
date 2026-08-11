//! `t!` macro - convenience over [`crate::I18n::t`].
//!
//! Two forms:
//!
//! ```ignore
//! // No args.
//! let s = t!(i18n, "hello");
//!
//! // With args. Each `name = value` is a `FluentArgs::set` call.
//! let s = t!(i18n, "greet", name = "World", count = 3);
//! ```
//!
//! The first argument is the `I18n` resource binding (typically a
//! `Res<I18n>` extracted from a system signature). The macro pushes
//! all `name = value` pairs into a temporary [`fluent_bundle::FluentArgs`]
//! and forwards to [`crate::I18n::t`].
//!
//! Also re-exported as `tr!` so `lumenc i18n extract` can recognize
//! both spellings.

/// See module-level docs.
#[macro_export]
macro_rules! t {
    ($i18n:expr, $key:expr $(,)?) => {{
        let args = $crate::reexports::FluentArgs::new();
        $i18n.t($key, &args).into_owned()
    }};
    ($i18n:expr, $key:expr, $($name:ident = $value:expr),+ $(,)?) => {{
        let mut args = $crate::reexports::FluentArgs::new();
        $(
            args.set(stringify!($name), $crate::reexports::FluentValue::from($value));
        )+
        $i18n.t($key, &args).into_owned()
    }};
}

/// Alias for [`t`]. Kept so authors who default-grep for `tr` (Qt's
/// spelling) find the same macro.
#[macro_export]
macro_rules! tr {
    ($($tt:tt)*) => { $crate::t!($($tt)*) };
}

/// Re-exports used by the [`t!`] macro expansion. Hidden so users only
/// see `t!` in IDE completion, not the underlying types.
#[doc(hidden)]
pub mod reexports {
    pub use fluent_bundle::{FluentArgs, FluentValue};
}

#[cfg(test)]
mod tests {
    use crate::I18n;
    use unic_langid::LanguageIdentifier;

    fn lang(s: &str) -> LanguageIdentifier {
        s.parse().expect("test lang parses")
    }

    #[test]
    fn t_macro_no_args() {
        let mut i = I18n::new(lang("en-US"), vec![]);
        i.load_ftl(lang("en-US"), "hi = Hi!").unwrap();
        let out: String = t!(i, "hi");
        assert_eq!(out, "Hi!");
    }

    #[test]
    fn t_macro_with_args() {
        let mut i = I18n::new(lang("en-US"), vec![]);
        i.load_ftl(lang("en-US"), "greet = Hi { $name }!").unwrap();
        let out: String = t!(i, "greet", name = "World");
        assert_eq!(out, "Hi World!");
    }

    #[test]
    fn t_macro_multiple_args() {
        let mut i = I18n::new(lang("en-US"), vec![]);
        i.load_ftl(lang("en-US"), "greet = { $greeting }, { $name }!")
            .unwrap();
        let out: String = t!(i, "greet", greeting = "Hello", name = "World");
        assert_eq!(out, "Hello, World!");
    }
}
