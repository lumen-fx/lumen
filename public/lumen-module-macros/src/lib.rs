//! The expansion behind `lumen_module!`. Reach it through `lumen-module`,
//! which re-exports it; this crate is not depended on directly.
//!
//! # What it emits
//!
//! Every export the macro generates carries the module's declared name, so
//! two modules linked into one binary never define the same symbol. The name
//! comes from the first argument, and the suffix is that name with every
//! character a symbol cannot carry replaced by `_`:
//!
//! - `lumen_module_register_<n>`, and a `#[used]` pointer to it in the
//!   platform's pre-main constructor section (`.init_array`,
//!   `__DATA,__mod_init_func`, `.CRT$XCU`). Always emitted: this is how a
//!   module linked into a binary reaches the registry.
//! - `lumen_module_probe_<n>` and `lumen_module_install_<n>`, the pair the
//!   loader looks up after opening a shared library. Emitted only with the
//!   `engine-dylib` feature and only off Windows, the two conditions under
//!   which a module can be opened rather than linked.
//!
//! The names are the loader's contract, not the author's: `crates/modules`
//! builds the same strings from the name the app declares in `lumen.toml`,
//! which is why the macro takes the name rather than reading the package's.
//! The two spellings must agree, and a module declared under a name it was
//! not built with reads as absent.

use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, LitStr, Token, parse_macro_input};

/// `lumen_module!("<declared name>", <constructor>)`.
struct ModuleEntry {
    name: LitStr,
    ctor: Expr,
}

impl Parse for ModuleEntry {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: LitStr = input.parse().map_err(|e| {
            syn::Error::new(
                e.span(),
                "lumen_module! takes the module's declared name first, as a string literal: \
                 lumen_module!(\"my-module\", |config: ModuleConfig| MyPlugin::new(&config))",
            )
        })?;
        input.parse::<Token![,]>()?;
        let ctor: Expr = input.parse()?;
        // A trailing comma reads naturally after a multi-line constructor.
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        }
        Ok(ModuleEntry { name, ctor })
    }
}

/// Export a plugin as a Lumen runtime module.
///
/// See the `lumen-module` crate docs for the authoring shape; see this
/// crate's docs for what the expansion contains.
#[proc_macro]
pub fn lumen_module(input: TokenStream) -> TokenStream {
    let ModuleEntry { name, ctor } = parse_macro_input!(input as ModuleEntry);

    let declared = name.value();
    if declared.trim().is_empty() {
        return syn::Error::new(name.span(), "the module's declared name must not be empty")
            .to_compile_error()
            .into();
    }
    let suffix = symbol_suffix(&declared);
    let probe_name = format!("lumen_module_probe_{suffix}");
    let install_name = format!("lumen_module_install_{suffix}");
    let register_name = format!("lumen_module_register_{suffix}");
    let register_fn = Ident::new(&format!("register_{suffix}"), Span::call_site());

    // The dlopen half. A module that is linked in has no probe to answer and
    // no shared engine to answer for.
    let dylib_entries = if cfg!(feature = "engine-dylib") {
        quote! {
            // Naming the engine dylib from the module's own crate is what
            // records its dependency on the shared engine, so an author
            // cannot build a module that forgot it.
            #[cfg(not(windows))]
            use ::lumen_module::lumen_dylib as _;

            #[cfg(not(windows))]
            #[unsafe(export_name = #probe_name)]
            extern "C" fn probe() -> *const ::std::os::raw::c_char {
                ::lumen_module::BUILD_ID_C.as_ptr() as *const ::std::os::raw::c_char
            }

            // Rust ABI: the loader calls this only after the probe proved
            // both sides are one build.
            #[cfg(not(windows))]
            #[unsafe(export_name = #install_name)]
            fn install(app: &mut ::lumen_module::App, config_toml: &str) -> u32 {
                install_module(app, config_toml)
            }
        }
    } else {
        quote! {}
    };

    quote! {
        const _: () = {
            fn install_module(app: &mut ::lumen_module::App, config_toml: &str) -> u32 {
                ::lumen_module::install_with(app, config_toml, #ctor)
            }

            #dylib_entries

            #[unsafe(export_name = #register_name)]
            extern "C" fn #register_fn() {
                ::lumen_module::registry::register(::lumen_module::registry::StaticModule {
                    name: #name,
                    install: install_module,
                });
            }

            // The pre-main constructor. `#[used]` keeps the pointer in the
            // object file; the section is what the platform's startup code
            // walks. A link that wants this module names the register symbol
            // above, which is what pulls this object in.
            #[used]
            #[cfg_attr(
                all(unix, not(target_vendor = "apple")),
                unsafe(link_section = ".init_array")
            )]
            #[cfg_attr(
                target_vendor = "apple",
                unsafe(link_section = "__DATA,__mod_init_func")
            )]
            #[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
            static CTOR: extern "C" fn() = #register_fn;
        };
    }
    .into()
}

/// The declared name as a symbol suffix. Kept in step with the loader's own
/// spelling in `crates/modules/src/lib.rs`, which builds the same names
/// from the `lumen.toml` key.
fn symbol_suffix(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::symbol_suffix;

    #[test]
    fn a_hyphenated_name_underscores() {
        assert_eq!(symbol_suffix("lumen-audio"), "lumen_audio");
    }

    #[test]
    fn a_plain_name_is_left_alone() {
        assert_eq!(symbol_suffix("fixture"), "fixture");
    }

    #[test]
    fn anything_a_symbol_cannot_carry_becomes_an_underscore() {
        assert_eq!(symbol_suffix("shape.tools+2"), "shape_tools_2");
    }
}
