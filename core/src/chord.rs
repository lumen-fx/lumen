//! Keyboard shortcuts: one key with the modifiers held for it.
//!
//! A [`Chord`] is read from the text an author writes (`Ctrl+Shift+P`,
//! `CommandOrControl+S`, `Alt+F4`, `F1`), matched against a key press, and
//! shown the way the platform's menus write one. Everything in Lumen that
//! binds a key reads it through this type: the in-window menu bar's `accel`,
//! a module's key table, a command registry.
//!
//! [`ReservedChords`] is where the parts of an app that answer chords
//! app-wide list them, so an element that forwards keys elsewhere (an
//! embedded renderer's viewport) can leave those to the app.

use std::fmt;

use bevy_ecs::prelude::Resource;

use crate::input::{Key, Modifiers, NamedKey};
use crate::keycode::KeyCode;

/// The modifier symbols macOS draws, in the order it draws them: Control,
/// Option, Shift, Command.
const MAC_SYMBOLS: [&str; 4] = ["\u{2303}", "\u{2325}", "\u{21e7}", "\u{2318}"];

/// One keyboard shortcut: a key and the modifiers held with it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    /// The key.
    pub key: Key,
    /// The modifiers that must be held, and no others.
    pub modifiers: Modifiers,
}

impl Chord {
    /// Read a chord as an author writes one, for the platform this build
    /// runs on. See [`Chord::parse_for`].
    pub fn parse(text: &str) -> Option<Self> {
        Self::parse_for(text, cfg!(target_os = "macos"))
    }

    /// Read a chord: modifiers and a key joined by `+` (`Ctrl+Shift+P`).
    ///
    /// Modifiers, in any case: `Ctrl`/`Control`, `Alt`/`Option`, `Shift`,
    /// `Super`/`Cmd`/`Command`/`Meta`, and `CommandOrControl` (also
    /// `CmdOrCtrl`), which is Command when `mac` and Control otherwise. The
    /// key is a single character, or a name: `Enter`, `Escape`, `Tab`,
    /// `Space`, `Backspace`, `Delete`, `ArrowUp` (or `Up`) and the other
    /// arrows, `Home`, `End`, `PageUp`, `F1`..., `Plus`, `Comma`. `Ctrl++`
    /// is Control and the plus key. `None` for an empty chord, a missing
    /// key, or a modifier this does not know.
    pub fn parse_for(text: &str, mac: bool) -> Option<Self> {
        let mut rest = text;
        let mut held = [false; 4];
        // A modifier name and its `+`, until the key; a `+` first is the
        // plus key.
        while let Some(at) = rest.find('+').filter(|&at| at > 0) {
            let name = rest[..at].trim();
            let slot = group_of(MODIFIER_NAMES, name)?;
            // `CommandOrControl` is Command on macOS, Control elsewhere.
            held[if slot == 4 {
                usize::from(mac) * 3
            } else {
                slot
            }] = true;
            rest = &rest[at + 1..];
        }
        Some(Self {
            key: key_named(rest.trim())?,
            modifiers: Modifiers {
                ctrl: held[0],
                alt: held[1],
                shift: held[2],
                super_: held[3],
            },
        })
    }

    /// Read a list of chords separated by commas (`Ctrl+Y, Ctrl+Shift+Z`).
    /// A comma right after a `+` is the comma key (`Ctrl+,`).
    ///
    /// # Errors
    ///
    /// The first entry that does not read as a chord.
    pub fn parse_list(text: &str) -> Result<Vec<Self>, String> {
        let mut chords = Vec::new();
        for part in split_list(text) {
            match Self::parse(&part) {
                Some(chord) => chords.push(chord),
                None => return Err(part),
            }
        }
        Ok(chords)
    }

    /// Whether a key press is this chord: the same modifiers, and the same
    /// key. Letters compare without case, since Shift is a modifier of the
    /// chord rather than part of its key. On a layout whose letters are not
    /// Latin, a chord on a Latin letter or digit matches the key that sits
    /// where the US layout has it.
    pub fn matches(&self, key: &Key, code: KeyCode, modifiers: &Modifiers) -> bool {
        if *modifiers != self.modifiers {
            return false;
        }
        match (&self.key, key) {
            (Key::Named(want), Key::Named(got)) => want == got,
            (Key::Named(NamedKey::Space), Key::Character(got)) => got == " ",
            (Key::Character(want), Key::Character(got)) => {
                want.eq_ignore_ascii_case(got)
                    || (want.len() == 1
                        && want.bytes().all(|b| b.is_ascii_alphanumeric())
                        && !got.is_ascii()
                        && code != KeyCode::Unidentified
                        && self.key.us_code() == code)
            }
            _ => false,
        }
    }

