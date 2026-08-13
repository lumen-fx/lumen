// Alongside parser smoke tests this file names `lumenc::spawn` /
// `lumenc::skins`, which lumenc only exposes under the `dev-run` feature.
// Gate the whole file so a thin (`--no-default-features`) `--all-targets`
// build compiles it out instead of failing on the missing symbols; the
// parser tests still run in the default (dev-run-on) build.
#![cfg(feature = "dev-run")]

//! Smoke tests for the markup parser.

use lumenc::layout_ir::{FlexAxis, LengthSpec, ScrollAxisSpec};
use lumenc::parse_html;
use lumenc::spawn::SpawnIntoWorld;

#[test]
fn empty_root_parses() {
    let ir = parse_html("<root/>").expect("parse");
    assert_eq!(ir.root.tag, "root");
    assert_eq!(ir.root.children.len(), 0);
    // D3: per-tag sizing defaults are UA-origin now - applied at spawn
    // where author CSS / inline attrs left the field unset, no longer
    // baked into the parse-time (inline-origin) attrs.
    assert_eq!(ir.root.attrs.width, None);
    assert_eq!(ir.root.attrs.flex, Some(FlexAxis::Column));
}

#[test]
fn tile_with_attrs() {
    let ir = parse_html(
        r##"<root>
            <tile width="100%" height="80px" bg="#dc4548" radius="16"
                  text="hello" tab-index="3" />
           </root>"##,
    )
    .expect("parse");
    let tile = &ir.root.children[0];
    assert_eq!(tile.tag, "tile");
    assert_eq!(tile.attrs.width, Some(LengthSpec::Percent(100.0)));
    assert_eq!(tile.attrs.height, Some(LengthSpec::Px(80.0)));
    assert_eq!(tile.attrs.radius, Some(16.0));
    assert_eq!(tile.attrs.text.as_deref(), Some("hello"));
    assert_eq!(tile.attrs.tab_index, Some(3));
    let bg = tile.attrs.bg.clone().expect("bg parsed");
    let lumenc::layout_ir::BgSpec::Solid(c) = bg else {
        panic!("expected solid")
    };
    assert!((c.r - 0.86).abs() < 0.02);
}

