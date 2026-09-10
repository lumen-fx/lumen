//! The `devtools` capability: mount the overlay into the built document.
//!
//! The overlay's markup and stylesheet are parsed through the front end the
//! run was given, so it mounts only when one was (every run from source;
//! never a compiled-artifact run). The parsed tree is spawned as a second
//! root, lifted into the top paint band, tagged so the Elements tab excludes
//! it, and the `--dt-*` custom properties `overlay.css` declares are resolved
//! into [`OverlayPalette`] for the parts the panel draws itself. Failures are
//! logged, never fatal: a broken dev overlay must not take the app down.

use std::sync::Arc;

use bevy_ecs::hierarchy::Children;
use bevy_ecs::prelude::{Entity, World};
use lumen_capability::CapabilityEnv;
use lumen_core::app::App;
use lumen_core::components::Color;
use lumen_ir::css::{MediaContext, Stylesheet, apply_css_with_media};
use lumen_ir::fragment::FragmentTable;
use lumen_scene::source_parser::SourceParser;
use lumen_scene::spawn::{Placeholders, spawn_subtree};

use crate::{DevtoolsPlugin, OVERLAY_CSS, OVERLAY_LMN, OverlayPalette, env_open, mount_marks};

/// Install the subsystem. What the capability crate beside this one
/// registers.
pub fn install(app: &mut App, env: &CapabilityEnv) {
    let Some(parser) = env.provided::<Arc<dyn SourceParser>>() else {
        return;
    };
    let mut ir = match parser.parse_html(OVERLAY_LMN, &FragmentTable::new()) {
        Ok(ir) => ir,
        Err(e) => {
            tracing::warn!("devtools: overlay markup failed to parse: {e}");
            return;
        }
    };
    let sheet = match parser.parse_css(OVERLAY_CSS) {
        Ok(sheet) => {
            let media = MediaContext::default();
            if let Err(e) = apply_css_with_media(&mut ir, &sheet, &media) {
                tracing::warn!("devtools: overlay CSS failed to apply: {e}");
            }
            Some(sheet)
        }
        Err(e) => {
            tracing::warn!("devtools: overlay CSS failed to parse: {e}");
            None
        }
    };

    // The palette goes in before `DevtoolsPlugin` builds, which spawns the
    // highlight box and tooltip off it: every field that finds its `--dt-*`
    // custom property gets that value, every other field keeps
    // `OverlayPalette::default`'s fallback.
    app.world.insert_resource(
        sheet
            .as_ref()
            .map(resolve_overlay_palette)
            .unwrap_or_default(),
    );

    // State, the network-capture ring and sink, the snapshot-schedule tweak,
    // and the per-tick systems.
    app.add_plugin(DevtoolsPlugin);

    // An isolated root, so the app's own stylesheet is left alone.
    let root = spawn_subtree(&mut app.world, &ir.root, None, Placeholders::Unresolved);
    let descendants = collect_subtree(&mut app.world, root);
    // Hidden until F12 unless `LUMEN_DEVTOOLS_OPEN` asks for startup-open.
    mount_marks(&mut app.world, root, &descendants, env_open());

    tracing::info!(
        "devtools: overlay mounted ({} entities); press F12 to toggle",
        descendants.len()
    );
}

/// Resolve [`OverlayPalette`] from `overlay.css`'s `:root` custom properties.
/// A property that is missing, or does not parse as a solid color, leaves
/// that field at [`OverlayPalette::default`]'s fallback rather than failing
/// the whole overlay.
fn resolve_overlay_palette(sheet: &Stylesheet) -> OverlayPalette {
    let fallback = OverlayPalette::default();
    let dt = |name: &str, fallback: Color| -> Color {
        sheet
            .resolve_root_var(name)
            .and_then(|value| lumen_ir::values::parse_color("overlay.css", name, &value).ok())
            .map(Into::into)
            .unwrap_or(fallback)
    };
    OverlayPalette {
        tab_text: dt("dt-tab-text", fallback.tab_text),
        tab_text_active: dt("dt-tab-text-active", fallback.tab_text_active),
        tab_underline: dt("dt-tab-underline", fallback.tab_underline),
        tab_fill_hover: dt("dt-tab-fill-hover", fallback.tab_fill_hover),
        row_fill_hover: dt("dt-row-fill-hover", fallback.row_fill_hover),
        row_fill_selected: dt("dt-row-fill-selected", fallback.row_fill_selected),
        tag_color: dt("dt-tag-color", fallback.tag_color),
        meta_color: dt("dt-meta-color", fallback.meta_color),
        dim_color: dt("dt-dim-color", fallback.dim_color),
        flag_color: dt("dt-flag-color", fallback.flag_color),
        highlight_fill: dt("dt-highlight-fill", fallback.highlight_fill),
        highlight_border: dt("dt-highlight-border", fallback.highlight_border),
        tip_fill: dt("dt-tip-fill", fallback.tip_fill),
        tip_border: dt("dt-tip-border", fallback.tip_border),
        tip_text: dt("dt-tip-text", fallback.tip_text),
    }
}

