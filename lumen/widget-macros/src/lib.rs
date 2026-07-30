//! `#[derive(Widget)]` - generates a [`lumen_widget::Widget`] impl plus a
//! companion `Plugin` struct for the annotated component.
//!
//! ## What gets emitted
//!
//! Given:
//!
//! ```ignore
//! use bevy_ecs::prelude::*;
//! use lumen_widget_macros::Widget;
//!
//! #[derive(Component, Widget, Default)]
//! #[widget(tag = "hello")]
//! pub struct Hello {
//!     #[widget(prop)] pub greeting: String,
//!     #[widget(state)] pub shown: bool,
//! }
//! ```
//!
//! the macro emits roughly:
//!
//! ```ignore
//! impl ::lumen_widget::Widget for Hello {
//!     fn name() -> &'static str { "Hello" }
//!     fn parser_tag() -> &'static str { "hello" }
//!     fn spawn(
//!         _parent: ::bevy_ecs::prelude::Entity,
//!         attrs: &::lumen_widget::Attributes,
//!         world: &mut ::bevy_ecs::prelude::World,
//!     ) -> ::bevy_ecs::prelude::Entity {
//!         let mut value: Hello = ::core::default::Default::default();
//!         if let Some(v) = attrs.get("greeting") { value.greeting = v.into(); }
//!         world.spawn(value).id()
//!     }
//! }
//!
//! /// Plugin registering `Hello` with the App.
//! #[derive(Default, Debug, Clone, Copy)]
//! pub struct HelloPlugin;
//!
//! impl ::lumen_core::app::Plugin for HelloPlugin {
//!     fn name(&self) -> &'static str { "HelloPlugin" }
//!     fn build(self, _app: &mut ::lumen_core::app::App) {
//!         // Widget-specific systems remain hand-written: authors
//!         // call `app.add_systems(TickStage::Systems, my_system)`
//!         // alongside `app.add_plugin(HelloPlugin)`. Future
//!         // iterations may walk `#[widget(systems = "...")]` here.
//!     }
//! }
//! ```
//!
//! ## Attribute reference
//!
//! - `#[widget(tag = "name")]` on the struct (required) - the markup
//!   tag handled by this widget.
//! - `#[widget(name = "Hello")]` on the struct (optional) - overrides
//!   the display name returned by [`lumen_widget::Widget::name`].
//! - `#[widget(plugin = "MyExistingPlugin")]` on the struct (optional):
//!   skip emitting a fresh Plugin struct; the author is providing their
//!   own. The derive only emits the `Widget` impl in that case.
//! - `#[widget(prop)]` on a field - parse from the attribute bag in
//!   `Widget::spawn`. Field type must implement [`std::str::FromStr`]
//!   OR be a `String` (special-cased: copied verbatim).
//! - `#[widget(state)]` on a field - leave at the struct's `Default`
//!   value; never read from the attribute bag. Marker.
//!
//! ## What's NOT covered in v1
//!
//! - Per-widget system registration. Authors still call
//!   `app.add_systems(...)` next to `app.add_plugin(WidgetPlugin)`.
//! - Parser integration. The lumenc HTML parser ships a hard-coded
//!   `KNOWN_TAGS` whitelist; emitting an `inventory`-style
//!   registration would be ignored today. The `Widget::parser_tag`
//!   method documents the intended tag for the future runtime
//!   registry.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Attribute, Data, DataStruct, DeriveInput, Field, Fields, Lit, Type, parse_macro_input};

