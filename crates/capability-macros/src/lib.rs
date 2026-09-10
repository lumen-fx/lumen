//! The expansion behind `lumen_capability!`. Reach it through
//! `lumen-capability`, which re-exports it; this crate is not depended on
//! directly.
//!
//! # What it emits
//!
//! - `lumen_capability_register_<n>`, an `extern "C"` function that puts the
//!   capability on the registry, where `<n>` is the declared name with every
//!   character a symbol cannot carry replaced by `_`. The entry records the
//!   package the macro expanded in, so a kit can tell which of its files
//!   carries the capability.
//! - A `#[used]` pointer to that function in the platform's pre-main
//!   constructor section (`.init_array`, `__DATA,__mod_init_func`,
//!   `.CRT$XCU`), so a binary the capability is linked into registers it
//!   before `main` runs.
//!
//! The register symbol is the linker's handle on the capability. A link that
//! names it (`-u` on ELF and Mach-O, `/INCLUDE:` on Windows) pulls the
//! object that defines it out of its archive; a link that does not, and that
//! leaves rustc's `symbols.o` off the line, links nothing of the capability
//! at all. `crates/capability` spells the same name from the declared string
//! at runtime, which is why the macro takes the name rather than reading the
//! package's.

use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Expr, LitStr, Token, parse_macro_input};

/// `lumen_capability!("<name>", <phase>, <install>)`, with any of
/// `preflight = <fn>` and `select = <Select>` after the install entry.
struct CapabilityEntry {
    name: LitStr,
    phase: Expr,
    install: Expr,
    preflight: Option<Expr>,
    select: Option<Expr>,
}

impl Parse for CapabilityEntry {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: LitStr = input.parse().map_err(|e| {
            syn::Error::new(
                e.span(),
                "lumen_capability! takes the capability's name first, as a string literal: \
                 lumen_capability!(\"os-tray\", Phase::Platform, install)",
            )
        })?;
        input.parse::<Token![,]>()?;
        let phase: Expr = input.parse()?;
        input.parse::<Token![,]>()?;
        let install: Expr = input.parse()?;
        let mut preflight = None;
        let mut select = None;
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let slot = match key.to_string().as_str() {
                "preflight" => &mut preflight,
                "select" => &mut select,
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        "the options after the install entry are `preflight = <fn>` and \
                         `select = <Select>`",
                    ));
                }
            };
            if slot.is_some() {
                return Err(syn::Error::new(
                    key.span(),
                    format!("`{key}` is given twice"),
                ));
            }
            *slot = Some(input.parse()?);
        }
        Ok(CapabilityEntry {
            name,
            phase,
            install,
            preflight,
            select,
        })
    }
}

/// Declare an optional runtime subsystem.
///
/// See the `lumen-capability` crate docs for the authoring shape; see this
/// crate's docs for what the expansion contains.
#[proc_macro]
pub fn lumen_capability(input: TokenStream) -> TokenStream {
    let CapabilityEntry {
        name,
        phase,
        install,
        preflight,
        select,
    } = parse_macro_input!(input as CapabilityEntry);

    let declared = name.value();
    if declared.trim().is_empty() {
        return syn::Error::new(name.span(), "the capability's name must not be empty")
            .to_compile_error()
            .into();
    }
    let suffix = symbol_suffix(&declared);
    let register_name = format!("lumen_capability_register_{suffix}");
    let register_fn = Ident::new(&format!("register_{suffix}"), Span::call_site());
    let preflight = match preflight {
        Some(f) => quote! { ::core::option::Option::Some(#f) },
        None => quote! { ::core::option::Option::None },
    };
    let select = match select {
        Some(s) => quote! { #s },
        None => quote! { ::lumen_capability::Select::Always },
    };

    quote! {
        const _: () = {
            #[unsafe(export_name = #register_name)]
            extern "C" fn #register_fn() {
                ::lumen_capability::register(::lumen_capability::Capability {
                    name: #name,
                    phase: #phase,
                    install: #install,
                    preflight: #preflight,
                    select: #select,
                    // The package this expands in, which is the object the
                    // register symbol pulls.
                    crate_name: env!("CARGO_PKG_NAME"),
                });
            }

            // The pre-main constructor. `#[used]` keeps the pointer in the
            // object file; the section is what the platform's startup code
            // walks. A link that wants this capability names the register
            // symbol above, which is what pulls this object in.
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

/// The declared name as a symbol suffix. Kept in step with
/// `lumen_capability::register_symbol`, which builds the same name from the
/// string an app or a link kit declares.
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
        assert_eq!(symbol_suffix("os-tray"), "os_tray");
    }

    #[test]
    fn anything_a_symbol_cannot_carry_becomes_an_underscore() {
        assert_eq!(symbol_suffix("http.fetch+2"), "http_fetch_2");
    }
}
