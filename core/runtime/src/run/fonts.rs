//! The fonts an app ships, from its stylesheet's `@font-face` rules.
//!
//! Each rule names a family and the files that hold it. [`load_app_fonts`]
//! reads the files through the asset layer (an archive, a registered source,
//! then the disk, the same chain an image loads through), resolves their
//! paths against the app directory the way a `bg: url(...)` is, and hands the
//! first file of each rule that holds a font to the layout world's shaper.
//! [`extract_app_fonts`] then hands every loaded face to the render world's
//! shaper, so the glyphs a frame paints come from the font layout measured.
//!
//! The loader runs when the stylesheet is first installed and again whenever
//! it is replaced, so a rule a hot reload adds loads on the same tick, before
//! that tick lays text out.

use bevy_ecs::prelude::*;
use lumen_assets::AssetServer;
use lumen_core::components::TextStyle;
use lumen_ir::css::{AtRule, Stylesheet};
use lumen_text::{FontFace, FontStyle, ShaperService};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use super::restyle::RuntimeStylesheet;

/// One `@font-face` rule, as the desktop reads it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FontFaceRule {
    /// `font-family`.
    pub family: String,
    /// The `url()`s of `src` the desktop can read, in order. `local()`
    /// entries and WOFF files are the browser's and are left out.
    pub urls: Vec<String>,
    /// `font-weight`, when it is one weight.
    pub weight: Option<u16>,
    /// `font-style`.
    pub style: Option<FontStyle>,
}

/// Every face the app's rules loaded, in load order, and which rules have
/// been tried, so a replaced stylesheet loads only what it adds.
#[derive(Resource, Default)]
pub(crate) struct AppFonts {
    loaded: Vec<FontFace>,
    tried: HashSet<FontFaceRule>,
}

/// How many of [`AppFonts`]'s faces the render world's shaper holds.
#[derive(Resource, Default)]
struct RenderFonts(usize);

/// The `@font-face` rules of `sheet` that name a family and a file.
pub(crate) fn font_face_rules(sheet: &Stylesheet) -> Vec<FontFaceRule> {
    sheet
        .at_rules
        .iter()
        .filter(|rule| rule.name == "font-face")
        .filter_map(font_face_rule)
        .collect()
}

fn font_face_rule(rule: &AtRule) -> Option<FontFaceRule> {
    let mut face = FontFaceRule {
        family: String::new(),
        urls: Vec::new(),
        weight: None,
        style: None,
    };
    for declaration in split_outside(&rule.body, ';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "font-family" => face.family = unquote(value).to_string(),
            "src" => {
                face.urls = split_outside(value, ',')
                    .into_iter()
                    .filter_map(src_url)
                    .collect();
            }
            "font-weight" => {
                face.weight = match value.to_ascii_lowercase().as_str() {
                    "normal" => Some(400),
                    "bold" => Some(700),
                    // A range ("100 900") is a variable font's: the file's
                    // own axis says what it covers.
                    v => v.parse::<u16>().ok().filter(|w| (1..=1000).contains(w)),
                };
            }
            "font-style" => {
                let keyword = value.split_whitespace().next().unwrap_or_default();
                face.style = match keyword.to_ascii_lowercase().as_str() {
                    "normal" => Some(FontStyle::Normal),
                    "italic" => Some(FontStyle::Italic),
                    "oblique" => Some(FontStyle::Oblique),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    (!face.family.is_empty() && !face.urls.is_empty()).then_some(face)
}

/// The path of one `src` entry's `url()`, or `None` for an entry the
/// desktop does not read: a `local()` font, or a WOFF file, named by its
/// `format()` hint or its extension. A rule that lists only those is the
/// browser's, so it loads nothing here and says nothing about it.
fn src_url(entry: &str) -> Option<String> {
    let entry = entry.trim();
    let open = entry.get(..4)?.eq_ignore_ascii_case("url(").then_some(4)?;
    let mut quote = None;
    for (i, c) in entry[open..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, ')') => {
                let url = unquote(entry[open..open + i].trim());
                let hint = entry[open + i + 1..].to_ascii_lowercase();
                let path = url.to_ascii_lowercase();
                let woff =
                    hint.contains("woff") || path.ends_with(".woff") || path.ends_with(".woff2");
                return (!url.is_empty() && !woff).then(|| url.to_string());
            }
            _ => {}
        }
    }
    None
}

fn unquote(value: &str) -> &str {
    ['"', '\'']
        .iter()
        .find_map(|q| value.strip_prefix(*q).and_then(|v| v.strip_suffix(*q)))
        .unwrap_or(value)
}

/// Split `text` at each `sep` outside quotes and parentheses.
fn split_outside(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut start) = (0usize, None::<char>, 0usize);
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.saturating_sub(1),
            (None, c) if c == sep && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts.retain(|p| !p.trim().is_empty());
    parts
}

