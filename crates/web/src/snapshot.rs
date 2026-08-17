//! The state an app is in, read out as the state a page is rendered with.
//!
//! A document is produced from signal state, wherever that state came from:
//! values an author declared, or values the app itself wrote while it ran.
//! This turns a running app's stores into the two forms a document needs, the
//! text the markup is rendered with and the typed seed the browser adopts it
//! with, so that a build and a server produce a page the same way.

use lumen_core::property_store::{PropertyKey, PropertyStore};
use lumen_core::signals::ArraySignals;
use lumen_html::contract::{Seed, SeedValue};

use crate::spec::SignalEnv;

/// One app's signal state, in both the forms a page needs.
///
/// [`Self::signals`] is what the markup is rendered against and
/// [`Self::seed`] is what the browser restores before its first reconcile.
/// They come out of one read so the two cannot disagree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// The state the document is rendered with.
    pub signals: SignalEnv,
    /// The same state, typed, for the runtime that adopts the document.
    pub seed: Seed,
    /// Globals whose value has no place in a document, by name. A caller
    /// reports these: the page renders without them.
    pub skipped: Vec<String>,
}

/// Read the signal state out of a running app's stores.
///
/// Globals are ordered by name and rows keep the order the app put them in,
/// so the same state always produces the same document.
///
/// A global's text is the text the app itself reads out of the store, which
/// is what keeps a rendered page and the app that adopts it showing the same
/// thing. Its seed value is the typed one, because the text of a color is
/// empty and a page that seeded that would hand the browser an empty color.
/// The two values a store can hold that a document cannot carry, a vector and
/// a live Rust value, are named in [`State::skipped`].
pub fn state_of(store: &PropertyStore, arrays: &ArraySignals) -> State {
    let mut names: Vec<&str> = store
        .iter()
        .filter_map(|(key, _)| match key {
            PropertyKey::Global(name) => Some(&**name),
            PropertyKey::Entity(..) => None,
        })
        .collect();
    names.sort_unstable();

    let mut state = State::default();
    for name in names {
        let Some(value) = store.get(&PropertyKey::global(name)) else {
            continue;
        };
        let text = store.get_global_str(name).unwrap_or_default();
        state.signals = state.signals.with_global(name, &*text);
        match SeedValue::try_from(value) {
            Ok(seeded) => {
                state.seed.globals.insert(name.to_string(), seeded);
            }
            Err(unsupported) => state.skipped.push(format!("`{name}`: {unsupported}")),
        }
    }

    let mut array_names: Vec<&str> = arrays.0.keys().map(String::as_str).collect();
    array_names.sort_unstable();
    for name in array_names {
        let Some(rows) = arrays.get(name) else {
            continue;
        };
        state.signals = state.signals.with_array(name, rows.to_vec());
        state.seed.arrays.insert(
            name.to_string(),
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(|(field, value)| (field.clone(), value.clone()))
                        .collect()
                })
                .collect(),
        );
    }
    state
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lumen_core::components::Color;
    use lumen_core::property_store::PropertyValue;
    use lumen_core::signals::ArrayItem;

    use super::*;

    fn store_with(values: &[(&str, PropertyValue)]) -> PropertyStore {
        let mut store = PropertyStore::default();
        for (name, value) in values {
            store.set(PropertyKey::global(*name), value.clone());
        }
        store
    }

    #[test]
    fn a_global_reaches_both_the_markup_and_the_seed() {
        let store = store_with(&[
            ("count", PropertyValue::I64(3)),
            ("open", PropertyValue::Bool(true)),
        ]);
        let state = state_of(&store, &ArraySignals::default());
        assert_eq!(state.signals.global("count"), Some("3"));
        assert!(state.signals.is_truthy("open"));
        assert_eq!(state.seed.globals["count"], SeedValue::I64(3));
        assert_eq!(state.seed.globals["open"], SeedValue::Bool(true));
        assert!(state.skipped.is_empty());
    }

    #[test]
    fn a_color_keeps_its_type_in_the_seed() {
        let store = store_with(&[(
            "accent",
            PropertyValue::Color(Color::rgba(1.0, 0.0, 0.0, 1.0)),
        )]);
        let state = state_of(&store, &ArraySignals::default());
        assert_eq!(
            state.seed.globals["accent"],
            SeedValue::Color([1.0, 0.0, 0.0, 1.0])
        );
        assert!(state.skipped.is_empty());
    }

    #[test]
    fn a_value_a_document_cannot_carry_is_named() {
        let store = store_with(&[("handle", PropertyValue::Custom(Arc::new(7u32)))]);
        let state = state_of(&store, &ArraySignals::default());
        assert!(state.seed.globals.is_empty());
        assert_eq!(state.skipped.len(), 1);
        assert!(state.skipped[0].starts_with("`handle`: "));
    }

    #[test]
    fn rows_reach_both_forms_in_order() {
        let mut arrays = ArraySignals::default();
        let row = |id: &str| ArrayItem::from([("id".to_string(), id.to_string())]);
        arrays.set("todos", vec![row("1"), row("2")]);
        let state = state_of(&PropertyStore::default(), &arrays);
        assert_eq!(state.signals.rows("todos").map(<[_]>::len), Some(2));
        assert_eq!(state.seed.arrays["todos"][0]["id"], "1");
        assert_eq!(state.seed.arrays["todos"][1]["id"], "2");
    }

    #[test]
    fn the_same_state_reads_the_same_way_twice() {
        let store = store_with(&[
            ("b", PropertyValue::Str(Arc::from("two"))),
            ("a", PropertyValue::F64(1.5)),
        ]);
        let mut arrays = ArraySignals::default();
        arrays.set("rows", vec![ArrayItem::new()]);
        assert_eq!(state_of(&store, &arrays), state_of(&store, &arrays));
    }
}