/// Breadth-first collect `root` and every descendant through `Children`.
fn collect_subtree(world: &mut World, root: Entity) -> Vec<Entity> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let e = out[i];
        i += 1;
        let kids: Vec<Entity> = world
            .get::<Children>(e)
            .map(|c| c.iter().copied().collect())
            .unwrap_or_default();
        out.extend(kids);
    }
    out
}

#[cfg(test)]
mod tests {
    use lumen_core::components::Color;
    use lumen_ir::css::{
        Combinator, CompoundSelector, Declaration, LegacySelectorShim, Origin, PseudoClass, Rule,
        SelectorBuf, Stylesheet,
    };

    use super::resolve_overlay_palette;
    use crate::OverlayPalette;

    /// A `:root { --name: value; ... }` rule - the same shape
    /// `Stylesheet::root_vars` looks for (see its doc comment for why the
    /// `:root` *pseudo-class* and not a `root` tag rule).
    fn root_rule(decls: &[(&str, &str)]) -> Rule {
        Rule {
            selectors: vec![SelectorBuf {
                chain: vec![(
                    Combinator::Subject,
                    CompoundSelector {
                        tag: None,
                        id: None,
                        classes: Vec::new(),
                        pseudo_classes: vec![PseudoClass::Root],
                    },
                )],
            }],
            declarations: decls
                .iter()
                .map(|(name, value)| Declaration {
                    name: name.to_string(),
                    value: value.to_string(),
                    important: false,
                })
                .collect(),
            origin: Origin::Author,
            source_order: 0,
            media: None,
            selector: LegacySelectorShim {
                tag: Some("root".to_string()),
                classes: Vec::new(),
            },
        }
    }

    /// A stylesheet that never defines `--dt-tag-color` resolves that field
    /// to the default palette's fallback, the one Rust-side value the mount
    /// falls back to when the token is missing.
    #[test]
    fn missing_token_falls_back_to_the_default_palette() {
        let sheet = Stylesheet {
            rules: vec![root_rule(&[("--dt-tab-text", "#010203")])],
        };
        let palette = resolve_overlay_palette(&sheet);
        assert_eq!(
            palette.tag_color,
            OverlayPalette::default().tag_color,
            "undefined token keeps the fallback color"
        );
    }

    /// A stylesheet that defines a `--dt-*` token wins over the fallback:
    /// a color changed in `overlay.css` reaches the Rust-drawn half.
    #[test]
    fn defined_token_wins_over_the_default_palette() {
        let sheet = Stylesheet {
            rules: vec![root_rule(&[("--dt-tag-color", "#010203")])],
        };
        let palette = resolve_overlay_palette(&sheet);
        assert_eq!(
            palette.tag_color,
            Color::from_rgba8([0x01, 0x02, 0x03, 0xff])
        );
        assert_ne!(palette.tag_color, OverlayPalette::default().tag_color);
    }

    /// A token whose value does not parse as a color also falls back,
    /// rather than the whole overlay refusing to mount.
    #[test]
    fn unparseable_token_falls_back_to_the_default_palette() {
        let sheet = Stylesheet {
            rules: vec![root_rule(&[("--dt-tag-color", "not-a-color")])],
        };
        let palette = resolve_overlay_palette(&sheet);
        assert_eq!(palette.tag_color, OverlayPalette::default().tag_color);
    }
}