/// Install the loader and the hand-off to the render world.
///
/// The loader runs in `LayoutSync`, ahead of the solve: after every system
/// that can replace the stylesheet this tick (a hot reload among them), and
/// before anything measures text against it. It runs only on a tick that
/// installed or replaced the stylesheet.
pub(crate) fn register(app: &mut lumen_core::app::App) {
    app.world.init_resource::<AppFonts>();
    app.add_systems(
        lumen_core::tick::TickStage::LayoutSync,
        load_app_fonts
            .run_if(resource_exists_and_changed::<RuntimeStylesheet>)
            .before(lumen_core::layout_backend::LayoutSolve),
    );
    app.add_extract_fn(extract_app_fonts);
}

/// Load the faces of every `@font-face` rule not tried yet into the layout
/// world's shaper, whenever the stylesheet is installed or replaced.
///
/// Each rule's files are tried in the order `src` lists them, as a browser
/// does, and the first one that holds a font this shaper reads wins; one
/// that is missing falls through to the next. A rule none of
/// whose files loads is reported once and text naming its family keeps the
/// fallback font. Every text element is marked changed after a load, so text
/// measured before its font arrived is measured again.
pub(crate) fn load_app_fonts(
    sheet: Option<Res<RuntimeStylesheet>>,
    server: Option<Res<AssetServer>>,
    mut fonts: ResMut<AppFonts>,
    shaper: Option<NonSendMut<ShaperService>>,
    mut text: Query<&mut TextStyle>,
) {
    let (Some(sheet), Some(mut shaper)) = (sheet, shaper) else {
        return;
    };
    let reader = server.map(|s| s.source_reader());
    let mut loaded_any = false;
    for rule in font_face_rules(&sheet.0) {
        if !fonts.tried.insert(rule.clone()) {
            continue;
        }
        let mut problems = Vec::new();
        let face = rule.urls.iter().find_map(|url| {
            let path: PathBuf = lumen_assets::resolve_source_path(url);
            let bytes = match &reader {
                Some(reader) => reader.read(&path),
                None => std::fs::read(&path),
            };
            let face = FontFace {
                family: rule.family.clone(),
                data: Arc::new(match bytes {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        problems.push(format!("{url}: {e}"));
                        return None;
                    }
                }),
                weight: rule.weight,
                style: rule.style,
            };
            if shaper.register_font(&face) {
                Some(face)
            } else {
                problems.push(format!(
                    "{url}: not a font this build reads (TrueType or OpenType)"
                ));
                None
            }
        });
        match face {
            Some(face) => {
                fonts.loaded.push(face);
                loaded_any = true;
            }
            None => lumen_core::warn_line!(
                "lumen: @font-face \"{}\" loaded no font, so its text uses the fallback font ({})",
                rule.family,
                problems.join("; ")
            ),
        }
    }
    if loaded_any {
        for mut style in &mut text {
            style.set_changed();
        }
    }
}

/// Hand the render world's shaper the faces it does not hold yet.
pub(crate) fn extract_app_fonts(main: &mut World, render: &mut World) {
    let Some(fonts) = main.get_resource::<AppFonts>() else {
        return;
    };
    let done = render.get_resource::<RenderFonts>().map_or(0, |r| r.0);
    if done >= fonts.loaded.len() {
        return;
    }
    let pending = fonts.loaded[done..].to_vec();
    let total = fonts.loaded.len();
    let Some(mut shaper) = render.get_non_send_mut::<ShaperService>() else {
        return;
    };
    for face in &pending {
        shaper.register_font(face);
    }
    render.insert_resource(RenderFonts(total));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(body: &str) -> Option<FontFaceRule> {
        font_face_rule(&AtRule {
            name: "font-face".into(),
            prelude: String::new(),
            body: body.into(),
            media: None,
        })
    }

    #[test]
    fn a_rule_reads_its_family_files_and_descriptors() {
        let face = rule(
            r#" font-family: "Pixel Demo"; src: local("Pixel Demo"),
                url("fonts/demo.woff2") format("woff2"), url(fonts/demo.ttf);
                font-weight: bold; font-style: italic; font-display: swap; "#,
        )
        .expect("a usable rule");
        assert_eq!(face.family, "Pixel Demo");
        assert_eq!(face.urls, ["fonts/demo.ttf"], "the woff2 is the browser's");
        assert_eq!(face.weight, Some(700));
        assert_eq!(face.style, Some(FontStyle::Italic));
    }

    #[test]
    fn a_weight_range_keeps_the_file_weight() {
        let face = rule("font-family: V; src: url(v.ttf); font-weight: 100 900;").unwrap();
        assert_eq!(face.weight, None);
    }

    #[test]
    fn a_rule_without_a_family_or_a_file_is_skipped() {
        assert!(rule("src: url(a.ttf);").is_none());
        assert!(rule("font-family: A; src: local(A);").is_none());
    }

    #[test]
    fn a_rule_for_browsers_only_is_left_to_them() {
        assert!(rule(r#"font-family: W; src: url("w.woff2") format("woff2");"#).is_none());
        assert!(rule("font-family: W; src: url(w.woff), url(w.bin) format('woff');").is_none());
    }
}