#[test]
fn scroll_defaults_to_y_axis() {
    let ir = parse_html(r##"<root><scroll/></root>"##).expect("parse");
    let s = &ir.root.children[0];
    assert_eq!(s.attrs.scroll, Some(ScrollAxisSpec::Y));
}

#[test]
fn label_text_from_body() {
    let ir = parse_html(r##"<root><label>Inline text</label></root>"##).expect("parse");
    let l = &ir.root.children[0];
    assert_eq!(l.attrs.text.as_deref(), Some("Inline text"));
}

#[test]
fn unknown_tag_errors() {
    let r = parse_html(r##"<root><nope/></root>"##);
    assert!(matches!(r, Err(lumenc::ParseError::UnknownTag(t, _)) if t == "nope"));
}

#[test]
fn bad_color_errors() {
    let r = parse_html(r##"<root><tile bg="not-hex"/></root>"##);
    assert!(matches!(r, Err(lumenc::ParseError::BadAttribute { name, .. }) if name == "bg"));
}

#[test]
fn css_class_selector_applies() {
    let mut ir = parse_html(r##"<root><tile class="t" /></root>"##).expect("html");
    let css = lumenc::parse_css(".t { width: 100px; bg: #ff0000; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tile = &ir.root.children[0];
    assert_eq!(tile.attrs.width, Some(LengthSpec::Px(100.0)));
    let bg = tile.attrs.bg.clone().expect("bg");
    let lumenc::layout_ir::BgSpec::Solid(c) = bg else {
        panic!("expected solid")
    };
    assert!((c.r - 1.0).abs() < 0.001 && c.g < 0.001);
}

#[test]
fn css_does_not_override_html_inline() {
    let mut ir = parse_html(r##"<root><tile class="t" width="50px" /></root>"##).expect("html");
    let css = lumenc::parse_css(".t { width: 100px; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tile = &ir.root.children[0];
    assert_eq!(tile.attrs.width, Some(LengthSpec::Px(50.0)));
}

#[test]
fn theme_tag_is_now_unknown() {
    // <theme>...</theme> was a parse-time textual substitution layer
    // (`$name` tokens) - removed in alpha5 in favour of CSS `:root { --foo }`
    // + `var(--foo)`. Any leftover authoring of `<theme>` errors as an
    // unknown tag.
    let r = parse_html(r##"<root><theme bg="#000"/></root>"##);
    assert!(matches!(r, Err(lumenc::ParseError::UnknownTag(t, _)) if t == "theme"));
}

#[test]
fn template_expands_with_placeholder_substitution() {
    let ir = parse_html(
        r##"<root>
            <template name="pill">
              <tile width="100px" height="40px" bg="#11aa22" text="{label}" id="pill-{n}"/>
            </template>
            <pill n="1" label="One"/>
            <pill n="2" label="Two"/>
           </root>"##,
    )
    .expect("parse");
    assert_eq!(ir.root.children.len(), 2);
    assert_eq!(ir.root.children[0].attrs.text.as_deref(), Some("One"));
    assert_eq!(ir.root.children[0].attrs.id.as_deref(), Some("pill-1"));
    assert_eq!(ir.root.children[1].attrs.id.as_deref(), Some("pill-2"));
}

#[test]
fn template_with_explicit_use_tag() {
    let ir = parse_html(
        r##"<root>
            <template name="block">
              <tile width="50px" height="50px" bg="#ffffff" text="{x}"/>
            </template>
            <use template="block" x="hi"/>
           </root>"##,
    )
    .expect("parse");
    assert_eq!(ir.root.children[0].attrs.text.as_deref(), Some("hi"));
}

#[test]
fn template_slot_injects_caller_children() {
    // <slot/> placeholder in template body is replaced with the caller's
    // inner XML. Caller children compose any markup.
    let ir = parse_html(
        r##"<root>
            <template name="card">
              <column class="card"><slot/></column>
            </template>
            <card>
              <label text="hello"/>
              <label text="world"/>
            </card>
           </root>"##,
    )
    .expect("parse");
    let card = &ir.root.children[0];
    assert!(card.attrs.classes.iter().any(|c| c == "card"));
    assert_eq!(card.children.len(), 2);
    assert_eq!(card.children[0].attrs.text.as_deref(), Some("hello"));
    assert_eq!(card.children[1].attrs.text.as_deref(), Some("world"));
}

#[test]
fn template_slot_default_when_empty() {
    // <slot default="..."/> falls back to the default value when the
    // caller passes no children (self-closing use).
    let ir = parse_html(
        r##"<root>
            <template name="hello">
              <label text="default-text"><slot default=""/></label>
            </template>
            <hello/>
           </root>"##,
    )
    .expect("parse");
    let label = &ir.root.children[0];
    assert_eq!(label.attrs.text.as_deref(), Some("default-text"));
}

#[test]
fn template_id_namespacing_on_use() {
    // A use with `id="X"` prefixes every inner id with `X:`. Two uses
    // get independent namespaces.
    let ir = parse_html(
        r##"<root>
            <template name="card">
              <column><button id="save"/><button id="cancel"/></column>
            </template>
            <card id="user"/>
            <card id="team"/>
           </root>"##,
    )
    .expect("parse");
    let user = &ir.root.children[0];
    assert_eq!(user.children[0].attrs.id.as_deref(), Some("user:save"));
    assert_eq!(user.children[1].attrs.id.as_deref(), Some("user:cancel"));
    let team = &ir.root.children[1];
    assert_eq!(team.children[0].attrs.id.as_deref(), Some("team:save"));
    assert_eq!(team.children[1].attrs.id.as_deref(), Some("team:cancel"));
}

#[test]
fn template_id_namespacing_skipped_without_use_id() {
    // No `id` on the use -> no prefix; inner ids stay as authored.
    let ir = parse_html(
        r##"<root>
            <template name="card">
              <column><button id="save"/></column>
            </template>
            <card/>
           </root>"##,
    )
    .expect("parse");
    let card = &ir.root.children[0];
    assert_eq!(card.children[0].attrs.id.as_deref(), Some("save"));
}

#[test]
fn template_defaults_fill_omitted_use_attrs() {
    // Defaults declared on <template ...> fill in placeholders the
    // use-site omits; use-site values still win when present.
    let ir = parse_html(
        r##"<root>
            <template name="card" variant="primary" size="md">
              <tile bg="#000000" text="{variant}-{size}"/>
            </template>
            <card variant="danger"/>
            <card/>
           </root>"##,
    )
    .expect("parse");
    assert_eq!(
        ir.root.children[0].attrs.text.as_deref(),
        Some("danger-md"),
        "use-site `variant` wins over default; `size` fills from default"
    );
    assert_eq!(
        ir.root.children[1].attrs.text.as_deref(),
        Some("primary-md"),
        "both attrs from defaults"
    );
}

#[test]
fn template_slot_supports_nested_same_name() {
    // Same-name nesting: <card> containing <card>. Slot capture must
    // depth-count, not stop at the first `</card>`.
    let ir = parse_html(
        r##"<root>
            <template name="card">
              <column class="card"><slot/></column>
            </template>
            <card>
              <card>
                <label text="inner"/>
              </card>
            </card>
           </root>"##,
    )
    .expect("parse");
    let outer = &ir.root.children[0];
    assert!(outer.attrs.classes.iter().any(|c| c == "card"));
    assert_eq!(outer.children.len(), 1);
    let inner = &outer.children[0];
    assert!(inner.attrs.classes.iter().any(|c| c == "card"));
    assert_eq!(inner.children.len(), 1);
    assert_eq!(inner.children[0].attrs.text.as_deref(), Some("inner"));
}

#[test]
fn css_hover_pseudo_routes_to_hover_bg() {
    let mut ir = parse_html(r##"<root><tile class="t" bg="#000000"/></root>"##).expect("html");
    let css = lumenc::parse_css(".t:hover { bg: #ff0000; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tile = &ir.root.children[0];
    let hb = tile.attrs.hover_bg.expect("hover_bg set by :hover rule");
    assert!(hb.r > 0.99 && hb.g < 0.01);
}

#[test]
fn css_active_pseudo_routes_to_press_bg() {
    let mut ir = parse_html(r##"<root><tile class="t"/></root>"##).expect("html");
    let css = lumenc::parse_css(".t:active { bg: #00ff00; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tile = &ir.root.children[0];
    let pb = tile.attrs.press_bg.expect("press_bg set by :active rule");
    assert!(pb.g > 0.99 && pb.r < 0.01);
}

#[test]
fn css_focus_pseudo_outline() {
    let mut ir = parse_html(r##"<root><tile class="t"/></root>"##).expect("html");
    let css = lumenc::parse_css(".t:focus { outline: 2 #0000ff; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tile = &ir.root.children[0];
    let o = tile.attrs.focus_outline.expect("focus_outline set");
    assert_eq!(o.width, 2.0);
    assert!(o.color.b > 0.99);
}

#[test]
fn bind_per_kind_form() {
    let ir = parse_html(r##"<root><label bind-text="counter" text="0"/></root>"##).expect("html");
    let label = &ir.root.children[0];
    let b = label.attrs.bind.as_ref().expect("bind set");
    assert_eq!(b.name, "counter");
    assert_eq!(b.kind, lumenc::layout_ir::BindKind::Text);
}

#[test]
fn dialog_desugars_to_absolute_if_with_hide_mode() {
    let ir =
        parse_html(r##"<root><dialog open="show_settings"><label text="hi"/></dialog></root>"##)
            .expect("html");
    let dlg = &ir.root.children[0];
    assert_eq!(dlg.tag, "dialog");
    assert_eq!(
        dlg.attrs.position,
        Some(lumenc::layout_ir::PositionSpec::Absolute)
    );
    let inset = dlg.attrs.inset.expect("dialog default inset=0");
    assert_eq!(
        (inset.top, inset.right, inset.bottom, inset.left),
        (0.0, 0.0, 0.0, 0.0)
    );
    assert_eq!(dlg.attrs.if_signal.as_deref(), Some("show_settings"));
    assert_eq!(dlg.attrs.if_mode, lumenc::layout_ir::IfModeSpec::Hide);
}

#[test]
fn if_mode_defaults_to_render_and_parses_hide() {
    let default = parse_html(r##"<root><if signal="x"/></root>"##).expect("html");
    let blk = &default.root.children[0];
    assert_eq!(blk.attrs.if_mode, lumenc::layout_ir::IfModeSpec::Render);
    let hide = parse_html(r##"<root><if signal="x" mode="hide"/></root>"##).expect("html");
    let blk = &hide.root.children[0];
    assert_eq!(blk.attrs.if_mode, lumenc::layout_ir::IfModeSpec::Hide);
    let bad = parse_html(r##"<root><if signal="x" mode="nope"/></root>"##);
    assert!(bad.is_err());
}

#[test]
fn bind_checked_and_value_kinds() {
    let ir = parse_html(
        r##"<root>
            <toggle bind-checked="dark"/>
            <slider bind-value="volume" min="0" max="1"/>
           </root>"##,
    )
    .expect("html");
    let toggle = &ir.root.children[0];
    let b = toggle.attrs.bind.as_ref().expect("toggle bind set");
    assert_eq!(b.name, "dark");
    assert_eq!(b.kind, lumenc::layout_ir::BindKind::Checked);
    let slider = &ir.root.children[1];
    let b = slider.attrs.bind.as_ref().expect("slider bind set");
    assert_eq!(b.name, "volume");
    assert_eq!(b.kind, lumenc::layout_ir::BindKind::Value);
}

#[test]
fn bind_disabled_parses_and_coexists_with_other_binds() {
    let ir = parse_html(
        r##"<root>
            <button bind-disabled="locked" text="Save"/>
            <toggle bind-checked="dark" bind-disabled="locked"/>
           </root>"##,
    )
    .expect("html");
    let button = &ir.root.children[0];
    assert_eq!(button.attrs.bind_disabled.as_deref(), Some("locked"));
    // The dedicated slot keeps bind-checked intact on the same element.
    let toggle = &ir.root.children[1];
    assert_eq!(toggle.attrs.bind_disabled.as_deref(), Some("locked"));
    let b = toggle.attrs.bind.as_ref().expect("bind-checked kept");
    assert_eq!(b.kind, lumenc::layout_ir::BindKind::Checked);
    // `$` sugar strips; $self./$parent. forms are rejected.
    let ir = parse_html(r##"<root><button bind-disabled="$locked"/></root>"##).expect("html");
    assert_eq!(
        ir.root.children[0].attrs.bind_disabled.as_deref(),
        Some("locked")
    );
    assert!(parse_html(r##"<root><button bind-disabled="$self.locked"/></root>"##).is_err());
}

#[test]
fn bind_disabled_spawns_binding_and_toggles_marker_live() {
    use bevy_ecs::prelude::*;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::components::{BindDisabled, Disabled};

    let ir = parse_html(r##"<root><button bind-disabled="locked" text="Save"/></root>"##)
        .expect("parse");
    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let _root = ir.spawn_into(&mut world);

    let mut q = world.query_filtered::<Entity, With<BindDisabled>>();
    let button = q.iter(&world).next().expect("BindDisabled spawned");
    assert!(
        world.get::<Disabled>(button).is_none(),
        "enabled until the signal says otherwise"
    );

    // Signal -> disabled: the dirty-gated reader inserts the marker.
    world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_bool("locked", true);
    world
        .run_system_once(lumen_core::signals::apply_disabled_bindings)
        .unwrap();
    assert!(
        world.get::<Disabled>(button).is_some(),
        "truthy signal disables live"
    );

    // Signal -> enabled again.
    world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_bool("locked", false);
    world
        .run_system_once(lumen_core::signals::apply_disabled_bindings)
        .unwrap();
    assert!(
        world.get::<Disabled>(button).is_none(),
        "falsy signal re-enables live"
    );

    // A bind-disabled element carries the runtime `:disabled` patch
    // (default dim) so the swap is reversible.
    let sv = world
        .get::<lumen_primitives::StateVisuals>(button)
        .expect("runtime disabled patch installed");
    assert_eq!(sv.disabled.opacity, Some(0.5));
}

#[test]
fn skin_attribute_captured_on_root() {
    let ir = parse_html(r##"<root skin="default"/>"##).expect("html");
    assert_eq!(ir.skin.as_deref(), Some("default"));
}

#[test]
fn no_skin_leaves_field_none() {
    let ir = parse_html(r##"<root/>"##).expect("html");
    assert!(ir.skin.is_none(), "bare framework defaults to no skin");
}

#[test]
fn default_skin_is_parseable_css() {
    // Regression: the embedded default.css must always parse so the
    // runtime never panics on `<root skin="default">`.
    let css = lumenc::skins::lookup("default").expect("default skin present");
    let _ = lumenc::parse_css(css).expect("default skin parses");
}

#[test]
fn overlay_defaults_position_absolute() {
    let ir = parse_html(r##"<root><overlay/></root>"##).expect("html");
    let ov = &ir.root.children[0];
    assert_eq!(ov.tag, "overlay");
    assert_eq!(
        ov.attrs.position,
        Some(lumenc::layout_ir::PositionSpec::Absolute)
    );
    let inset = ov.attrs.inset.expect("overlay defaults inset=0");
    assert_eq!(inset.top, 0.0);
    assert_eq!(inset.right, 0.0);
    assert_eq!(inset.bottom, 0.0);
    assert_eq!(inset.left, 0.0);
}

#[test]
fn min_max_aspect_ratio_parsed() {
    let ir =
        parse_html(r##"<root><tile min-width="100" max-width="400" aspect-ratio="1.5"/></root>"##)
            .expect("html");
    let t = &ir.root.children[0];
    assert_eq!(
        t.attrs.min_width,
        Some(lumenc::layout_ir::LengthSpec::Px(100.0))
    );
    assert_eq!(
        t.attrs.max_width,
        Some(lumenc::layout_ir::LengthSpec::Px(400.0))
    );
    assert_eq!(t.attrs.aspect_ratio, Some(1.5));
}

#[test]
fn overflow_shorthand_and_per_axis() {
    let ir = parse_html(r##"<root><tile overflow="hidden" overflow-y="scroll"/></root>"##)
        .expect("html");
    let t = &ir.root.children[0];
    assert_eq!(
        t.attrs.overflow,
        Some(lumenc::layout_ir::OverflowSpec::Hidden)
    );
    assert_eq!(
        t.attrs.overflow_y,
        Some(lumenc::layout_ir::OverflowSpec::Scroll)
    );
}

#[test]
fn css_var_resolves_against_root_custom_property() {
    let mut ir = parse_html(r##"<root><tile class="t" /></root>"##).expect("html");
    let css = lumenc::parse_css(
        r##"
        :root { --primary: #ff0000; }
        .t    { bg: var(--primary); }
        "##,
    )
    .expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tile = &ir.root.children[0];
    let bg = tile.attrs.bg.clone().expect("bg resolved via var");
    let lumenc::layout_ir::BgSpec::Solid(c) = bg else {
        panic!("expected solid")
    };
    assert!((c.r - 1.0).abs() < 0.001 && c.g < 0.001);
}

#[test]
fn css_var_with_fallback_uses_default_when_missing() {
    let mut ir = parse_html(r##"<root><tile class="t" /></root>"##).expect("html");
    let css = lumenc::parse_css(".t { bg: var(--undefined, #00ff00); }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tile = &ir.root.children[0];
    let bg = tile.attrs.bg.clone().expect("bg");
    let lumenc::layout_ir::BgSpec::Solid(c) = bg else {
        panic!("expected solid")
    };
    assert!(c.g > 0.99 && c.r < 0.01);
}

#[test]
fn css_var_unknown_without_fallback_warns_and_skips() {
    let mut ir = parse_html(r##"<root><tile class="t" /></root>"##).expect("html");
    let css = lumenc::parse_css(".t { bg: var(--missing); }").expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply recovers");
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].property, "bg");
    // Declaration skipped - bg untouched.
    assert!(ir.root.children[0].attrs.bg.is_none());
}

#[test]
fn css_unknown_pseudo_errors() {
    let r = lumenc::parse_css(".t:never { bg: #fff; }");
    assert!(r.is_err());
}

#[test]
fn css_comments_and_whitespace() {
    let css = lumenc::parse_css(
        r"
        /* whole-rule comment */
        tile {
          /* prop comment */
          radius: 16;
          padding: 8;
        }
        ",
    )
    .expect("css");
    assert_eq!(css.rules.len(), 1);
    let r = &css.rules[0];
    assert_eq!(r.selector.tag.as_deref(), Some("tile"));
    assert_eq!(r.declarations.len(), 2);
}

// ---------------------------------------------------------------------------
// W5.4 - `dir=` / `lang=` markup attributes
// ---------------------------------------------------------------------------

#[test]
fn parser_html_accepts_dir_rtl() {
    parse_html(r##"<root dir="rtl"><tile dir="ltr"/></root>"##).expect("dir=rtl parses");
}

#[test]
fn parser_html_accepts_dir_auto_and_uppercase() {
    parse_html(r##"<root dir="auto"/>"##).expect("auto parses");
    parse_html(r##"<root dir="LTR"/>"##).expect("uppercase parses");
}

#[test]
fn parser_html_rejects_unknown_dir_value() {
    let err = parse_html(r##"<root dir="sideways"/>"##).expect_err("unknown dir rejected");
    let msg = format!("{err}");
    assert!(msg.contains("dir"), "error mentions dir: {msg}");
    assert!(
        msg.contains("ltr") || msg.contains("rtl") || msg.contains("supported"),
        "error mentions valid values: {msg}"
    );
}

#[test]
fn parser_html_accepts_lang_bcp47_tag() {
    parse_html(r##"<root lang="ar-EG"><label lang="en-US">Hi</label></root>"##)
        .expect("lang parses");
}

#[test]
fn parser_html_rejects_empty_lang() {
    let err = parse_html(r##"<root lang=""/>"##).expect_err("empty lang rejected");
    assert!(format!("{err}").contains("lang"));
}

// ---------------------------------------------------------------------------
// W5.5 - CSS logical properties
// ---------------------------------------------------------------------------

#[test]
fn parser_css_accepts_padding_inline_start() {
    let css = lumenc::parse_css(
        r"
        tile {
          padding-inline-start: 8;
          padding-inline-end: 12;
        }
        ",
    )
    .expect("css");
    assert_eq!(css.rules.len(), 1);
    // Each declaration parses without error; the IR side wiring (the
    // actual write into `Edges.inline_start`) lands in a follow-up
    // agent that owns `layout_ir.rs`.
    assert_eq!(css.rules[0].declarations.len(), 2);
}

#[test]
fn parser_css_accepts_margin_and_inset_inline_logical_props() {
    lumenc::parse_css(
        r"
        tile {
          margin-inline-start: 4;
          margin-inline-end: 4;
          inset-inline-start: 0;
          inset-inline-end: 0;
          border-inline-start-width: 1;
        }
        ",
    )
    .expect("css");
}

#[test]
fn parser_css_warns_on_non_numeric_logical_property() {
    // `parse_css` is purely lexical and always succeeds; declaration
    // values are validated when the cascade applies them via
    // `apply_css`. A bad value is skipped with a warning - it must not
    // abort the rest of the stylesheet.
    let css =
        lumenc::parse_css(r"tile { padding-inline-start: foo; radius: 4; }").expect("rule parses");
    let mut ir = parse_html(r##"<root><tile/></root>"##).expect("ir");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply recovers");
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].property, "padding-inline-start");
    assert!(
        warnings[0].message.contains("foo"),
        "warning mentions the bad value: {}",
        warnings[0].message
    );
    // Later declaration in the same rule still applied.
    assert_eq!(ir.root.children[0].attrs.radius, Some(4.0));
}

// ---------------------------------------------------------------------------
// W5.4 / W5.5 - IR plumbing for dir, lang, and logical edges
// ---------------------------------------------------------------------------

#[test]
fn parser_html_dir_attr_lands_in_attributes() {
    let ir = parse_html(r##"<root dir="rtl"><tile dir="ltr"/></root>"##).expect("parse");
    assert_eq!(
        ir.root.attrs.dir,
        Some(lumen_core::components::LayoutDirection::Rtl)
    );
    assert_eq!(
        ir.root.children[0].attrs.dir,
        Some(lumen_core::components::LayoutDirection::Ltr)
    );
}

#[test]
fn parser_html_dir_auto_lands_in_attributes() {
    let ir = parse_html(r##"<root dir="auto"/>"##).expect("parse");
    assert_eq!(
        ir.root.attrs.dir,
        Some(lumen_core::components::LayoutDirection::Auto)
    );
}

#[test]
fn parser_html_lang_attr_lands_in_attributes() {
    let ir =
        parse_html(r##"<root lang="ar-EG"><label lang="en-US">Hi</label></root>"##).expect("parse");
    assert_eq!(ir.root.attrs.lang.as_deref(), Some("ar-EG"));
    assert_eq!(ir.root.children[0].attrs.lang.as_deref(), Some("en-US"));
}

#[test]
fn parser_css_padding_inline_start_writes_edges_ir_field() {
    let mut ir = parse_html(r##"<root><tile class="box"/></root>"##).expect("html");
    let css =
        lumenc::parse_css(r".box { padding-inline-start: 8; padding-block-end: 4; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tile = &ir.root.children[0];
    let edges = tile.attrs.padding.expect("padding edges populated");
    assert_eq!(edges.inline_start, Some(8.0));
    assert_eq!(edges.block_end, Some(4.0));
}

#[test]
fn parser_css_margin_and_inset_logical_edges_land_in_ir() {
    let mut ir = parse_html(r##"<root><tile class="box"/></root>"##).expect("html");
    let css = lumenc::parse_css(
        r".box {
            margin-inline-end: 6;
            inset-block-start: 2;
         }",
    )
    .expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tile = &ir.root.children[0];
    assert_eq!(
        tile.attrs.margin.expect("margin populated").inline_end,
        Some(6.0)
    );
    assert_eq!(
        tile.attrs.inset.expect("inset populated").block_start,
        Some(2.0)
    );
}

#[test]
fn ir_edges_forward_logical_overrides_to_core_edges() {
    let ir_edges = lumenc::layout_ir::Edges {
        left: 1.0,
        right: 2.0,
        top: 3.0,
        bottom: 4.0,
        inline_start: Some(8.0),
        inline_end: Some(12.0),
        block_start: Some(5.0),
        block_end: Some(7.0),
        ..Default::default()
    };
    let core_edges: lumen_core::components::Edges = ir_edges.into();
    assert_eq!(core_edges.left, 1.0);
    assert_eq!(core_edges.right, 2.0);
    assert_eq!(core_edges.top, 3.0);
    assert_eq!(core_edges.bottom, 4.0);
    assert_eq!(core_edges.inline_start, Some(8.0));
    assert_eq!(core_edges.inline_end, Some(12.0));
    assert_eq!(core_edges.block_start, Some(5.0));
    assert_eq!(core_edges.block_end, Some(7.0));
}

#[test]
fn spawn_installs_layout_direction_component_when_dir_set() {
    use bevy_ecs::prelude::*;
    let ir = parse_html(r##"<root dir="rtl"><tile/></root>"##).expect("parse");
    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let root = ir.spawn_into(&mut world);
    let dir = world
        .get::<lumen_core::components::LayoutDirection>(root)
        .copied();
    assert_eq!(dir, Some(lumen_core::components::LayoutDirection::Rtl));
}

#[test]
fn spawn_omits_layout_direction_when_dir_absent() {
    use bevy_ecs::prelude::*;
    let ir = parse_html(r##"<root><tile/></root>"##).expect("parse");
    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let root = ir.spawn_into(&mut world);
    assert!(
        world
            .get::<lumen_core::components::LayoutDirection>(root)
            .is_none(),
        "no LayoutDirection when dir attr absent"
    );
}

#[test]
fn spawn_installs_lang_component_when_lang_set() {
    use bevy_ecs::prelude::*;
    let ir = parse_html(r##"<root><div lang="ar-EG"/></root>"##).expect("parse");
    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let _root = ir.spawn_into(&mut world);
    let mut q = world.query::<&lumen_core::components::Lang>();
    let langs: Vec<String> = q.iter(&world).map(|l| l.0.to_string()).collect();
    assert_eq!(langs, vec!["ar-EG".to_string()]);
}

#[test]
fn spawn_propagates_logical_padding_into_core_edges_via_css() {
    use bevy_ecs::prelude::*;
    let mut ir = parse_html(r##"<root><tile class="box"/></root>"##).expect("html");
    let css = lumenc::parse_css(r".box { padding-inline-start: 8; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let _root = ir.spawn_into(&mut world);
    let mut q = world.query::<&lumen_core::components::Style>();
    let with_logical = q
        .iter(&world)
        .find(|s| s.padding.inline_start.is_some())
        .expect("entity with logical padding");
    assert_eq!(with_logical.padding.inline_start, Some(8.0));
    // Physical sides default to zero - only the logical override was set.
    assert_eq!(with_logical.padding.left, 0.0);
}

#[test]
fn bind_text_dollar_alias_lowers_same_as_bare() {
    // `bind-text="$count"` is opt-in sugar for `bind-text="count"` -
    // the parser strips the leading `$` so the IR is bit-identical.
    let bare = parse_html(r##"<root><label bind-text="count" text="0"/></root>"##).expect("html");
    let dollared =
        parse_html(r##"<root><label bind-text="$count" text="0"/></root>"##).expect("html");
    let bare_bind = bare.root.children[0]
        .attrs
        .bind
        .as_ref()
        .expect("bare bind set");
    let dollared_bind = dollared.root.children[0]
        .attrs
        .bind
        .as_ref()
        .expect("$-prefixed bind set");
    assert_eq!(bare_bind, dollared_bind);
    assert_eq!(dollared_bind.name, "count");
    assert_eq!(dollared_bind.kind, lumenc::layout_ir::BindKind::Text);
    // The per-entity attrs stay None in the named-signal case.
    assert!(dollared.root.children[0].attrs.bind_self_text.is_none());
    assert!(dollared.root.children[0].attrs.bind_parent_text.is_none());
}

#[test]
fn interpolation_dollar_alias_lowers_same() {
    // `{$count}` is opt-in sugar for `{count}` - the parser normalises
    // it during build so the IR text content matches the bare form.
    let bare = parse_html(r##"<root><label>{count}</label></root>"##).expect("html");
    let dollared = parse_html(r##"<root><label>{$count}</label></root>"##).expect("html");
    let bare_text = bare.root.children[0]
        .attrs
        .text
        .as_deref()
        .expect("bare text");
    let dollared_text = dollared.root.children[0]
        .attrs
        .text
        .as_deref()
        .expect("$-prefixed text");
    assert_eq!(bare_text, dollared_text);
    assert_eq!(dollared_text, "{count}");
}

#[test]
fn bind_text_dollar_self_field_lowers_to_bind_self() {
    // `bind-text="$self.title"` routes to the per-entity binding field;
    // the regular named-signal `bind` field stays None.
    let ir = parse_html(r##"<root><label bind-text="$self.title"/></root>"##).expect("html");
    let label = &ir.root.children[0];
    assert_eq!(label.attrs.bind_self_text.as_deref(), Some("title"));
    assert!(label.attrs.bind.is_none());
    assert!(label.attrs.bind_parent_text.is_none());
}

#[test]
fn bind_text_dollar_parent_field_lowers_to_bind_parent() {
    // `bind-text="$parent.title"` routes to the parent-entity binding
    // field; the regular named-signal `bind` field stays None.
    let ir = parse_html(r##"<root><label bind-text="$parent.title"/></root>"##).expect("html");
    let label = &ir.root.children[0];
    assert_eq!(label.attrs.bind_parent_text.as_deref(), Some("title"));
    assert!(label.attrs.bind.is_none());
    assert!(label.attrs.bind_self_text.is_none());
}

#[test]
fn bare_interpolation_records_lint_finding() {
    // Round-8 wave B: every bare `{name}` placeholder produces an
    // info-level `BareInterpolation` finding so authors can migrate
    // to the explicit `{$name}` form one site at a time. The IR /
    // runtime substitution pipeline is unchanged - the placeholder
    // text remains `{count}`.
    use lumenc::layout_ir::{LintKind, LintSeverity};
    let ir = parse_html(r##"<root><label>{count}</label></root>"##).expect("html");
    assert_eq!(
        ir.lint_findings.len(),
        1,
        "expected 1 BareInterpolation finding, got {:?}",
        ir.lint_findings,
    );
    let f = &ir.lint_findings[0];
    assert_eq!(f.kind, LintKind::BareInterpolation);
    assert_eq!(f.severity, LintSeverity::Info);
    assert!(f.line >= 1);
    assert!(f.col >= 1);
    // IR text is unchanged.
    assert_eq!(ir.root.children[0].attrs.text.as_deref(), Some("{count}"));
}

#[test]
fn dollar_interpolation_no_lint_finding() {
    // `{$count}` is the preferred form - no deprecation finding.
    let ir = parse_html(r##"<root><label>{$count}</label></root>"##).expect("html");
    assert!(
        ir.lint_findings.is_empty(),
        "explicit `{{$count}}` should not lint, got {:?}",
        ir.lint_findings,
    );
}

#[test]
fn mixed_text_records_only_bare() {
    // Mixed bare + dollar interpolation in the same text node - only
    // the bare site lints.
    use lumenc::layout_ir::LintKind;
    let ir = parse_html(r##"<root><label>{count} vs {$other}</label></root>"##).expect("html");
    let bare: Vec<_> = ir
        .lint_findings
        .iter()
        .filter(|f| f.kind == LintKind::BareInterpolation)
        .collect();
    assert_eq!(
        bare.len(),
        1,
        "expected 1 bare-interpolation, got {:?}",
        ir.lint_findings,
    );
    // The suggestion targets `count`, not `other`.
    assert_eq!(bare[0].suggest.as_deref(), Some("{$count}"));
}

#[test]
fn unknown_attribute_records_lint_finding() {
    // An attribute the vocabulary has no meaning for is still dropped -
    // forward-compatible markup parses - but it warns, so a typo does
    // not pass review as working markup.
    use lumenc::layout_ir::{LintKind, LintSeverity};
    let ir = parse_html(r##"<root><label tect="hi"/></root>"##).expect("html");
    let unknown: Vec<_> = ir
        .lint_findings
        .iter()
        .filter(|f| f.kind == LintKind::UnknownAttribute)
        .collect();
    assert_eq!(
        unknown.len(),
        1,
        "expected 1 UnknownAttribute finding, got {:?}",
        ir.lint_findings,
    );
    assert_eq!(unknown[0].severity, LintSeverity::Warn);
    assert!(
        unknown[0].message.contains("tect") && unknown[0].message.contains("label"),
        "message names the attribute and the tag: {}",
        unknown[0].message,
    );
    assert!(unknown[0].line >= 1 && unknown[0].col >= 1);
    // The attribute is still dropped: nothing lands in the IR.
    assert!(ir.root.children[0].attrs.text.is_none());
}

#[test]
fn known_attributes_record_no_unknown_finding() {
    use lumenc::layout_ir::LintKind;
    let ir = parse_html(r##"<root skin="macos" frameless="true"><label id="a" class="b" text="hi" width="10"/></root>"##)
        .expect("html");
    assert!(
        !ir.lint_findings
            .iter()
            .any(|f| f.kind == LintKind::UnknownAttribute),
        "spelled-right markup should not lint, got {:?}",
        ir.lint_findings,
    );
}

#[test]
fn typoed_bind_attribute_lints_or_errors() {
    use lumenc::layout_ir::LintKind;
    // A typo in the bind KIND is caught by the `bind-` arm itself and
    // fails the parse - the vocabulary there is closed.
    let err = parse_html(r##"<root><label bind-tex="$title"/></root>"##)
        .expect_err("unknown bind kind is a parse error");
    assert!(format!("{err}").contains("unknown bind kind"), "got {err}",);
    // A typo in the `bind-` prefix itself lands in the catch-all and warns.
    let ir = parse_html(r##"<root><label bnid-text="$title"/></root>"##).expect("html");
    assert!(
        ir.lint_findings
            .iter()
            .any(|f| f.kind == LintKind::UnknownAttribute && f.message.contains("bnid-text")),
        "expected an unknown-attribute finding, got {:?}",
        ir.lint_findings,
    );
}

#[test]
fn markup_event_attribute_lints() {
    // There are no event attributes in markup: `on_click` is a script
    // naming convention, so `on_click="inc"` in a `.lmn` file does
    // nothing at all. The warning is the only thing that says so.
    use lumenc::layout_ir::LintKind;
    let ir =
        parse_html(r##"<root><button id="inc" on_click="inc" text="+"/></root>"##).expect("html");
    assert!(
        ir.lint_findings
            .iter()
            .any(|f| f.kind == LintKind::UnknownAttribute && f.message.contains("on_click")),
        "expected an unknown-attribute finding for on_click, got {:?}",
        ir.lint_findings,
    );
}

#[test]
fn lint_findings_have_correct_suggestion() {
    // The structured suggestion is the explicit-form replacement -
    // `{$count}` for a bare `{count}`. Tools like `lumenc fix` use it
    // verbatim as the apply text.
    let ir = parse_html(r##"<root><label>{count}</label></root>"##).expect("html");
    assert_eq!(ir.lint_findings.len(), 1);
    assert_eq!(ir.lint_findings[0].suggest.as_deref(), Some("{$count}"));
}

// --- Round-8 wave-C: `<for>` body interpolation scoping ------------
//
// Inside a `<for each="$users">...</for>` body, `{row.field}` resolves
// against the per-iteration record (the `<for>` element's
// `ArraySignals` entry) and `{$index}` / `{idx}` resolve to the
// 0-based iteration index. Bare `{name}` inside a `<for>` is now
// flagged as ambiguous - the lint message nudges authors to pick
// `{row.name}` (iteration field) or `{$name}` (global signal)
// explicitly. Resolution stays Global to preserve back-compat.

#[test]
fn for_row_dot_field_lowers_to_row_slot() {
    // `<for each="$users"><label>{row.name}</label></for>` records a
    // `Row("name")` slot on the inner `<label>` so the spawner reads
    // from the iteration record at reconcile time.
    use lumenc::layout_ir::InterpolationSlot;
    let ir = parse_html(r##"<root><for each="$users"><label>{row.name}</label></for></root>"##)
        .expect("html");
    // `<for>` element should not pick up the slot - it lives on the
    // `<label>` child where the placeholder actually appeared.
    let for_el = &ir.root.children[0];
    assert_eq!(for_el.tag, "for");
    let label = &for_el.children[0];
    assert_eq!(label.tag, "label");
    assert_eq!(
        label.interpolations,
        vec![InterpolationSlot::Row("name".to_string())]
    );
    // Wave-A: `$users` parse-time alias means `each=` stores the
    // bare name without the `$`.
    assert_eq!(for_el.attrs.each.as_deref(), Some("users"));
}

#[test]
fn for_dollar_index_lowers_to_row_index() {
    // `{$index}` inside a `<for>` body records a `RowIndex` slot.
    use lumenc::layout_ir::InterpolationSlot;
    let ir = parse_html(r##"<root><for each="$users"><label>{$index}</label></for></root>"##)
        .expect("html");
    let label = &ir.root.children[0].children[0];
    assert_eq!(label.interpolations, vec![InterpolationSlot::RowIndex]);
    // No lint finding for the explicit `$`-prefixed form.
    assert!(
        ir.lint_findings.is_empty(),
        "explicit `{{$index}}` should not lint, got {:?}",
        ir.lint_findings,
    );
}

#[test]
fn for_bare_field_warns_with_row_suggestion() {
    // `<for each="$users"><label>{name}</label></for>` - bare `{name}`
    // inside a `<for>` body emits a `BareInterpolation` finding whose
    // suggest field points at the row form (`{row.name}`) because
    // iteration fields are the common case inside a loop. The
    // message body mentions both `{row.name}` and `{$name}` so the
    // author can pick.
    use lumenc::layout_ir::LintKind;
    let ir = parse_html(r##"<root><for each="$users"><label>{name}</label></for></root>"##)
        .expect("html");
    let bare: Vec<_> = ir
        .lint_findings
        .iter()
        .filter(|f| f.kind == LintKind::BareInterpolation)
        .collect();
    assert_eq!(
        bare.len(),
        1,
        "expected 1 bare-interp finding, got {:?}",
        ir.lint_findings,
    );
    let f = bare[0];
    assert_eq!(f.suggest.as_deref(), Some("{row.name}"));
    assert!(
        f.message.contains("row.name"),
        "message should mention `row.name`, got: {}",
        f.message,
    );
    assert!(
        f.message.contains("$name"),
        "message should mention `$name`, got: {}",
        f.message,
    );
}

#[test]
fn for_global_dollar_inside_loop_no_warn() {
    // `<for each="$users"><label>{$theme}</label></for>` - explicit
    // `$`-prefixed global signal reference inside a `<for>` body. No
    // lint finding because the author already disambiguated.
    let ir = parse_html(r##"<root><for each="$users"><label>{$theme}</label></for></root>"##)
        .expect("html");
    assert!(
        ir.lint_findings.is_empty(),
        "explicit `{{$theme}}` inside `<for>` should not lint, got {:?}",
        ir.lint_findings,
    );
}

#[test]
fn for_row_index_via_idx_alias() {
    // Legacy `{idx}` alias inside a `<for>` body still records a
    // `RowIndex` slot for the spawner, AND emits a
    // `BareInterpolation` finding suggesting `{$index}` so authors
    // can migrate.
    use lumenc::layout_ir::{InterpolationSlot, LintKind};
    let ir = parse_html(r##"<root><for each="$users"><label>{idx}</label></for></root>"##)
        .expect("html");
    let label = &ir.root.children[0].children[0];
    assert_eq!(label.interpolations, vec![InterpolationSlot::RowIndex]);
    let bare: Vec<_> = ir
        .lint_findings
        .iter()
        .filter(|f| f.kind == LintKind::BareInterpolation)
        .collect();
    assert_eq!(
        bare.len(),
        1,
        "expected 1 finding, got {:?}",
        ir.lint_findings
    );
    assert_eq!(bare[0].suggest.as_deref(), Some("{$index}"));
}

#[test]
fn css_checked_pseudo_routes_to_checked_bg() {
    let mut ir = parse_html(r##"<root><toggle class="t"/></root>"##).expect("html");
    let css = lumenc::parse_css(".t:checked { bg: #00ff00; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let toggle = &ir.root.children[0];
    let cb = toggle
        .attrs
        .checked_bg
        .expect("checked_bg set by :checked rule");
    assert!(cb.g > 0.99 && cb.r < 0.01);
    // The base bg slot must be untouched by the state rule.
    assert!(toggle.attrs.bg.is_none());
}

#[test]
fn css_disabled_pseudo_routes_to_disabled_bg() {
    let mut ir = parse_html(r##"<root><button class="b"/></root>"##).expect("html");
    let css = lumenc::parse_css(".b:disabled { bg: #0000ff; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let button = &ir.root.children[0];
    let db = button
        .attrs
        .disabled_bg
        .expect("disabled_bg set by :disabled rule");
    assert!(db.b > 0.99 && db.r < 0.01);
    assert!(button.attrs.bg.is_none());
}

#[test]
fn css_selected_pseudo_routes_to_selected_bg() {
    let mut ir = parse_html(r##"<root><button class="tab-btn"/></root>"##).expect("html");
    let css = lumenc::parse_css(".tab-btn:selected { bg: #ffffff; }").expect("css parses");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let attrs = &ir.root.children[0].attrs;
    // Routed to the dedicated slot; must not leak into the base bg.
    assert!(attrs.bg.is_none());
    let selected_bg = attrs
        .selected_bg
        .expect(":selected bg routes to selected_bg");
    assert!(selected_bg.r > 0.99 && selected_bg.g > 0.99 && selected_bg.b > 0.99);
}

#[test]
fn disabled_attr_spawns_disabled_marker() {
    use bevy_ecs::prelude::*;
    let ir =
        parse_html(r##"<root><button disabled="true" text="Save"/><button text="Ok"/></root>"##)
            .expect("parse");
    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let _root = ir.spawn_into(&mut world);
    let mut q = world.query_filtered::<Entity, With<lumen_core::components::Disabled>>();
    assert_eq!(
        q.iter(&world).count(),
        1,
        "only the disabled button carries the marker"
    );
    // Default disabled look: dimmed via Opacity.
    let disabled_e = q.iter(&world).next().unwrap();
    let op = world
        .get::<lumen_core::components::Opacity>(disabled_e)
        .expect("disabled entity dimmed by default");
    assert!(op.0 < 1.0);
}

#[test]
fn toggle_spawns_knob_child() {
    use bevy_ecs::prelude::*;
    let ir = parse_html(r##"<root><toggle checked="true"/></root>"##).expect("parse");
    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let _root = ir.spawn_into(&mut world);
    let mut toggles = world.query_filtered::<Entity, With<lumen_core::components::Toggleable>>();
    let toggle_e = toggles.iter(&world).next().expect("toggle spawned");
    // Track carries per-state fills and a paintable Visuals.
    assert!(
        world
            .get::<lumen_primitives::ToggleStyle>(toggle_e)
            .is_some()
    );
    assert!(
        world
            .get::<lumen_core::components::Visuals>(toggle_e)
            .is_some()
    );
    let mut knobs =
        world.query_filtered::<(Entity, &bevy_ecs::hierarchy::ChildOf), With<lumen_primitives::ToggleKnob>>();
    let (knob_e, child_of) = knobs.iter(&world).next().expect("knob child spawned");
    assert_eq!(child_of.parent(), toggle_e);
    let style = world
        .get::<lumen_core::components::Style>(knob_e)
        .expect("knob has Style");
    assert_eq!(style.position, lumen_core::components::Position::Absolute);
}

#[test]
fn slider_spawns_thumb_child() {
    use bevy_ecs::prelude::*;
    let ir = parse_html(r##"<root><slider min="0" max="10" value="5"/></root>"##).expect("parse");
    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let _root = ir.spawn_into(&mut world);
    let mut sliders = world.query_filtered::<Entity, With<lumen_core::components::SliderValue>>();
    let slider_e = sliders.iter(&world).next().expect("slider spawned");
    let mut thumbs =
        world.query_filtered::<(Entity, &bevy_ecs::hierarchy::ChildOf), With<lumen_primitives::SliderThumb>>();
    let (thumb_e, child_of) = thumbs.iter(&world).next().expect("thumb child spawned");
    assert_eq!(child_of.parent(), slider_e);
    let vis = world
        .get::<lumen_core::components::Visuals>(thumb_e)
        .expect("thumb paints");
    assert!(vis.fill.is_some());
}

/// `<slider step="...">` lands in `SliderValue.step`; absent = `None`
/// (runtime falls back to `(max - min) / 100` via
/// `SliderValue::step_size`).
#[test]
fn slider_step_attribute_parses_into_slider_value() {
    use bevy_ecs::prelude::*;
    let ir = parse_html(
        r##"<root><slider min="0" max="100" value="30" step="5"/><slider min="0" max="1"/></root>"##,
    )
    .expect("parse");
    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let _root = ir.spawn_into(&mut world);
    let mut sliders = world.query::<&lumen_core::components::SliderValue>();
    let steps: Vec<(Option<f32>, f32)> = sliders
        .iter(&world)
        .map(|s| (s.step, s.step_size()))
        .collect();
    assert!(
        steps.contains(&(Some(5.0), 5.0)),
        "authored step=\"5\" parses through to SliderValue ({steps:?})"
    );
    assert!(
        steps.contains(&(None, 0.01)),
        "no step attr -> None, step_size falls back to (max-min)/100 ({steps:?})"
    );
}

/// End-to-end: a `.tab-btn:selected { bg }` rule applied ahead of spawn
/// must land on every synthesised tab-strip button's `TabButtonStyle`,
/// and each button must carry a paintable `Visuals` for
/// `lumen_primitives::tabs::sync_tab_button_visuals` to swap at runtime.
#[test]
fn tab_strip_button_spawns_with_tab_button_style_from_css() {
    use bevy_ecs::prelude::*;
    let src = r##"<root>
        <tabs bind-value="active">
            <tab name="one" label="One"><label text="One" /></tab>
            <tab name="two" label="Two"><label text="Two" /></tab>
        </tabs>
    </root>"##;
    let mut ir = parse_html(src).expect("parse");
    let css = lumenc::parse_css(".tab-btn:selected { bg: #ff00ff; }").expect("css parses");
    lumenc::apply_css(&mut ir, &css).expect("apply");

    let mut world = World::new();
    world.insert_resource(lumen_core::property_store::PropertyStore::default());
    let _root = ir.spawn_into(&mut world);

    let mut q = world.query::<(
        &lumen_primitives::TabStripButton,
        &lumen_primitives::TabButtonStyle,
        &lumen_core::components::Visuals,
    )>();
    let mut found = 0;
    for (_btn, style, vis) in q.iter(&world) {
        found += 1;
        assert!(
            (style.selected_bg.r - 1.0).abs() < 0.01 && (style.selected_bg.b - 1.0).abs() < 0.01,
            "selected_bg picks up the :selected CSS rule (#ff00ff)"
        );
        assert!(vis.fill.is_some(), "button has a paintable Visuals");
    }
    assert_eq!(
        found, 2,
        "both synthesised tab-strip buttons carry TabButtonStyle"
    );
}

/// Depth-first search for the first descendant (or self) carrying `class`.
fn find_by_class<'a>(
    el: &'a lumenc::layout_ir::Element,
    class: &str,
) -> Option<&'a lumenc::layout_ir::Element> {
    if el.attrs.classes.iter().any(|c| c == class) {
        return Some(el);
    }
    el.children.iter().find_map(|c| find_by_class(c, class))
}

/// R4.3: the expanded default skin now covers dropdown / menu / dialog /
/// tabs / tooltip / scroll in addition to the original button / input /
/// toggle / slider / tile set. This is the zero-`CssWarning` regression
/// gate: every property + selector the skin writes must be in the
/// supported subset (docs/docs/reference/css.md), or this test fails.
#[test]
fn default_skin_applies_to_all_widgets_with_zero_warnings() {
    let src = r##"<root>
        <button text="Go" />
        <input placeholder="Type" />
        <textarea placeholder="Body" />
        <column class="card"><label text="Card body" /></column>
        <toggle checked="true" />
        <slider min="0" max="10" value="5" />
        <tile text="Card" tab-index="0" />
        <tile class="tooltip" text="Save the file" />
        <dropdown bind-value="choice">
            <option value="a" label="A" />
            <option value="b" label="B" />
        </dropdown>
        <menu id="ctx">
            <menuitem id="copy" label="Copy" />
            <separator />
            <menuitem id="paste" label="Paste" />
        </menu>
        <dialog open="show_dialog">
            <column class="dialog-surface">
                <label text="Hi" />
            </column>
        </dialog>
        <tabs bind-value="active_tab">
            <tab name="one" label="One"><label text="One body" /></tab>
            <tab name="two" label="Two"><label text="Two body" /></tab>
        </tabs>
        <tooltip text="Save the file" delay="300">
            <button text="Save" />
        </tooltip>
        <scroll tab-index="0"><label text="Long content" /></scroll>
    </root>"##;

    let css_src = lumenc::skins::lookup("default").expect("default skin present");
    let css = lumenc::parse_css(css_src).expect("default skin parses");

    let mut ir = parse_html(src).expect("html parses");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("skin applies");
    assert!(
        warnings.is_empty(),
        "expected zero CssWarnings applying the default skin, got: {warnings:?}"
    );

    // Spot-check that the widget selectors actually resolved values,
    // not just that nothing warned.
    let button = find_by_class(&ir.root, "dropdown-button")
        .or_else(|| ir.root.children.first())
        .expect("button-like element present");
    assert!(button.attrs.bg.is_some(), "button gets a default fill");

    let dropdown_panel =
        find_by_class(&ir.root, "dropdown-panel").expect("dropdown-panel element present");
    assert!(dropdown_panel.attrs.bg.is_some());
    assert!(!dropdown_panel.attrs.shadows.is_empty());

    let menu_panel = find_by_class(&ir.root, "menu-panel").expect("menu-panel element present");
    assert!(menu_panel.attrs.bg.is_some());
    assert!(!menu_panel.attrs.shadows.is_empty());

    let menu_separator =
        find_by_class(&ir.root, "menu-separator").expect("menu-separator element present");
    assert!(menu_separator.attrs.bg.is_some());

    let tab_strip = find_by_class(&ir.root, "tab-strip").expect("tab-strip element present");
    assert!(tab_strip.attrs.bg.is_some());

    // R-css-flex: the default chrome now includes real borders.
    let input = ir
        .root
        .children
        .iter()
        .find(|c| c.tag == "input")
        .expect("input present");
    assert!(
        input.attrs.effective_border().is_some(),
        "input gets a resting 1px border from the skin"
    );
    assert!(
        input.attrs.hover_border.is_some(),
        "input:hover routes a border swap"
    );
    let textarea = ir
        .root
        .children
        .iter()
        .find(|c| c.tag == "textarea")
        .expect("textarea present");
    assert!(textarea.attrs.effective_border().is_some());
    let card = find_by_class(&ir.root, "card").expect("card element present");
    assert!(card.attrs.effective_border().is_some());
    assert!(
        find_by_class(&ir.root, "dropdown-panel")
            .expect("dropdown-panel")
            .attrs
            .effective_border()
            .is_some(),
        "dropdown panel gets a border"
    );

    let tab_btn = find_by_class(&ir.root, "tab-btn").expect("tab-btn element present");
    assert!(tab_btn.attrs.bg.is_some());
    assert!(
        tab_btn.attrs.selected_bg.is_some(),
        ".tab-btn:selected {{ bg }} must route to Attributes::selected_bg"
    );

    let tooltip = find_by_class(&ir.root, "tooltip").expect("tooltip element present");
    assert!(tooltip.attrs.bg.is_some());
    assert!(!tooltip.attrs.shadows.is_empty());

    let dialog_surface =
        find_by_class(&ir.root, "dialog-surface").expect("dialog-surface element present");
    assert!(dialog_surface.attrs.bg.is_some());
    assert!(!dialog_surface.attrs.shadows.is_empty());

    // The `<dialog>` tag itself is the full-screen backdrop; it should
    // pick up the scrim fill from the bare `dialog { bg }` rule.
    let dialog_backdrop = ir
        .root
        .children
        .iter()
        .find(|c| c.tag == "dialog")
        .expect("dialog element present");
    assert!(dialog_backdrop.attrs.bg.is_some());

    // Re-run under an explicit dark-mode `MediaContext` to exercise the
    // `@media (prefers-color-scheme: dark)` token-override block too -
    // it must resolve just as cleanly as the light/default pass.
    let mut ir_dark = parse_html(src).expect("html parses");
    let dark_ctx = lumenc::parser_css::MediaContext {
        color_scheme: Some(lumenc::parser_css::ColorSchemePreference::Dark),
        ..Default::default()
    };
    let warnings_dark = lumenc::parser_css::apply_css_with_media(&mut ir_dark, &css, &dark_ctx)
        .expect("skin applies under dark MediaContext");
    assert!(
        warnings_dark.is_empty(),
        "expected zero CssWarnings under dark MediaContext, got: {warnings_dark:?}"
    );
}

// ---------------------------------------------------------------------------
// D3 (task #29) - UA-default origin: per-tag sizing defaults are a true
// user-agent layer, folded into the cascade beneath any skin and beneath
// author CSS. Author CSS must therefore beat them.
//
// Skin-tokens follow-up: the sizing floor moved out of Rust
// (`apply_ua_style_defaults` used to set it unconditionally at spawn,
// regardless of whether any cascade had run) and into
// `lumen_runtime::skins::UA` (`ua.css`), which only a real cascade pass
// applies. `parse_html` + `lumenc::apply_css` on a bare author sheet -
// what both tests below used to do - never touches `ua.css`; only
// `run::build_app` (what `lumenc run` / `lumenc build` / hot-reload all
// funnel through, via `load_ir`) or `compile_app` / `compile_dir_to_lmna`
// do. `parse_alone_leaves_ua_sizing_unset` below documents the old
// (still-correct) parse-only behavior; these two now drive the real
// pipeline so they prove what their names claim instead of coincidentally
// passing - `author_css_min_width_beats_ua_default_task29`'s `min-height`
// assertion, in particular, used to look like it proved "author beats UA"
// while never actually letting UA into the cascade at all, since `button`
// carries no UA `min-width` for the `min-width` half to contend with, and
// bypassing `ua.css` entirely mooted the `min-height` half too.
// ---------------------------------------------------------------------------

/// Build a full app from inline markup (+ optional CSS) through the real
/// `run::build_app` pipeline - the function `lumenc run` / `lumenc build`
/// both funnel through (via `load_ir`), and the only path that folds
/// `ua.css` into the cascade. No window plugin, no ticks: `Style` is
/// resolved at spawn, before any system runs, so the tree is already
/// final the instant `build_app` returns.
fn build_ua_test_app(markup: &str, css: Option<&str>) -> lumen_core::app::App {
    let dir = std::env::temp_dir().join(format!("lumenc_ua_defaults_{}_{}", std::process::id(), {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }));
    std::fs::create_dir_all(&dir).expect("mkdir");
    // Same rationale as `run_pipeline.rs`'s helpers: disable the MCP
    // server so parallel test threads don't collide on a shared port.
    std::fs::write(dir.join("lumen.toml"), "[mcp]\nport = 0\n").expect("write lumen.toml");
    let mut opts = lumenc::RunOptions::new(&dir)
        .with_parser(lumenc::default_parser())
        .with_markup(markup.to_string());
    if let Some(css) = css {
        opts = opts.with_css(css.to_string());
    }
    let (app, _winit) = lumenc::run::build_app(opts).expect("build_app");
    let _ = std::fs::remove_dir_all(&dir);
    app
}

#[test]
fn author_css_min_width_beats_ua_default_task29() {
    let mut app = build_ua_test_app(
        r##"<root><button text="Save"/></root>"##,
        Some("button { min-width: 40; min-height: 20; }"),
    );
    let mut q = app.world.query::<(
        &lumen_core::components::Style,
        &lumen_core::components::TextContent,
    )>();
    let (style, _) = q
        .iter(&app.world)
        .find(|(_, t)| t.0 == "Save")
        .expect("button entity");
    assert_eq!(
        style.min_width,
        lumen_core::components::Length::Px(40.0),
        "author CSS min-width must win over the UA layer"
    );
    assert_eq!(
        style.min_height,
        lumen_core::components::Length::Px(20.0),
        "author CSS min-height must win over the UA 36px tap floor"
    );
}

#[test]
fn ua_defaults_fill_only_unset_fields() {
    use bevy_ecs::prelude::*;
    let mut app = build_ua_test_app(r##"<root><toggle/><button text="Go"/></root>"##, None);

    // Toggle has no text to measure -> UA supplies concrete track dims.
    let mut toggles = app
        .world
        .query_filtered::<&lumen_core::components::Style, With<lumen_core::components::Toggleable>>(
        );
    let toggle_style = toggles.iter(&app.world).next().expect("toggle entity");
    assert_eq!(
        toggle_style.height,
        lumen_core::components::Length::Px(36.0)
    );
    assert_eq!(
        toggle_style.min_width,
        lumen_core::components::Length::Px(96.0)
    );

    // Button text is measured (W2.5): no fixed height, no min-width -
    // only the tap-size min-height floor.
    let mut q = app.world.query::<(
        &lumen_core::components::Style,
        &lumen_core::components::TextContent,
    )>();
    let (style, _) = q
        .iter(&app.world)
        .find(|(_, t)| t.0 == "Go")
        .expect("button entity");
    assert_eq!(
        style.height,
        lumen_core::components::Length::Auto,
        "button height comes from measured text, not a UA constant"
    );
    assert_eq!(
        style.min_width,
        lumen_core::components::Length::Auto,
        "the old 96px UA min-width is gone (text measurement provides intrinsic size)"
    );
    assert_eq!(style.min_height, lumen_core::components::Length::Px(36.0));
}

/// What `parse_html` alone still does and does not do: it never touches
/// `ua.css` (that only folds in via a real cascade pass - `build_app` /
/// `compile_app` / `compile_dir_to_lmna`), so a bare parse leaves UA-only
/// sizing fields unset. This is correct, current behavior, not a gap -
/// see `ua_defaults_fill_only_unset_fields` above for proof the defaults
/// apply once the real pipeline runs.
#[test]
fn parse_alone_leaves_ua_sizing_unset() {
    let ir = parse_html(r##"<root><toggle/></root>"##).expect("html");
    let toggle = &ir.root.children[0];
    assert_eq!(toggle.attrs.height, None);
    assert_eq!(toggle.attrs.min_width, None);
}

// ---------------------------------------------------------------------------
// D2 / D5 - `<if mode="hide">` flows through Display::None and touches
// Style / Visible only on an actual transition.
// ---------------------------------------------------------------------------

#[test]
fn if_hide_uses_display_none_and_writes_only_on_transition() {
    use bevy_ecs::prelude::*;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::components::{Display, Style, Visible};

    let mut world = World::new();
    let mut store = lumen_core::property_store::PropertyStore::default();
    store.set_global_str("open", "1");
    world.insert_resource(store);
    let block = world
        .spawn((
            Style::default(),
            lumenc::spawn::IfMarker {
                signal_name: "open".into(),
                body: Vec::new(),
                currently_mounted: false,
                mode: lumenc::spawn::IfMode::Hide,
                eq: None,
                saved_display: Display::Flex,
                applied_visible: None,
            },
        ))
        .id();

    // Truthy: shown, display untouched.
    world
        .run_system_once(lumenc::spawn::reconcile_if_blocks)
        .unwrap();
    assert_eq!(world.get::<Visible>(block), Some(&Visible(true)));
    assert!(matches!(
        world.get::<Style>(block).unwrap().display,
        Display::Flex
    ));

    // Steady tick: nothing may be re-written (D5 - the per-tick insert
    // kept FrameDirty permanently hot).
    let style_tick = world
        .entity(block)
        .get_ref::<Style>()
        .unwrap()
        .last_changed();
    let vis_tick = world
        .entity(block)
        .get_ref::<Visible>()
        .unwrap()
        .last_changed();
    world
        .run_system_once(lumenc::spawn::reconcile_if_blocks)
        .unwrap();
    assert_eq!(
        world
            .entity(block)
            .get_ref::<Style>()
            .unwrap()
            .last_changed(),
        style_tick,
        "steady tick must not touch Style"
    );
    assert_eq!(
        world
            .entity(block)
            .get_ref::<Visible>()
            .unwrap()
            .last_changed(),
        vis_tick,
        "steady tick must not re-insert Visible"
    );

    // Falsy transition: space released via Display::None + render/input
    // gated via Visible(false).
    world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_str("open", "");
    world
        .run_system_once(lumenc::spawn::reconcile_if_blocks)
        .unwrap();
    assert_eq!(world.get::<Visible>(block), Some(&Visible(false)));
    assert!(matches!(
        world.get::<Style>(block).unwrap().display,
        Display::None
    ));

    // Show again: prior display restored (section 17.4 test matrix c).
    world
        .resource_mut::<lumen_core::property_store::PropertyStore>()
        .set_global_str("open", "1");
    world
        .run_system_once(lumenc::spawn::reconcile_if_blocks)
        .unwrap();
    assert_eq!(world.get::<Visible>(block), Some(&Visible(true)));
    assert!(matches!(
        world.get::<Style>(block).unwrap().display,
        Display::Flex
    ));
}

// ---------------------------------------------------------------------------
// D4 - virtualized `<for>` writes its pinned Style only when it differs.
// ---------------------------------------------------------------------------

#[test]
fn virtualized_for_inserts_style_only_when_changed() {
    use bevy_ecs::prelude::*;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::components::Style;

    let mut world = World::new();
    let mut arrays = lumen_core::signals::ArraySignals::default();
    let rows: Vec<lumen_core::signals::ArrayItem> = (0..3)
        .map(|i| {
            let mut m = lumen_core::signals::ArrayItem::default();
            m.insert("label".to_string(), format!("row {i}"));
            m
        })
        .collect();
    arrays.set("rows", rows);
    world.insert_resource(arrays);
    world.insert_resource(lumen_core::property_store::PropertyStore::default());

    let row_tmpl = lumenc::layout_ir::Element {
        tag: "row".to_string(),
        ..Default::default()
    };
    let for_block = world
        .spawn((
            Style::default(),
            lumenc::spawn::ForMarker {
                array_name: "rows".into(),
                body: vec![row_tmpl],
                key_field: None,
                cached_keys: Vec::new(),
                virtualized: true,
                row_height: 20.0,
                win_rows: Vec::new(),
                cascaded_body: None,
            },
        ))
        .id();

    // First run pins the for-block Style (width 100%, height = rowsx20).
    world
        .run_system_once(lumenc::spawn::reconcile_for_blocks)
        .unwrap();
    let style = world.get::<Style>(for_block).unwrap();
    assert_eq!(style.height, lumen_core::components::Length::Px(60.0));
    let style_tick = world
        .entity(for_block)
        .get_ref::<Style>()
        .unwrap()
        .last_changed();

    // Steady run: identical computed style => no insert, no Changed
    // (D4 - this was the per-tick relayout loop on virtualized fors).
    world
        .run_system_once(lumenc::spawn::reconcile_for_blocks)
        .unwrap();
    assert_eq!(
        world
            .entity(for_block)
            .get_ref::<Style>()
            .unwrap()
            .last_changed(),
        style_tick,
        "steady tick must not re-insert the for-block Style"
    );

    // Row count change => the pinned height differs => one write.
    {
        let mut arrays = world.resource_mut::<lumen_core::signals::ArraySignals>();
        let rows: Vec<lumen_core::signals::ArrayItem> = (0..5)
            .map(|_| lumen_core::signals::ArrayItem::default())
            .collect();
        arrays.set("rows", rows);
    }
    world
        .run_system_once(lumenc::spawn::reconcile_for_blocks)
        .unwrap();
    assert_eq!(
        world.get::<Style>(for_block).unwrap().height,
        lumen_core::components::Length::Px(100.0)
    );
}

/// RC5 / spec section 8: a standalone `<menu>` desugars to a *vertical,
/// absolutely-positioned* overlay panel. The synthesized panel used to
/// carry default attrs (flex: Row, in-flow), so menu items laid out
/// inline horizontally in the document instead of stacking in a
/// floating panel.
#[test]
fn menu_panel_is_vertical_absolute_overlay() {
    let ir = parse_html(
        r##"<root>
            <menu id="ctx">
                <menuitem id="copy" label="Copy" />
                <separator />
                <menuitem id="paste" label="Paste" />
            </menu>
        </root>"##,
    )
    .expect("parse");
    // <menu> collapses to an <if eq="true" mode="hide"> gate...
    let if_block = &ir.root.children[0];
    assert_eq!(if_block.tag, "if");
    assert_eq!(
        if_block.attrs.if_signal.as_deref(),
        Some("__menu_open:ctx"),
        "menu gate keys on the synthetic open signal"
    );
    // ...whose single child is the panel overlay.
    let panel = &if_block.children[0];
    assert_eq!(panel.tag, "overlay");
    assert!(
        panel.attrs.classes.iter().any(|c| c == "menu-panel"),
        "panel carries .menu-panel for the skin"
    );
    assert_eq!(
        panel.attrs.flex,
        Some(FlexAxis::Column),
        "menu items stack vertically"
    );
    assert_eq!(
        panel.attrs.position,
        Some(lumenc::layout_ir::PositionSpec::Absolute),
        "panel is an overlay, not in-flow"
    );
    let inset = panel.attrs.inset.expect("panel inset set");
    assert_eq!(inset.left, 0.0);
    assert_eq!(inset.top, 0.0);
    assert!(
        inset.right.is_nan() && inset.bottom.is_nan(),
        "right/bottom auto so the panel shrink-wraps its items"
    );
    assert_eq!(
        panel.attrs.popup_panel.as_deref(),
        Some("__menu_open:ctx"),
        "panel wired into the popup dismissal machinery"
    );
    // Items keep their synthesized markers.
    let items: Vec<_> = panel
        .children
        .iter()
        .filter(|c| c.attrs.menu_item.is_some())
        .collect();
    assert_eq!(items.len(), 2, "one MenuItemButton per <menuitem>");
}

/// RC5 end-to-end (ECS side): a `<dropdown>` inside a `<tab>` body -
/// i.e. mounted through the reconciler template path - opens on a
/// header click, and an option click commits the value and closes the
/// panel. Before the spawn-path unification the header/options spawned
/// without their `DropdownButton` / `DropdownOptionButton` markers, so
/// clicks were inert.
#[test]
fn dropdown_inside_tab_opens_and_commits_on_clicks() {
    use bevy_ecs::prelude::*;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::input::{ClickEvent, PointerButton};
    use lumen_core::property_store::PropertyStore;

    let ir = parse_html(
        r##"<root>
            <tabs bind-value="active">
                <tab name="widgets" label="Widgets">
                    <dropdown bind-value="weight" placeholder="Select...">
                        <option value="light" label="Light" />
                        <option value="medium" label="Medium" />
                    </dropdown>
                </tab>
            </tabs>
        </root>"##,
    )
    .expect("parse");
    let mut world = World::new();
    world.insert_resource(PropertyStore::default());
    world.init_resource::<bevy_ecs::message::Messages<ClickEvent>>();
    ir.spawn_into(&mut world);

    // Tick 1: the tab gate (seeded to the first tab) mounts its body,
    // which contains the dropdown header + panel gate.
    world
        .run_system_once(lumenc::spawn::reconcile_if_blocks)
        .unwrap();
    let header = world
        .query::<(Entity, &lumen_primitives::DropdownButton)>()
        .iter(&world)
        .next()
        .expect("dropdown header inside a tab body carries DropdownButton")
        .0;

    // Click the header -> the open signal flips true.
    world
        .resource_mut::<bevy_ecs::message::Messages<ClickEvent>>()
        .write(ClickEvent {
            entity: header,
            position: glam::Vec2::ZERO,
            button: PointerButton::Primary,
        });
    world
        .run_system_once(lumen_primitives::tabs::dispatch_dropdown_clicks)
        .unwrap();
    assert_eq!(
        world
            .resource::<PropertyStore>()
            .get_global_bool("__dropdown_open:weight"),
        Some(true),
        "header click opens the panel"
    );

    // Tick 2: the panel gate mounts the options.
    world
        .run_system_once(lumenc::spawn::reconcile_if_blocks)
        .unwrap();
    let light = world
        .query::<(Entity, &lumen_primitives::DropdownOptionButton)>()
        .iter(&world)
        .find(|(_, o)| o.value == "light")
        .expect("options carry DropdownOptionButton")
        .0;

    // Click the option -> value committed, panel closed.
    world
        .resource_mut::<bevy_ecs::message::Messages<ClickEvent>>()
        .clear();
    world
        .resource_mut::<bevy_ecs::message::Messages<ClickEvent>>()
        .write(ClickEvent {
            entity: light,
            position: glam::Vec2::ZERO,
            button: PointerButton::Primary,
        });
    world
        .run_system_once(lumen_primitives::tabs::dispatch_dropdown_clicks)
        .unwrap();
    let store = world.resource::<PropertyStore>();
    assert_eq!(
        store.get_global_str("weight").as_deref(),
        Some("light"),
        "option click commits the value"
    );
    assert_eq!(
        store.get_global_bool("__dropdown_open:weight"),
        Some(false),
        "option click closes the panel"
    );
}

/// Dialog-open latency ("the dialog is very slow"): the tick that
/// writes the dialog's open signal must (a) leave the mount to at most
/// ONE follow-up tick, and (b) actually schedule that follow-up tick.
///
/// (b) was the live bug: a bare `PropertyStore` write raises neither
/// `FrameDirty` nor `AnimationsActive`, so the window backend's
/// `work_pending` re-arm never fired and `reconcile_if_blocks` sat
/// parked until the next incidental input event - measured on
/// widget-garden at ~550 ms (open) to ~4 s (close). The
/// `lumen_primitives::wake` system now raises `AnimationsActive` at
/// end-of-tick whenever the dirty queue is non-empty (before the core
/// clear system wipes it), which the backend already converts into an
/// immediate redraw + tick.
#[test]
fn dialog_open_signal_mounts_within_one_scheduled_tick() {
    use lumen_core::prelude::*;
    use lumen_core::property_store::PropertyStore;
    use lumen_core::render_world::AnimationsActive;
    use lumen_core::tick::TickStage;

    let ir = parse_html(
        r##"<root>
            <dialog open="dialog_open">
                <label text="dialog-body" />
            </dialog>
        </root>"##,
    )
    .expect("parse");

    let mut app = lumen_core::app::App::new();
    // The two systems under test, wired exactly as in the real app:
    // the reconciler in Systems, the reactive wake in A11ySync before
    // the core dirty-clear (which App::new() registers).
    app.add_systems(TickStage::Systems, lumenc::spawn::reconcile_if_blocks);
    app.add_systems(
        TickStage::A11ySync,
        lumen_primitives::wake::request_tick_on_property_writes
            .before(lumen_core::property_store::clear_property_store_dirty),
    );
    // In-tick script-write stand-in: whenever `PendingWrite` is armed,
    // write the open signal during Systems - like a click handler's
    // `signal("dialog_open").set("1")` would via apply_script_commands.
    #[derive(bevy_ecs::resource::Resource, Default)]
    struct PendingWrite(Option<&'static str>);
    app.world.insert_resource(PendingWrite::default());
    fn apply_pending(
        mut pending: bevy_ecs::system::ResMut<PendingWrite>,
        mut store: bevy_ecs::system::ResMut<PropertyStore>,
    ) {
        if let Some(v) = pending.0.take() {
            store.set_global_str("dialog_open", v);
        }
    }
    app.add_systems(TickStage::Systems, apply_pending);

    ir.spawn_into(&mut app.world);

    // `<dialog>` gates in IfMode::Hide (state survives close), so
    // "open" = body mounted AND the gate entity not display:none.
    let dialog_open = |app: &mut lumen_core::app::App| -> bool {
        let mut body = app.world.query::<&TextContent>();
        let mounted = body.iter(&app.world).any(|t| t.0 == "dialog-body");
        let mut gates = app
            .world
            .query::<(&lumen_core::components::Style, &lumenc::spawn::DialogMarker)>();
        let shown = gates
            .iter(&app.world)
            .any(|(s, _)| !matches!(s.display, lumen_core::components::Display::None));
        mounted && shown
    };

    // Idle ticks: nothing mounted, no wake requested (quiescence).
    // The very first tick is init-heavy (default mirrors seed cells),
    // so let the app settle before asserting the idle baseline.
    app.tick();
    app.tick();
    app.tick();
    assert!(!dialog_open(&mut app), "closed dialog has no body");
    assert!(
        !app.world.resource::<AnimationsActive>().get(),
        "idle tick must not request another tick"
    );

    // Click tick: the write lands mid-tick. Whether or not the
    // reconciler observed it this tick, the tick MUST end with a
    // scheduled follow-up so the mount can't wait on unrelated input.
    app.world.resource_mut::<PendingWrite>().0 = Some("1");
    app.tick();
    let mounted_after_write_tick = dialog_open(&mut app);
    if !mounted_after_write_tick {
        assert!(
            app.world.resource::<AnimationsActive>().get(),
            "a tick that wrote the open signal but did not mount must \
             schedule the follow-up tick (dialog-latency regression)"
        );
    }

    // The scheduled follow-up tick: dialog body is mounted now - a
    // total of at most 1 tick after the write tick, i.e. one frame.
    app.tick();
    assert!(
        dialog_open(&mut app),
        "dialog must mount within one tick of the signal write"
    );

    // Close: same contract in reverse.
    app.world.resource_mut::<PendingWrite>().0 = Some("");
    app.tick();
    let unmounted_after_write_tick = !dialog_open(&mut app);
    if !unmounted_after_write_tick {
        assert!(
            app.world.resource::<AnimationsActive>().get(),
            "the close-write tick must schedule the follow-up tick"
        );
    }
    app.tick();
    assert!(
        !dialog_open(&mut app),
        "dialog must unmount within one tick of the close write"
    );
}

// ---------------------------------------------------------------------------
// CSS-flexibility wave: borders / flex completeness / percent units /
// z-index. Parser round-trips + cascade behaviour.
// ---------------------------------------------------------------------------

#[test]
fn css_border_shorthand_parses_and_resolves() {
    let mut ir = parse_html(r##"<root><tile class="card" /></root>"##).expect("html");
    let css = lumenc::parse_css(".card { border: 1px solid #ff0000; }").expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let tile = &ir.root.children[0];
    assert_eq!(
        tile.attrs.border_style,
        Some(lumenc::layout_ir::BorderStyleSpec::Solid)
    );
    let (widths, color) = tile.attrs.effective_border().expect("effective border");
    assert_eq!(widths.top, 1.0);
    assert_eq!(widths.left, 1.0);
    assert!(color.r > 0.99 && color.g < 0.01);
}

#[test]
fn css_border_shorthand_any_token_order_and_style_leniency() {
    // Real CSS accepts width/style/color in any order; Lumen additionally
    // normalises a missing style keyword to `solid` (the IR stores the
    // explicit style so a web transpile emits `2px solid #00ff00`).
    let mut ir = parse_html(r##"<root><tile class="card" /></root>"##).expect("html");
    let css = lumenc::parse_css(".card { border: #00ff00 2px; }").expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let (widths, color) = ir.root.children[0]
        .attrs
        .effective_border()
        .expect("border resolves");
    assert_eq!(widths.top, 2.0);
    assert!(color.g > 0.99);
}

#[test]
fn css_border_none_clears() {
    let mut ir = parse_html(r##"<root><tile class="card" /></root>"##).expect("html");
    let css = lumenc::parse_css(".card { border: 1px solid #fff000; } .card { border: none; }")
        .expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(ir.root.children[0].attrs.effective_border().is_none());
}

#[test]
fn css_border_longhands_accumulate() {
    let mut ir = parse_html(r##"<root><tile class="card" /></root>"##).expect("html");
    let css = lumenc::parse_css(
        ".card { border-width: 1 2 3 4; border-color: #0000ff; border-style: solid; }",
    )
    .expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let (w, c) = ir.root.children[0]
        .attrs
        .effective_border()
        .expect("resolves");
    // CSS TRBL rotation.
    assert_eq!((w.top, w.right, w.bottom, w.left), (1.0, 2.0, 3.0, 4.0));
    assert!(c.b > 0.99);
}

#[test]
fn css_border_longhands_without_style_paint_nothing() {
    // Per CSS Backgrounds & Borders: the computed width of a side whose
    // style is `none` is zero - width+color alone produce no border.
    let mut ir = parse_html(r##"<root><tile class="card" /></root>"##).expect("html");
    let css = lumenc::parse_css(".card { border-width: 2; border-color: #123456; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(ir.root.children[0].attrs.effective_border().is_none());
}

#[test]
fn css_unsupported_border_style_warns_and_skips() {
    let mut ir = parse_html(r##"<root><tile class="card" /></root>"##).expect("html");
    let css = lumenc::parse_css(".card { border: 1px dashed #ffffff; }").expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply recovers");
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].property, "border");
    assert!(ir.root.children[0].attrs.effective_border().is_none());
}

#[test]
fn css_per_side_border_width_longhands() {
    let mut ir = parse_html(r##"<root><tile class="card" /></root>"##).expect("html");
    let css = lumenc::parse_css(
        ".card { border-style: solid; border-color: #333333; border-bottom-width: 1; }",
    )
    .expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let (w, _) = ir.root.children[0]
        .attrs
        .effective_border()
        .expect("resolves");
    assert_eq!((w.top, w.right, w.bottom, w.left), (0.0, 0.0, 1.0, 0.0));
}

#[test]
fn css_state_borders_route_via_pseudo_and_native_props() {
    let mut ir = parse_html(r##"<root><tile class="cell" /></root>"##).expect("html");
    let css = lumenc::parse_css(
        ".cell { border: 1px solid #303030; }
         .cell:hover { border: 1px solid #3d4654; }
         .cell:focus { border: 2px solid #1f6feb; }",
    )
    .expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let attrs = &ir.root.children[0].attrs;
    let hb = attrs.hover_border.expect("hover border");
    assert_eq!(hb.widths.top, 1.0);
    let fb = attrs.focus_border.expect("focus border");
    assert_eq!(fb.widths.top, 2.0);
    // Lumen-native property spellings behave identically (datagrid uses
    // `hover-border:` / `focus-border:` directly).
    let mut ir2 = parse_html(r##"<root><tile class="cell" /></root>"##).expect("html");
    let css2 = lumenc::parse_css(
        ".cell { hover-border: 1px #3d4654; focus-border: 2px #1f6feb; focus-outline: 2 #33c7ce; }",
    )
    .expect("css");
    let warnings2 = lumenc::apply_css(&mut ir2, &css2).expect("apply");
    assert!(warnings2.is_empty(), "unexpected warnings: {warnings2:?}");
    let attrs2 = &ir2.root.children[0].attrs;
    assert!(attrs2.hover_border.is_some());
    assert!(attrs2.focus_border.is_some());
    assert!(attrs2.focus_outline.is_some());
}

#[test]
fn markup_border_attribute_parses() {
    let ir = parse_html(r##"<root><tile border="1px solid #444444" /></root>"##).expect("html");
    let (w, _) = ir.root.children[0]
        .attrs
        .effective_border()
        .expect("resolves");
    assert_eq!(w.top, 1.0);
}

#[test]
fn css_flex_shorthand_forms() {
    use lumenc::layout_ir::LengthSpec;
    let mut ir = parse_html(
        r##"<root><tile class="a" /><tile class="b" /><tile class="c" /><tile class="d" /></root>"##,
    )
    .expect("html");
    let css = lumenc::parse_css(
        ".a { flex: 1; }
         .b { flex: 2 3; }
         .c { flex: 2 3 10px; }
         .d { flex: none; }",
    )
    .expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let a = &ir.root.children[0].attrs;
    assert_eq!(a.grow, Some(1.0));
    assert_eq!(a.shrink, Some(1.0));
    assert_eq!(a.basis, Some(LengthSpec::Percent(0.0)));
    let b = &ir.root.children[1].attrs;
    assert_eq!(b.grow, Some(2.0));
    assert_eq!(b.shrink, Some(3.0));
    let c = &ir.root.children[2].attrs;
    assert_eq!(c.basis, Some(LengthSpec::Px(10.0)));
    let d = &ir.root.children[3].attrs;
    assert_eq!(d.grow, Some(0.0));
    assert_eq!(d.shrink, Some(0.0));
    assert_eq!(d.basis, Some(LengthSpec::Auto));
}

#[test]
fn css_flex_wrap_align_content_direction() {
    use lumenc::layout_ir::{AlignContentSpec, FlexAxis, FlexWrapSpec};
    let mut ir = parse_html(r##"<root><row class="wrapper" /></root>"##).expect("html");
    let css = lumenc::parse_css(
        ".wrapper { flex-wrap: wrap; align-content: space-between; flex-direction: row-reverse; flex-shrink: 0; flex-basis: 25%; }",
    )
    .expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let a = &ir.root.children[0].attrs;
    assert_eq!(a.flex_wrap, Some(FlexWrapSpec::Wrap));
    assert_eq!(a.align_content, Some(AlignContentSpec::SpaceBetween));
    assert_eq!(a.flex, Some(FlexAxis::RowReverse));
    assert_eq!(a.shrink, Some(0.0));
    assert_eq!(a.basis, Some(lumenc::layout_ir::LengthSpec::Percent(25.0)));
}

#[test]
fn css_percent_padding_margin_gap() {
    let mut ir = parse_html(r##"<root><tile class="p" /></root>"##).expect("html");
    let css =
        lumenc::parse_css(".p { padding: 5% 10; margin: 2%; gap: 3%; row-gap: 4; }").expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let a = &ir.root.children[0].attrs;
    let p = a.padding.expect("padding");
    assert_eq!(p.pct_top, Some(5.0));
    assert_eq!(p.pct_bottom, Some(5.0));
    assert_eq!(p.right, 10.0);
    assert_eq!(p.pct_right, None);
    let m = a.margin.expect("margin");
    assert_eq!(m.pct_left, Some(2.0));
    assert_eq!(a.gap_pct, Some(3.0));
    assert_eq!(a.gap_row, Some(4.0));
}

#[test]
fn css_z_index_and_box_sizing() {
    use lumenc::layout_ir::BoxSizingSpec;
    let mut ir = parse_html(r##"<root><tile class="z" /></root>"##).expect("html");
    let css = lumenc::parse_css(".z { z-index: 3; box-sizing: content-box; }").expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let a = &ir.root.children[0].attrs;
    assert_eq!(a.z_index, Some(3));
    assert_eq!(a.box_sizing, Some(BoxSizingSpec::ContentBox));
}

#[test]
fn inline_border_beats_author_css() {
    // Inline origin wins per CSS Cascade: an inline `border=` attribute
    // must survive an author rule for the same element.
    let mut ir = parse_html(r##"<root><tile class="card" border="3px solid #101010" /></root>"##)
        .expect("html");
    let css = lumenc::parse_css(".card { border: 1px solid #ff0000; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let (w, c) = ir.root.children[0]
        .attrs
        .effective_border()
        .expect("resolves");
    assert_eq!(w.top, 3.0);
    assert!(c.r < 0.1, "inline color must win, got {c:?}");
}

#[test]
fn css_focus_outline_plain_property_no_warning() {
    let mut ir = parse_html(r##"<root><tile class="btn" /></root>"##).expect("html");
    let css = lumenc::parse_css(".btn { focus-outline: 2 #33c7ce; }").expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected: {warnings:?}");
    assert!(ir.root.children[0].attrs.focus_outline.is_some());
}

// === W5 form controls: <checkbox> / <radio> / <progress> desugars ===

#[test]
fn checkbox_desugars_to_box_and_label() {
    let ir = parse_html(
        r##"<root><checkbox id="cb" label="Enable" checked="true" bind-checked="on" /></root>"##,
    )
    .expect("html");
    let cb = &ir.root.children[0];
    assert_eq!(cb.tag, "checkbox");
    assert_eq!(cb.attrs.checked, Some(true));
    assert_eq!(cb.attrs.tab_index, Some(0), "checkbox is focusable");
    assert!(cb.attrs.bind.is_some(), "bind-checked survives the desugar");
    assert_eq!(cb.children.len(), 2, "box + label children");
    let bx = &cb.children[0];
    assert_eq!(
        bx.attrs.part,
        Some(lumenc::layout_ir::WidgetPart::CheckboxBox)
    );
    assert_eq!(bx.attrs.classes, vec!["checkbox-box".to_string()]);
    let lbl = &cb.children[1];
    assert_eq!(lbl.attrs.text.as_deref(), Some("Enable"));
    assert_eq!(lbl.attrs.classes, vec!["checkbox-label".to_string()]);
    assert_eq!(
        cb.attrs.text, None,
        "label text moves to the child, not the row"
    );
}

#[test]
fn checkbox_indeterminate_flag_parses() {
    let ir =
        parse_html(r##"<root><checkbox label="X" indeterminate="true" /></root>"##).expect("html");
    assert!(ir.root.children[0].attrs.indeterminate);
}

#[test]
fn radio_desugars_with_group_value_and_roving_tab_index() {
    let ir = parse_html(
        r##"<root><radio group="ship" value="air" label="Air" checked="true" /></root>"##,
    )
    .expect("html");
    let r = &ir.root.children[0];
    assert_eq!(r.attrs.radio_group.as_deref(), Some("ship"));
    assert_eq!(r.attrs.radio_value.as_deref(), Some("air"));
    assert_eq!(
        r.attrs.tab_index,
        Some(-1),
        "roving tabindex: runtime promotes exactly one member to 0"
    );
    assert_eq!(
        r.attrs.signal_seed,
        Some(("ship".to_string(), "air".to_string())),
        "checked seeds the group signal"
    );
    assert_eq!(
        r.children[0].attrs.part,
        Some(lumenc::layout_ir::WidgetPart::RadioDot)
    );
    assert_eq!(r.children[1].attrs.text.as_deref(), Some("Air"));
}

#[test]
fn radio_without_group_errors() {
    let r = parse_html(r##"<root><radio value="a" /></root>"##);
    assert!(r.is_err(), "group + value are mandatory on <radio>");
}

#[test]
fn progress_desugars_to_fill_child() {
    let ir = parse_html(r##"<root><progress value="30" max="100" duration="900" /></root>"##)
        .expect("html");
    let p = &ir.root.children[0];
    assert_eq!(p.tag, "progress");
    assert_eq!(p.attrs.value, Some(30.0));
    assert_eq!(p.attrs.max, Some(100.0));
    assert_eq!(p.attrs.progress_duration, Some(900));
    assert_eq!(p.attrs.tab_index, None, "progress is never focusable");
    let fill = &p.children[0];
    assert_eq!(
        fill.attrs.part,
        Some(lumenc::layout_ir::WidgetPart::ProgressFill)
    );
    assert_eq!(fill.attrs.classes, vec!["progress-fill".to_string()]);
}

// === W5 dialog contract: autofocus + default button ===

#[test]
fn autofocus_and_default_button_parse() {
    let ir = parse_html(
        r##"<root><dialog open="d"><input autofocus="true" /><button default="true" text="OK" /></dialog></root>"##,
    )
    .expect("html");
    let dialog = &ir.root.children[0];
    assert!(dialog.children[0].attrs.autofocus);
    let ok = &dialog.children[1];
    assert!(ok.attrs.default_button);
    assert!(
        ok.attrs.classes.iter().any(|c| c == "default"),
        "default button gains the `default` class for skin styling"
    );
}

#[test]
fn default_class_survives_explicit_class_attr() {
    // `class=` is processed in source order - the parser must append
    // `default` AFTER the attribute loop so it can't be clobbered.
    let ir =
        parse_html(r##"<root><button default="true" class="btn-primary" text="OK" /></root>"##)
            .expect("html");
    let ok = &ir.root.children[0];
    assert!(ok.attrs.classes.iter().any(|c| c == "btn-primary"));
    assert!(ok.attrs.classes.iter().any(|c| c == "default"));
}

// === W5 ellipsis ===

#[test]
fn wrap_ellipsis_attr_sets_text_overflow() {
    let ir = parse_html(r##"<root><label wrap="ellipsis" text="long" /></root>"##).expect("html");
    assert_eq!(
        ir.root.children[0].attrs.text_overflow,
        Some(lumenc::layout_ir::TextOverflowSpec::Ellipsis)
    );
    assert_eq!(
        ir.root.children[0].attrs.text_wrap, None,
        "ellipsis is not a wrap policy - the spawn layer lowers it"
    );
}

#[test]
fn css_text_overflow_ellipsis_applies() {
    let mut ir = parse_html(r##"<root><label class="e" text="long" /></root>"##).expect("html");
    let css = lumenc::parse_css(".e { text-overflow: ellipsis; }").expect("css");
    let warnings = lumenc::apply_css(&mut ir, &css).expect("apply");
    assert!(warnings.is_empty(), "unexpected: {warnings:?}");
    assert_eq!(
        ir.root.children[0].attrs.text_overflow,
        Some(lumenc::layout_ir::TextOverflowSpec::Ellipsis)
    );
}

// === W5 tooltip tokens ===

#[test]
fn tooltip_delay_and_offset_resolve_from_tokens() {
    let mut ir = parse_html(r##"<root><tooltip text="tip"><button text="b" /></tooltip></root>"##)
        .expect("html");
    let css =
        lumenc::parse_css(":root { --lumen-tooltip-delay: 250; --lumen-tooltip-offset: 20; }")
            .expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let trigger = &ir.root.children[0];
    let tip = trigger.attrs.tooltip.as_ref().expect("tooltip spec");
    assert_eq!(tip.delay_ms, Some(250), "token fills the unset delay");
    assert_eq!(tip.offset, Some(20.0), "token fills the unset offset");
}

#[test]
fn tooltip_inline_delay_beats_token() {
    let mut ir = parse_html(
        r##"<root><tooltip text="tip" delay="900"><button text="b" /></tooltip></root>"##,
    )
    .expect("html");
    let css = lumenc::parse_css(":root { --lumen-tooltip-delay: 250; }").expect("css");
    lumenc::apply_css(&mut ir, &css).expect("apply");
    let tip = ir.root.children[0].attrs.tooltip.as_ref().unwrap();
    assert_eq!(tip.delay_ms, Some(900), "inline attr wins over the token");
}

#[test]
fn bind_scroll_parses_and_rejects_entity_forms() {
    let ir = parse_html(
        r##"<root>
            <scroll bind-scroll="feed_pos" height="200"><label text="a"/></scroll>
           </root>"##,
    )
    .expect("html");
    let sc = &ir.root.children[0];
    assert_eq!(sc.attrs.bind_scroll.as_deref(), Some("feed_pos"));

    // `$` sugar strips; $self./$parent. forms are rejected; empty errors.
    let ir = parse_html(r##"<root><scroll bind-scroll="$feed_pos"/></root>"##).expect("html");
    assert_eq!(
        ir.root.children[0].attrs.bind_scroll.as_deref(),
        Some("feed_pos")
    );
    assert!(parse_html(r##"<root><scroll bind-scroll="$self.pos"/></root>"##).is_err());
    assert!(parse_html(r##"<root><scroll bind-scroll="$parent.pos"/></root>"##).is_err());
    assert!(parse_html(r##"<root><scroll bind-scroll=""/></root>"##).is_err());
}

/// W6 T6 pipeline: script sets the signal -> the dirty-gated reader moves
/// the scroll offset (and kills any fling); user scrolling pushes the
/// offset back into the signal ONCE, on settle - never per frame.
#[test]
fn bind_scroll_drives_offset_and_pushes_back_on_settle() {
    use bevy_ecs::prelude::*;
    use bevy_ecs::system::RunSystemOnce;
    use lumen_core::components::BindScroll;
    use lumen_core::input::{Scroll, ScrollOffset};
    use lumen_core::property_store::PropertyStore;

    let ir = parse_html(
        r##"<root><scroll bind-scroll="feed_pos" height="100"><label text="row"/></scroll></root>"##,
    )
    .expect("parse");
    let mut world = World::new();
    world.insert_resource(PropertyStore::default());
    let _root = ir.spawn_into(&mut world);

    let mut q = world.query_filtered::<Entity, (With<BindScroll>, With<ScrollOffset>)>();
    let scroller = q
        .iter(&world)
        .next()
        .expect("<scroll bind-scroll> spawns BindScroll on the Scroll entity");

    // Signal -> offset (pull half): a plain store write scrolls the
    // container on its tick, and cancels in-flight momentum.
    world.get_mut::<Scroll>(scroller).unwrap().velocity = glam::Vec2::new(0.0, 500.0);
    world
        .resource_mut::<PropertyStore>()
        .set_global_str("feed_pos", "240");
    world
        .run_system_once(lumen_core::signals::apply_scroll_bindings)
        .unwrap();
    assert_eq!(
        world.get::<ScrollOffset>(scroller).unwrap().0.y,
        240.0,
        "signal write drives the vertical offset"
    );
    assert_eq!(
        world.get::<Scroll>(scroller).unwrap().velocity,
        glam::Vec2::ZERO,
        "reactive scroll_to cancels the fling"
    );

    // Offset -> signal (push half): a registered system keeps its Local
    // settle latch across runs, mirroring per-tick scheduling.
    let push = world.register_system(lumen_core::signals::push_scroll_to_signal);
    // Run 1: every component looks freshly-added to a new system - the
    // is_added() guard must swallow it (spawn default, not a user edit).
    world.run_system(push).unwrap();
    assert_eq!(
        world
            .resource::<PropertyStore>()
            .get_global_str("feed_pos")
            .as_deref(),
        Some("240"),
        "no push before any user scroll"
    );

    // User scroll: the offset changes on tick N...
    world.get_mut::<ScrollOffset>(scroller).unwrap().0.y = 50.0;
    world.run_system(push).unwrap();
    assert_eq!(
        world
            .resource::<PropertyStore>()
            .get_global_str("feed_pos")
            .as_deref(),
        Some("240"),
        "mid-scroll ticks must NOT push (settle throttle)"
    );

    // ...and stops on tick N+1: exactly one settle push.
    world.run_system(push).unwrap();
    assert_eq!(
        world
            .resource::<PropertyStore>()
            .get_global_str("feed_pos")
            .as_deref(),
        Some("50"),
        "settled offset mirrors into the signal"
    );

    // Echo guard: re-applying the (now equal) signal is a no-op.
    world
        .run_system_once(lumen_core::signals::apply_scroll_bindings)
        .unwrap();
    assert_eq!(world.get::<ScrollOffset>(scroller).unwrap().0.y, 50.0);
}

#[test]
fn boolean_attributes_share_one_truthiness_rule() {
    // Every boolean attribute takes the same set, whichever tag or
    // desugar reads it: `true` / `yes` / `1` / an empty value for
    // true, and `false` / `no` / `0` for false.
    for spelling in ["true", "yes", "1", ""] {
        let value = format!("=\"{spelling}\"");
        let ir = parse_html(&format!(
            r##"<root frameless{value}>
                    <button disabled{value} default{value} text="Go"/>
                    <tile draggable{value} drop{value} drop-target{value} layout-boundary{value}/>
                    <checkbox indeterminate{value} checked{value}/>
                    <input required{value} autofocus{value} multiline{value}/>
                    <for each="rows" virtualized{value}/>
                    <title-bar drag{value}/>
                </root>"##
        ))
        .expect("html");
        assert!(ir.frameless, "frameless with `{spelling}`");
        let btn = &ir.root.children[0];
        assert!(btn.attrs.disabled, "disabled with `{spelling}`");
        assert!(btn.attrs.default_button, "default with `{spelling}`");
        let tile = &ir.root.children[1];
        assert!(tile.attrs.draggable, "draggable with `{spelling}`");
        assert!(tile.attrs.drop_target, "drop-target with `{spelling}`");
        assert!(
            tile.attrs.layout_boundary,
            "layout-boundary with `{spelling}`"
        );
        let check = &ir.root.children[2];
        assert!(check.attrs.indeterminate, "indeterminate with `{spelling}`");
        assert_eq!(check.attrs.checked, Some(true), "checked with `{spelling}`");
        let input = &ir.root.children[3];
        assert!(input.attrs.required, "required with `{spelling}`");
        assert!(input.attrs.autofocus, "autofocus with `{spelling}`");
        assert_eq!(
            input.attrs.multiline,
            Some(true),
            "multiline with `{spelling}`"
        );
        assert!(
            ir.root.children[4].attrs.virtualized,
            "virtualized with `{spelling}`"
        );
        assert!(
            ir.root.children[5].attrs.title_bar_drag,
            "drag with `{spelling}`"
        );
        assert!(
            ir.lint_findings.is_empty(),
            "`{spelling}` must not lint: {:?}",
            ir.lint_findings
        );
    }

    for spelling in ["false", "no", "0"] {
        let ir = parse_html(&format!(
            r##"<root frameless="{spelling}">
                    <button disabled="{spelling}" text="Go"/>
                    <tile draggable="{spelling}" drop="{spelling}"/>
                    <checkbox checked="{spelling}"/>
                </root>"##
        ))
        .expect("html");
        assert!(!ir.frameless, "frameless with `{spelling}`");
        assert!(!ir.root.children[0].attrs.disabled);
        assert!(!ir.root.children[1].attrs.draggable);
        assert!(!ir.root.children[1].attrs.drop_target);
        assert_eq!(ir.root.children[2].attrs.checked, Some(false));
        assert!(
            ir.lint_findings.is_empty(),
            "`{spelling}` must not lint: {:?}",
            ir.lint_findings
        );
    }
}

#[test]
fn unrecognized_boolean_value_warns_and_reads_false() {
    use lumenc::layout_ir::{LintKind, LintSeverity};
    // `draggable` used to be the one boolean that hard-errored on a
    // stray value; it now warns like the rest.
    let ir = parse_html(r##"<root><tile draggable="maybe"/></root>"##).expect("html");
    assert!(!ir.root.children[0].attrs.draggable);
    assert_eq!(ir.lint_findings.len(), 1);
    let f = &ir.lint_findings[0];
    assert_eq!(f.kind, LintKind::BooleanAttribute);
    assert_eq!(f.severity, LintSeverity::Warn);
    assert!(f.message.contains("draggable"), "{}", f.message);
    assert!(f.message.contains("true"), "{}", f.message);
    assert!(f.line >= 1 && f.col >= 1);

    // Case matters, and the rule reaches attributes the desugar passes
    // read off children rather than through the attribute table.
    let ir = parse_html(
        r##"<root>
                <dropdown bind-value="pick" placeholder="Pick">
                    <option value="a" disabled="True"/>
                </dropdown>
                <tabs bind-value="tab"><tab name="one" disabled="on"/></tabs>
            </root>"##,
    )
    .expect("html");
    assert_eq!(ir.lint_findings.len(), 2, "{:?}", ir.lint_findings);
    assert!(
        ir.lint_findings
            .iter()
            .all(|f| f.kind == LintKind::BooleanAttribute)
    );
}

#[test]
fn dropdown_seeds_the_first_option() {
    // Parity with `<tabs>`: without a placeholder the widget opens on a
    // real selection instead of showing an empty header.
    let ir = parse_html(
        r##"<root>
                <dropdown bind-value="pick">
                    <option value="a" label="Ay"/>
                    <option value="b" label="Bee"/>
                </dropdown>
            </root>"##,
    )
    .expect("html");
    let column = &ir.root.children[0];
    assert_eq!(
        column.attrs.signal_seed,
        Some(("__dropdown_open:pick".to_string(), "false".to_string())),
        "the panel still starts closed"
    );
    let header = &column.children[0];
    assert_eq!(
        header.attrs.signal_seed,
        Some(("pick".to_string(), "a".to_string())),
        "the first option seeds the value signal"
    );
}

#[test]
fn dropdown_with_placeholder_starts_unselected() {
    let ir = parse_html(
        r##"<root>
                <dropdown bind-value="pick" placeholder="Choose one">
                    <option value="a"/>
                </dropdown>
            </root>"##,
    )
    .expect("html");
    let header = &ir.root.children[0].children[0];
    assert_eq!(header.attrs.text.as_deref(), Some("Choose one"));
    assert_eq!(
        header.attrs.signal_seed, None,
        "a placeholder opts out of the first-option seed"
    );
}

#[test]
fn pickers_carry_a_shape_pattern() {
    // The generated pattern is the structural check, not the `-` / `:`
    // substring the placeholder used to promise.
    let ir = parse_html(
        r##"<root>
                <date-picker bind-value="due" id="due"/>
                <time-picker bind-value="at" id="at"/>
            </root>"##,
    )
    .expect("html");
    let date = &ir.root.children[0];
    assert_eq!(date.tag, "input");
    assert_eq!(date.attrs.pattern.as_deref(), Some("shape:date"));
    assert_eq!(date.attrs.placeholder.as_deref(), Some("YYYY-MM-DD"));
    let time = &ir.root.children[1];
    assert_eq!(time.attrs.pattern.as_deref(), Some("shape:time"));
    assert_eq!(time.attrs.placeholder.as_deref(), Some("HH:MM"));
}
