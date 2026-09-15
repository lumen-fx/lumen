//! Rebuilding what the tree says when the app changes locale.
//!
//! An app's locale is not fixed for the life of the process: a language
//! menu changes it, and everything the old locale wrote has to be written
//! again. The spawner left the raw material behind for exactly this, as
//! [`AuthoredStrings`] on every element whose text came from a catalogue or
//! went through a `format`, so a switch resolves the same strings against
//! the catalogue in force now rather than re-reading the markup. A shipped
//! app has no markup to re-read.
//!
//! What follows a switch: a marked element's text, a text entry's
//! placeholder, a tooltip's body, and the output of a `format` spec. What
//! does not: a string a script composed and set with `set_text` on an
//! element that names no key, and anything built once at startup outside
//! the tree, such as a native menu or a tray label.

use bevy_ecs::prelude::*;
use lumen_core::components::{AuthoredStrings, BindText, TextContent, TextFormat, TextInput};
use lumen_core::i18n::AppI18n;
use lumen_core::input::Focused;
use lumen_core::prelude::{App, TickStage};
use lumen_primitives::TooltipSource;

use crate::script_commands::apply_scene_script_commands;
use crate::spawn::resolve_strings;

/// Register the rebuild, after the applier that performs the switch, so a
/// locale a script picks lands on the tick it was picked on.
///
/// The order is not the host's to choose: running the rebuild before the
/// applier leaves the tree a tick behind the locale, which shows up as a
/// language menu whose entries take two clicks.
pub fn install_retranslate(app: &mut App) {
    app.add_systems(
        TickStage::Systems,
        retranslate_on_locale_change.after(apply_scene_script_commands),
    );
}

/// Resolve every element that kept its authored strings against the locale
/// in force, and write back what changed.
///
/// Gated on [`AppI18n`] being marked changed, which is what the applier
/// does when it switches the locale through a `ResMut` borrow. A steady
/// tick costs one flag read.
///
/// Two kinds of element are left alone. One carrying a `bind-text` belongs
/// to its signal, and [`lumen_core::signals::apply_text_bindings`] refreshes
/// it on the same tick. A focused text entry has an edit in flight, and
/// overwriting the buffer under the caret would lose the keystroke; its
/// placeholder still moves, since nothing is typing into that.
#[allow(clippy::type_complexity)]
pub fn retranslate_on_locale_change(
    i18n: Option<Res<AppI18n>>,
    mut q: Query<
        (
            &AuthoredStrings,
            Option<&TextFormat>,
            Option<&mut TextContent>,
            Option<&mut TextInput>,
            Option<&mut TooltipSource>,
            Option<&Focused>,
        ),
        Without<BindText>,
    >,
) {
    let Some(i18n) = i18n else {
        return;
    };
    if !i18n.is_changed() {
        return;
    }
    for (authored, format, text, mut input, tooltip, focused) in &mut q {
        let strings = resolve_strings(Some(&*i18n), authored, format.map(|f| f.0.as_str()));
        // A placeholder shows while the field is empty, so it moves even
        // mid-edit; the buffer under the caret does not.
        if let Some(input) = input.as_mut() {
            let want = strings.placeholder.unwrap_or_default();
            if input.placeholder != want {
                input.placeholder = want;
            }
        }
        if let Some(mut tooltip) = tooltip
            && let Some(body) = &strings.tooltip
            && tooltip.text != *body
        {
            tooltip.text = body.clone();
        }
        // The edit in flight the gate protects: a focused text buffer, whose
        // next keystroke would land on the text this write replaces.
        if focused.is_some() && input.is_some() {
            continue;
        }
        // Only when the rule resolved a text at all: an `<input
        // translatable="search" placeholder="Search"/>` has none, and its
        // typed value is not the catalogue's to replace.
        let (Some(mut content), Some(want)) = (text, strings.text) else {
            continue;
        };
        if content.0 == want {
            continue;
        }
        content.0 = want;
        // Byte offsets into the old text can point past the end of the new
        // one; the next keystroke would insert out of bounds.
        if let Some(input) = input.as_mut() {
            if input.cursor > content.0.len() {
                input.cursor = content.0.len();
            }
            if let Some(anchor) = input.selection_anchor
                && anchor > content.0.len()
            {
                input.selection_anchor = None;
            }
        }
    }
}