/// The derive entry point. See the crate-level docs for the full
/// surface.
#[proc_macro_derive(Widget, attributes(widget))]
pub fn derive_widget(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

#[derive(Default)]
struct StructAttrs {
    tag: Option<String>,
    name_override: Option<String>,
    /// When set, skip emitting a Plugin struct; the author provides one.
    plugin_override: Option<String>,
}

#[derive(Default)]
struct FieldKind {
    is_prop: bool,
    is_state: bool,
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let ident = input.ident.clone();
    let display_name = ident.to_string();

    let struct_attrs = parse_struct_attrs(&input.attrs)?;
    let tag = struct_attrs.tag.clone().ok_or_else(|| {
        syn::Error::new_spanned(
            &ident,
            "#[derive(Widget)] requires #[widget(tag = \"...\")] on the struct",
        )
    })?;
    let widget_name = struct_attrs
        .name_override
        .clone()
        .unwrap_or(display_name.clone());

    let fields = match &input.data {
        Data::Struct(DataStruct {
            fields: Fields::Named(named),
            ..
        }) => named.named.iter().collect::<Vec<_>>(),
        Data::Struct(DataStruct {
            fields: Fields::Unit,
            ..
        }) => Vec::new(),
        _ => {
            return Err(syn::Error::new_spanned(
                &ident,
                "#[derive(Widget)] only supports structs with named fields (or unit structs)",
            ));
        }
    };

    let mut prop_writes = Vec::new();
    for f in &fields {
        let kind = parse_field_attrs(&f.attrs)?;
        if kind.is_prop && kind.is_state {
            return Err(syn::Error::new_spanned(
                f,
                "field cannot be both #[widget(prop)] and #[widget(state)]",
            ));
        }
        if !kind.is_prop {
            continue;
        }
        prop_writes.push(emit_prop_write(f)?);
    }

    let widget_impl = quote! {
        impl ::lumen_widget::Widget for #ident {
            fn name() -> &'static str {
                #widget_name
            }
            fn parser_tag() -> &'static str {
                #tag
            }
            fn spawn(
                _parent: ::bevy_ecs::prelude::Entity,
                attrs: &::lumen_widget::Attributes,
                world: &mut ::bevy_ecs::prelude::World,
            ) -> ::bevy_ecs::prelude::Entity {
                let mut value: #ident = ::core::default::Default::default();
                #(#prop_writes)*
                ::bevy_ecs::prelude::World::spawn(world, value).id()
            }
        }

        impl #ident {
            #[doc = concat!(
                "Register the `",
                #tag,
                "` markup tag with the lumenc HTML parser. Call this at app startup before `lumenc::run::run_app` so the parser accepts `<",
                #tag,
                " ...>` instead of rejecting it as `UnknownTag`. Subsequent calls are no-ops.",
            )]
            pub fn register() {
                ::lumen_widget::register_widget_tag(#tag);
            }
        }
    };

    let plugin_tokens = if struct_attrs.plugin_override.is_some() {
        // Author owns the plugin struct - don't shadow it.
        TokenStream2::new()
    } else {
        let plugin_ident = format_ident!("{}Plugin", ident);
        let plugin_name_lit = format!("{plugin_ident}");
        quote! {
            #[doc = concat!(
                "Plugin generated by `#[derive(Widget)]` for `",
                stringify!(#ident),
                "`. Registers the widget with the App. Widget-specific systems remain hand-written - call `app.add_systems(...)` alongside `app.add_plugin(",
                stringify!(#plugin_ident),
                ")`.",
            )]
            #[derive(Default, Debug, Clone, Copy)]
            pub struct #plugin_ident;

            impl ::lumen_core::app::Plugin for #plugin_ident {
                fn name(&self) -> &'static str {
                    #plugin_name_lit
                }
                fn build(self, _app: &mut ::lumen_core::app::App) {
                    // Self-register the markup tag with the lumenc
                    // parser registry so `<#tag ...>` markup is accepted
                    // by parse_html. Idempotent - subsequent calls
                    // (across multiple App rebuilds) are no-ops.
                    <#ident>::register();
                    // Widget-specific systems are author-supplied.
                }
            }
        }
    };

    Ok(quote! {
        #widget_impl
        #plugin_tokens
    })
}

fn parse_struct_attrs(attrs: &[Attribute]) -> syn::Result<StructAttrs> {
    let mut out = StructAttrs::default();
    for attr in attrs {
        if !attr.path().is_ident("widget") {
            continue;
        }
        // We accept any combination of `key = "value"` pairs inside a
        // single #[widget(...)] attribute.
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("tag") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    out.tag = Some(s.value());
                    return Ok(());
                }
                return Err(meta.error("`tag` must be a string literal"));
            }
            if meta.path.is_ident("name") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    out.name_override = Some(s.value());
                    return Ok(());
                }
                return Err(meta.error("`name` must be a string literal"));
            }
            if meta.path.is_ident("plugin") {
                let value = meta.value()?;
                let lit: Lit = value.parse()?;
                if let Lit::Str(s) = lit {
                    out.plugin_override = Some(s.value());
                    return Ok(());
                }
                return Err(meta.error("`plugin` must be a string literal"));
            }
            Err(meta.error("unknown `#[widget(...)]` key on struct"))
        })?;
    }
    Ok(out)
}

fn parse_field_attrs(attrs: &[Attribute]) -> syn::Result<FieldKind> {
    let mut k = FieldKind::default();
    for attr in attrs {
        if !attr.path().is_ident("widget") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("prop") {
                k.is_prop = true;
                return Ok(());
            }
            if meta.path.is_ident("state") {
                k.is_state = true;
                return Ok(());
            }
            Err(meta.error("unknown `#[widget(...)]` key on field - expected `prop` or `state`"))
        })?;
    }
    Ok(k)
}

fn emit_prop_write(field: &Field) -> syn::Result<TokenStream2> {
    let ident = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new_spanned(field, "#[widget(prop)] requires a named field"))?;
    let key = ident.to_string();
    if type_is_string(&field.ty) {
        return Ok(quote! {
            if let Some(__v) = attrs.get(#key) {
                value.#ident = __v.to_string();
            }
        });
    }
    let ty = &field.ty;
    Ok(quote! {
        if let Some(__parsed) = attrs.parse::<#ty>(#key) {
            value.#ident = __parsed;
        }
    })
}

fn type_is_string(ty: &Type) -> bool {
    if let Type::Path(tp) = ty {
        if let Some(seg) = tp.path.segments.last() {
            return seg.ident == "String";
        }
    }
    false
}
