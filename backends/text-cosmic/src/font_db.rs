//! Fonts an app ships, added to a [`fontdb::Database`] next to the system's.
//!
//! The counterpart of `QFontDatabase::addApplicationFont` (Qt) and a
//! browser's `@font-face`: the faces in a font file become available under
//! the family the app names them by, whatever the file calls itself, so a
//! stylesheet's `font-family` finds them.

use cosmic_text::fontdb;
use lumen_text::{FontFace, FontStyle};

/// Add every face in `face.data` to `db` under `face.family`, with the
/// weight and style the rule declares where it declares them. Returns how
/// many faces were added; `0` when the bytes hold none this database can
/// read (TrueType, OpenType, and their collections).
pub fn register_face(db: &mut fontdb::Database, face: &FontFace) -> usize {
    let source = fontdb::Source::Binary(face.data.clone());
    let ids = db.load_font_source(source);
    let mut added = 0;
    for id in ids {
        let Some(mut info) = db.face(id).cloned() else {
            continue;
        };
        db.remove_face(id);
        info.families = vec![(face.family.clone(), fontdb::Language::English_UnitedStates)];
        if let Some(weight) = face.weight {
            info.weight = fontdb::Weight(weight);
        }
        if let Some(style) = face.style {
            info.style = match style {
                FontStyle::Normal => fontdb::Style::Normal,
                FontStyle::Italic => fontdb::Style::Italic,
                FontStyle::Oblique => fontdb::Style::Oblique,
            };
        }
        db.push_face_info(info);
        added += 1;
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn noto() -> Vec<u8> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../public/lumenc/tests/goldens/fonts/NotoSans-Regular.ttf"
        );
        std::fs::read(path).expect("read the test font")
    }

    fn face(data: Vec<u8>) -> FontFace {
        FontFace {
            family: "Pixel Demo".into(),
            data: Arc::new(data),
            weight: Some(700),
            style: Some(FontStyle::Italic),
        }
    }

    #[test]
    fn a_face_is_known_by_the_family_and_descriptors_the_rule_gives() {
        let mut db = fontdb::Database::new();
        assert_eq!(register_face(&mut db, &face(noto())), 1);
        let info = db.faces().next().expect("one face");
        assert_eq!(info.families[0].0, "Pixel Demo");
        assert_eq!(info.weight, fontdb::Weight(700));
        assert_eq!(info.style, fontdb::Style::Italic);
        let query = fontdb::Query {
            families: &[fontdb::Family::Name("Pixel Demo")],
            weight: fontdb::Weight(700),
            style: fontdb::Style::Italic,
            ..fontdb::Query::default()
        };
        assert!(db.query(&query).is_some());
    }

    #[test]
    fn bytes_that_hold_no_font_add_nothing() {
        let mut db = fontdb::Database::new();
        assert_eq!(register_face(&mut db, &face(b"not a font".to_vec())), 0);
        assert_eq!(db.len(), 0);
    }
}