    /// The chord as the platform's menus write it: on macOS the modifier
    /// symbols in the system's order followed by the key
    /// (`\u{21e7}\u{2318}S`); elsewhere modifiers and key joined by `+` in
    /// the order Ctrl, Alt, Shift, Super (`Ctrl+Shift+S`). A single-letter
    /// key is upper-cased.
    pub fn label(&self, mac: bool) -> String {
        self.write(mac, "Super+")
    }

    /// The chord as text that reads back as the same chord on this
    /// platform, in the `+` form whatever the platform: `Ctrl+Shift+P`,
    /// with the logo key written `Cmd` on macOS and `Super` elsewhere.
    pub fn to_text(&self) -> String {
        self.write(
            false,
            if cfg!(target_os = "macos") {
                "Cmd+"
            } else {
                "Super+"
            },
        )
    }

    /// The modifiers in the order Ctrl, Alt, Shift, logo key, as macOS's
    /// symbols when `mac` and as names each followed by `+` otherwise, then
    /// the key.
    fn write(&self, mac: bool, logo: &str) -> String {
        let m = &self.modifiers;
        let names = if mac {
            MAC_SYMBOLS
        } else {
            ["Ctrl+", "Alt+", "Shift+", logo]
        };
        let mut out = String::new();
        for (on, name) in [m.ctrl, m.alt, m.shift, m.super_].into_iter().zip(names) {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&key_label(&self.key));
        out
    }
}

impl fmt::Display for Chord {
    /// The platform's spelling; see [`Chord::label`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label(cfg!(target_os = "macos")))
    }
}

impl std::str::FromStr for Chord {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| format!("'{s}' is not a key chord"))
    }
}

