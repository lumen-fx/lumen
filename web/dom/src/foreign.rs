//! Elements an add-on answers for.
//!
//! An add-on can declare a markup tag of its own. The page writes it as the
//! HTML element the add-on named, with the markup's own content inside, and
//! from there the element belongs to the add-on: the backend binds it to its
//! entity like any other, so a script reaches it and its classes, attributes
//! and style are projected onto it, but the walk does not descend into it and
//! no text is written into it. What is inside is the add-on's to build.
//!
//! The add-on hears about the element through three hooks, each optional:
//! `mount(element)` once it is bound, `update(element, name, value)` when an
//! attribute or its text changes, and `unmount(element)` before it leaves the
//! page. A hook that throws is reported to the console and the app carries
//! on.

use std::collections::HashMap;

use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::Element;

/// The hooks an add-on gave for one of its elements.
#[derive(Default)]
pub struct ForeignHooks {
    mount: Option<Function>,
    update: Option<Function>,
    unmount: Option<Function>,
}

impl ForeignHooks {
    /// Read the hooks off `object`, an add-on's entry for one element: its
    /// `mount`, `update` and `unmount` functions. A member that is missing or
    /// is not a function is a hook the add-on does not want.
    pub fn from_object(object: &JsValue) -> Self {
        let hook = |name: &str| -> Option<Function> {
            Reflect::get(object, &JsValue::from_str(name))
                .ok()
                .and_then(|value| value.dyn_into::<Function>().ok())
        };
        Self {
            mount: hook("mount"),
            update: hook("update"),
            unmount: hook("unmount"),
        }
    }
}

/// One element an add-on answers for.
pub struct ForeignTag {
    /// The HTML element it is written as.
    pub html: String,
    /// True when that element takes no children and no end tag.
    pub void: bool,
    /// What the add-on does with it.
    pub hooks: ForeignHooks,
}

/// Every element the page's add-ons answer for, by markup tag.
///
/// Not a [`bevy_ecs::prelude::Resource`]: the hooks are JavaScript functions,
/// which are neither `Send` nor `Sync`, so this lives in the world as a
/// non-send resource beside the node table.
#[derive(Default)]
pub struct ForeignElements {
    tags: HashMap<String, ForeignTag>,
}

impl ForeignElements {
    /// A set that answers for no tag.
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer for `tag` with `element`.
    pub fn insert(&mut self, tag: impl Into<String>, element: ForeignTag) {
        self.tags.insert(tag.into(), element);
    }

    /// The element `tag` is written as, when an add-on answers for it.
    pub fn get(&self, tag: &str) -> Option<&ForeignTag> {
        self.tags.get(tag)
    }

    /// True when an add-on answers for `tag`.
    pub fn contains(&self, tag: &str) -> bool {
        self.tags.contains_key(tag)
    }

    /// Tell the add-on answering for `tag` that `element` is in the page.
    pub(crate) fn mount(&self, tag: &str, element: &Element) {
        self.call(
            tag,
            "mount",
            |hooks| hooks.mount.as_ref(),
            &[element.into()],
        );
    }

    /// Tell the add-on answering for `tag` that `name` changed to `value`.
    pub(crate) fn update(&self, tag: &str, element: &Element, name: &str, value: &str) {
        self.call(
            tag,
            "update",
            |hooks| hooks.update.as_ref(),
            &[element.into(), name.into(), value.into()],
        );
    }

    /// Tell the add-on answering for `tag` that `element` is leaving the page.
    pub(crate) fn unmount(&self, tag: &str, element: &Element) {
        self.call(
            tag,
            "unmount",
            |hooks| hooks.unmount.as_ref(),
            &[element.into()],
        );
    }

    fn call(
        &self,
        tag: &str,
        name: &str,
        hook: impl Fn(&ForeignHooks) -> Option<&Function>,
        args: &[JsValue],
    ) {
        let Some(function) = self.tags.get(tag).and_then(|t| hook(&t.hooks)) else {
            return;
        };
        let args: js_sys::Array = args.iter().collect();
        if let Err(error) = function.apply(&JsValue::NULL, &args) {
            web_sys::console::error_2(
                &JsValue::from_str(&format!("lumen: the <{tag}> {name} hook threw")),
                &error,
            );
        }
    }
}