/// The entries of a comma-separated chord list.
fn split_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if c == ',' && !current.trim_end().ends_with('+') && !current.trim().is_empty() {
            out.push(std::mem::take(&mut current).trim().to_string());
        } else {
            current.push(c);
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

/// The modifier names a chord is written with, in any case: Ctrl, Alt,
/// Shift, Super, then `CommandOrControl`, each group's spellings separated
/// by spaces. One string rather than a table keeps it small where every byte
/// is paid for (a page downloads it).
const MODIFIER_NAMES: &str = "ctrl control|alt option|shift|super cmd command meta|commandorcontrol cmdorctrl cmdorcontrol commandorctrl";

/// The named keys' spellings, in [`NAMED`] order, the first of each group
/// being how a chord shows it.
const NAMED_KEYS: &str = "Tab|Enter return|Escape esc|Backspace|Space|Up arrowup|Down arrowdown|Left arrowleft|Right arrowright|Home|End|Delete del";

/// [`NamedKey`] by its group in [`NAMED_KEYS`].
const NAMED: [NamedKey; 12] = [
    NamedKey::Tab,
    NamedKey::Enter,
    NamedKey::Escape,
    NamedKey::Backspace,
    NamedKey::Space,
    NamedKey::ArrowUp,
    NamedKey::ArrowDown,
    NamedKey::ArrowLeft,
    NamedKey::ArrowRight,
    NamedKey::Home,
    NamedKey::End,
    NamedKey::Delete,
];

/// Keys written by name that are characters, and the characters they are.
const CHARACTER_KEYS: &str = "plus|comma|pageup|pagedown|insert";
const CHARACTERS: [&str; 5] = ["+", ",", "PageUp", "PageDown", "Insert"];

/// The group of `names` (groups separated by `|`) that spells `word`.
fn group_of(names: &str, word: &str) -> Option<usize> {
    names
        .split('|')
        .position(|group| group.split(' ').any(|n| n.eq_ignore_ascii_case(word)))
}

/// The key a chord names, `None` for none.
fn key_named(name: &str) -> Option<Key> {
    if name.is_empty() {
        return None;
    }
    if let Some(i) = group_of(NAMED_KEYS, name) {
        return Some(Key::Named(NAMED[i]));
    }
    if let Some(i) = group_of(CHARACTER_KEYS, name) {
        return Some(Key::Character(CHARACTERS[i].to_string()));
    }
    // A letter is kept lower-case, so `Ctrl+S` and `Ctrl+s` are one chord;
    // matching ignores case anyway.
    let mut key = name.to_string();
    if name.len() == 1 {
        key.make_ascii_lowercase();
    }
    Some(Key::Character(key))
}

/// How a chord shows its key: a Latin letter upper-cased, `+` as `Plus`.
fn key_label(key: &Key) -> String {
    match key {
        Key::Named(named) => {
            let group = NAMED_KEYS
                .split('|')
                .nth(*named as usize)
                .unwrap_or_default();
            group.split(' ').next().unwrap_or_default().to_string()
        }
        Key::Character(c) if c == "+" => "Plus".to_string(),
        Key::Character(c) if c.len() == 1 => c.to_ascii_uppercase(),
        Key::Character(c) => c.clone(),
    }
}

/// The chords parts of the app answer app-wide, each list under the name of
/// what answers it (`menubar`, a module's name). An element that hands keys
/// to something else, such as an embedded renderer's viewport, reads this
/// to leave those chords to the app.
#[derive(Resource, Clone, Debug, Default)]
pub struct ReservedChords {
    owners: Vec<(String, Vec<Chord>)>,
}

impl ReservedChords {
    /// Replace the chords `owner` answers.
    pub fn set(&mut self, owner: &str, chords: Vec<Chord>) {
        match self.owners.iter_mut().find(|(o, _)| o == owner) {
            Some((_, list)) => *list = chords,
            None => self.owners.push((owner.to_string(), chords)),
        }
    }

    /// Drop every chord `owner` answers.
    pub fn remove(&mut self, owner: &str) {
        self.owners.retain(|(o, _)| o != owner);
    }

    /// The chords `owner` answers.
    pub fn of(&self, owner: &str) -> &[Chord] {
        self.owners
            .iter()
            .find(|(o, _)| o == owner)
            .map_or(&[], |(_, list)| list.as_slice())
    }

    /// Whether any owner answers this key press.
    pub fn matches(&self, key: &Key, code: KeyCode, modifiers: &Modifiers) -> bool {
        self.owners
            .iter()
            .flat_map(|(_, list)| list)
            .any(|c| c.matches(key, code, modifiers))
    }

    /// Whether an owner other than `owner` answers this key press.
    pub fn taken_by_other(
        &self,
        owner: &str,
        key: &Key,
        code: KeyCode,
        modifiers: &Modifiers,
    ) -> bool {
        self.owners
            .iter()
            .filter(|(o, _)| o != owner)
            .flat_map(|(_, list)| list)
            .any(|c| c.matches(key, code, modifiers))
    }

    /// Whether no owner answers any chord.
    pub fn is_empty(&self) -> bool {
        self.owners.iter().all(|(_, list)| list.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, alt: bool, shift: bool, super_: bool) -> Modifiers {
        Modifiers {
            shift,
            ctrl,
            alt,
            super_,
        }
    }

    #[test]
    fn chords_read_as_authors_write_them() {
        let c = Chord::parse_for("Ctrl+Shift+P", false).unwrap();
        assert_eq!(c.modifiers, mods(true, false, true, false));
        assert_eq!(c.key, Key::Character("p".into()));
        let c = Chord::parse_for("CommandOrControl+S", false).unwrap();
        assert_eq!(c.modifiers, mods(true, false, false, false));
        let c = Chord::parse_for("CmdOrCtrl+S", true).unwrap();
        assert_eq!(c.modifiers, mods(false, false, false, true));
        for logo in ["Cmd+K", "Super+K", "Meta+K", "command+k"] {
            assert!(
                Chord::parse_for(logo, false).unwrap().modifiers.super_,
                "{logo}"
            );
        }
        let c = Chord::parse_for("Alt+F4", false).unwrap();
        assert_eq!(c.key, Key::Character("F4".into()));
        assert!(c.modifiers.alt);
        assert!(Chord::parse_for("f6", false).unwrap().matches(
            &Key::Character("F6".into()),
            KeyCode::F6,
            &Modifiers::default()
        ));
        assert_eq!(
            Chord::parse_for("Option+Esc", false).unwrap().key,
            Key::Named(NamedKey::Escape)
        );
        assert_eq!(
            Chord::parse_for("ctrl+pagedown", false).unwrap().key,
            Key::Character("PageDown".into())
        );
        assert_eq!(
            Chord::parse_for("Ctrl++", false).unwrap().key,
            Key::Character("+".into())
        );
        assert_eq!(
            Chord::parse_for("Ctrl+Plus", false).unwrap().key,
            Key::Character("+".into())
        );
        assert_eq!(
            Chord::parse_for("Shift+Up", false).unwrap().key,
            Key::Named(NamedKey::ArrowUp)
        );
        assert!(Chord::parse_for("", false).is_none());
        assert!(Chord::parse_for("Ctrl+", false).is_none());
        assert!(Chord::parse_for("Hyper+X", false).is_none());
    }

    #[test]
    fn mac_labels_use_the_symbols() {
        let c = Chord::parse_for("Shift+Cmd+S", true).unwrap();
        assert_eq!(c.modifiers, mods(false, false, true, true));
        assert_eq!(c.key, Key::Character("s".into()));
        assert_eq!(c.label(true), "\u{21e7}\u{2318}S");
    }

    #[test]
    fn lists_split_on_commas_but_keep_the_comma_key() {
        let list = Chord::parse_list("Ctrl+Y, Ctrl+Shift+Z").unwrap();
        assert_eq!(list.len(), 2);
        let list = Chord::parse_list("Ctrl+,, F1").unwrap();
        assert_eq!(list[0].key, Key::Character(",".into()));
        assert_eq!(list[1].key, Key::Character("F1".into()));
        assert_eq!(Chord::parse_list("").unwrap(), Vec::new());
        assert_eq!(
            Chord::parse_list("Ctrl+P, Hyper+Q"),
            Err("Hyper+Q".to_string())
        );
    }

    #[test]
    fn a_chord_matches_its_press_and_no_other() {
        let c = Chord::parse_for("Ctrl+Shift+P", false).unwrap();
        let ctrl_shift = mods(true, false, true, false);
        assert!(c.matches(&Key::Character("P".into()), KeyCode::KeyP, &ctrl_shift));
        assert!(c.matches(&Key::Character("p".into()), KeyCode::KeyP, &ctrl_shift));
        assert!(!c.matches(
            &Key::Character("p".into()),
            KeyCode::KeyP,
            &mods(true, false, false, false)
        ));
        assert!(!c.matches(&Key::Character("o".into()), KeyCode::KeyO, &ctrl_shift));
        let f6 = Chord::parse_for("F6", false).unwrap();
        assert!(f6.matches(
            &Key::Character("F6".into()),
            KeyCode::F6,
            &Modifiers::default()
        ));
        let space = Chord::parse_for("Shift+Space", false).unwrap();
        assert!(space.matches(
            &Key::Character(" ".into()),
            KeyCode::Space,
            &mods(false, false, true, false)
        ));
    }

    #[test]
    fn a_latin_chord_matches_where_the_key_sits_on_another_layout() {
        let c = Chord::parse_for("Ctrl+S", false).unwrap();
        let ctrl = mods(true, false, false, false);
        // Cyrillic: the key that types `s` on a US layout types U+044B.
        assert!(c.matches(&Key::Character("\u{44b}".into()), KeyCode::KeyS, &ctrl));
        assert!(!c.matches(&Key::Character("\u{44b}".into()), KeyCode::KeyD, &ctrl));
        // A Latin key elsewhere is that key, not its place.
        assert!(!c.matches(&Key::Character("z".into()), KeyCode::KeyS, &ctrl));
    }

    #[test]
    fn chords_show_the_platform_s_way() {
        let show = |text: &str, mac: bool| Chord::parse_for(text, mac).unwrap().label(mac);
        assert_eq!(show("CommandOrControl+z", false), "Ctrl+Z");
        assert_eq!(show("Shift+CmdOrCtrl+S", false), "Ctrl+Shift+S");
        assert_eq!(show("Alt+F4", false), "Alt+F4");
        assert_eq!(show("Super+Option+Space", false), "Alt+Super+Space");
        assert_eq!(show("CommandOrControl+Z", true), "\u{2318}Z");
        assert_eq!(show("Shift+CommandOrControl+S", true), "\u{21e7}\u{2318}S");
        assert_eq!(show("Control+Alt+Delete", true), "\u{2303}\u{2325}Delete");
        assert_eq!(show("F1", true), "F1");
        assert_eq!(show("Ctrl++", false), "Ctrl+Plus");
    }

    #[test]
    fn text_reads_back_as_the_same_chord() {
        for text in [
            "Ctrl+Shift+P",
            "Alt+F4",
            "Ctrl++",
            "Super+Up",
            "Ctrl+,",
            "Escape",
        ] {
            let c = Chord::parse(text).unwrap();
            assert_eq!(Chord::parse(&c.to_text()), Some(c.clone()), "{text}");
        }
    }

    #[test]
    fn reserved_chords_are_kept_per_owner() {
        let mut r = ReservedChords::default();
        assert!(r.is_empty());
        r.set("menubar", vec![Chord::parse_for("Ctrl+O", false).unwrap()]);
        r.set("commands", vec![Chord::parse_for("Ctrl+K", false).unwrap()]);
        let ctrl = mods(true, false, false, false);
        assert!(r.matches(&Key::Character("o".into()), KeyCode::KeyO, &ctrl));
        assert!(r.matches(&Key::Character("k".into()), KeyCode::KeyK, &ctrl));
        assert!(r.taken_by_other("menubar", &Key::Character("k".into()), KeyCode::KeyK, &ctrl));
        assert!(!r.taken_by_other(
            "commands",
            &Key::Character("k".into()),
            KeyCode::KeyK,
            &ctrl
        ));
        r.set("commands", Vec::new());
        assert!(!r.matches(&Key::Character("k".into()), KeyCode::KeyK, &ctrl));
        assert_eq!(r.of("menubar").len(), 1);
        r.remove("menubar");
        assert!(r.is_empty());
    }
}
