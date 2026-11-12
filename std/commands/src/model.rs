//! The registry itself: the commands an app declares, the keys the user
//! bound to them, and the work the plugin's systems have to do next. Plain
//! data, so every surface (scripts, the C ABI, Rust systems) reaches it
//! through one lock and one set of operations.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use bevy_ecs::prelude::Resource;
use lumen_module::lumen_core::chord::Chord;
use lumen_module::lumen_core::input::{Key, KeyCode, Modifiers};

/// The id of the built-in command that opens the palette.
pub const PALETTE: &str = "commands.palette";

/// One command.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    /// What it is called by: `commands::run(id)`, `<menuitem command>`.
    pub id: String,
    /// The label the markup or the script gave it.
    pub label: String,
    /// The catalogue message that translates the label (`translatable`).
    pub label_key: Option<String>,
    /// The chords it is declared with, before the user rebinds it.
    pub defaults: Vec<Chord>,
    /// Whether it runs. A disabled command keeps its chords, shows greyed
    /// out in menus, and is left out of the palette.
    pub enabled: bool,
    /// Declared by a `<command>` element, which owns it.
    pub from_markup: bool,
}

impl Command {
    /// The label in the app's language.
    pub fn label(&self) -> String {
        self.label_key
            .as_deref()
            .and_then(lumen_module::lumen_core::i18n::try_translate)
            .unwrap_or_else(|| self.label.clone())
    }
}

/// Why a call did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// No command has the id.
    Unknown(String),
    /// An id that is empty.
    EmptyId,
    /// A chord list entry that does not read as a chord.
    BadChord(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::Unknown(id) => write!(f, "no command has the id '{id}'"),
            CommandError::EmptyId => f.write_str("a command needs an id"),
            CommandError::BadChord(c) => write!(f, "'{c}' is not a key chord"),
        }
    }
}

impl std::error::Error for CommandError {}

/// Read a chord list, or say which entry is not a chord.
pub fn chords(text: &str) -> Result<Vec<Chord>, CommandError> {
    Chord::parse_list(text).map_err(CommandError::BadChord)
}

/// The registry and what is due.
#[derive(Debug, Default)]
pub struct Core {
    /// Every command, in the order it was declared: the palette lists them
    /// in this order, and the first whose chord matches a key runs.
    pub commands: Vec<Command>,
    /// The user's bindings, by command id. An empty list unbinds the
    /// command. Kept for ids no command has yet, so a binding saved for a
    /// command a script registers later applies when it does.
    pub overrides: BTreeMap<String, Vec<Chord>>,
    /// The command whose next chord binds it (`commands::record`).
    pub recording: Option<String>,
    /// Commands to run on the next tick, in order.
    pub queue: Vec<String>,
    /// What a call asked of the palette, for the next tick.
    pub palette: Option<PaletteAsk>,
    /// The chords, menus or palette list have to follow the registry.
    pub changed: bool,
    /// The user's bindings changed and are to be kept.
    pub save_due: bool,
    /// Commands whose chords changed (or whose recording ended), to tell
    /// the app with `keyschange`.
    pub rebound: Vec<String>,
}

/// What a call asked of the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAsk {
    /// Open it.
    Open,
}

impl Core {
    /// The command `id`.
    pub fn get(&self, id: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Command> {
        self.commands.iter_mut().find(|c| c.id == id)
    }

    /// Add a command, or replace the one with the same id. Keeps its place
    /// when it replaces one.
    pub fn register(&mut self, command: Command) -> Result<(), CommandError> {
        if command.id.trim().is_empty() {
            return Err(CommandError::EmptyId);
        }
        match self.get_mut(&command.id) {
            Some(slot) => *slot = command,
            None => self.commands.push(command),
        }
        self.changed = true;
        Ok(())
    }

    /// Drop the command `id`; whether there was one.
    pub fn unregister(&mut self, id: &str) -> bool {
        let before = self.commands.len();
        self.commands.retain(|c| c.id != id);
        if self.recording.as_deref() == Some(id) {
            self.recording = None;
        }
        let gone = self.commands.len() != before;
        self.changed |= gone;
        gone
    }

    /// Turn `id` on or off.
    pub fn set_enabled(&mut self, id: &str, on: bool) -> Result<(), CommandError> {
        let c = self
            .get_mut(id)
            .ok_or_else(|| CommandError::Unknown(id.to_string()))?;
        if c.enabled != on {
            c.enabled = on;
            self.changed = true;
        }
        Ok(())
    }

    /// The chords that run `id` now: the user's, else the declared ones.
    pub fn keys(&self, id: &str) -> Vec<Chord> {
        match self.overrides.get(id) {
            Some(bound) => bound.clone(),
            None => self.get(id).map(|c| c.defaults.clone()).unwrap_or_default(),
        }
    }

    /// The chords that run `id`, as text: `Ctrl+S, Ctrl+Shift+S`.
    pub fn keys_text(&self, id: &str) -> String {
        self.keys(id)
            .iter()
            .map(Chord::to_text)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Bind `id` to `chords` in place of what it had. A chord another
    /// command has moves to this one.
    pub fn bind(&mut self, id: &str, chords: Vec<Chord>) -> Result<(), CommandError> {
        if self.get(id).is_none() {
            return Err(CommandError::Unknown(id.to_string()));
        }
        let others: Vec<String> = self
            .commands
            .iter()
            .filter(|c| c.id != id)
            .map(|c| c.id.clone())
            .collect();
        for other in others {
            let theirs = self.keys(&other);
            if theirs.iter().any(|c| chords.contains(c)) {
                let kept = theirs.into_iter().filter(|c| !chords.contains(c)).collect();
                self.overrides.insert(other, kept);
            }
        }
        self.overrides.insert(id.to_string(), chords);
        self.changed = true;
        self.save_due = true;
        self.rebound.push(id.to_string());
        Ok(())
    }

    /// Give `id` its declared chords back; whether the user had bound it.
    pub fn reset(&mut self, id: &str) -> bool {
        let had = self.overrides.remove(id).is_some();
        self.changed |= had;
        self.save_due |= had;
        if had {
            self.rebound.push(id.to_string());
        }
        had
    }

    /// Bind the next chord pressed to `id`.
    pub fn record(&mut self, id: &str) -> Result<(), CommandError> {
        if self.get(id).is_none() {
            return Err(CommandError::Unknown(id.to_string()));
        }
        self.recording = Some(id.to_string());
        Ok(())
    }

    /// Queue `id` to run; whether it will (it exists and is enabled).
    pub fn run(&mut self, id: &str) -> bool {
        if self.get(id).is_some_and(|c| c.enabled) {
            self.queue.push(id.to_string());
            true
        } else {
            false
        }
    }

    /// The first enabled command one of whose chords is this key press.
    pub fn find(&self, key: &Key, code: KeyCode, modifiers: &Modifiers) -> Option<&str> {
        self.commands
            .iter()
            .filter(|c| c.enabled)
            .find(|c| match self.overrides.get(&c.id) {
                Some(bound) => bound.iter().any(|ch| ch.matches(key, code, modifiers)),
                None => c.defaults.iter().any(|ch| ch.matches(key, code, modifiers)),
            })
            .map(|c| c.id.as_str())
    }

    /// Every chord an enabled command answers, for the reserved chords.
    pub fn all_chords(&self) -> Vec<Chord> {
        self.commands
            .iter()
            .filter(|c| c.enabled)
            .flat_map(|c| self.keys(&c.id))
            .collect()
    }

    /// The enabled commands whose label holds every word of `query`, case
    /// aside, in declaration order: `(id, label, chord label)`. The palette's
    /// own command is left out.
    pub fn matching(&self, query: &str) -> Vec<(String, String, String)> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.commands
            .iter()
            .filter(|c| c.enabled && c.id != PALETTE)
            .filter_map(|c| {
                let label = c.label();
                let lower = label.to_lowercase();
                words.iter().all(|w| lower.contains(w.as_str())).then(|| {
                    let chord = self.keys(&c.id).first().map(Chord::to_string);
                    (c.id.clone(), label, chord.unwrap_or_default())
                })
            })
            .collect()
    }
}

/// The registry, shared between the plugin's systems and every surface that
/// calls into it.
#[derive(Resource, Clone, Debug, Default)]
pub struct Shared(Arc<Mutex<Core>>);

impl Shared {
    /// Lock the registry. A panic while it was held leaves it as it was.
    pub fn lock(&self) -> MutexGuard<'_, Core> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(id: &str, label: &str, keys: &str) -> Command {
        Command {
            id: id.into(),
            label: label.into(),
            label_key: None,
            defaults: chords(keys).unwrap(),
            enabled: true,
            from_markup: false,
        }
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: !cfg!(target_os = "macos"),
            super_: cfg!(target_os = "macos"),
            ..Modifiers::default()
        }
    }

    fn key(c: &str) -> Key {
        Key::Character(c.into())
    }

    #[test]
    fn the_first_enabled_match_wins() {
        let mut core = Core::default();
        core.register(cmd("a", "A", "CmdOrCtrl+K")).unwrap();
        core.register(cmd("b", "B", "CmdOrCtrl+K")).unwrap();
        assert_eq!(core.find(&key("k"), KeyCode::KeyK, &ctrl()), Some("a"));
        core.set_enabled("a", false).unwrap();
        assert_eq!(core.find(&key("k"), KeyCode::KeyK, &ctrl()), Some("b"));
        assert!(!core.run("a"), "a disabled command does not run");
        assert!(core.run("b"));
        assert_eq!(core.queue, vec!["b"]);
        assert!(!core.run("nope"));
    }

    #[test]
    fn binding_takes_a_chord_from_the_command_that_had_it() {
        let mut core = Core::default();
        core.register(cmd("save", "Save", "CmdOrCtrl+S")).unwrap();
        core.register(cmd("sync", "Sync", "CmdOrCtrl+Y, F5"))
            .unwrap();
        core.bind("save", chords("F5").unwrap()).unwrap();
        assert_eq!(core.keys("save"), chords("F5").unwrap());
        assert_eq!(core.keys("sync"), chords("CmdOrCtrl+Y").unwrap());
        assert!(core.save_due);
        assert!(core.reset("save"));
        assert_eq!(core.keys("save"), chords("CmdOrCtrl+S").unwrap());
        assert!(!core.reset("save"), "nothing left to reset");
        assert_eq!(
            core.bind("ghost", Vec::new()),
            Err(CommandError::Unknown("ghost".into()))
        );
    }

    #[test]
    fn registering_again_replaces_in_place_and_unregistering_drops() {
        let mut core = Core::default();
        core.register(cmd("a", "A", "")).unwrap();
        core.register(cmd("b", "B", "")).unwrap();
        core.register(cmd("a", "Again", "F2")).unwrap();
        assert_eq!(core.commands[0].label, "Again");
        assert_eq!(core.commands.len(), 2);
        assert!(core.unregister("a"));
        assert!(!core.unregister("a"));
        assert_eq!(core.register(cmd(" ", "", "")), Err(CommandError::EmptyId));
    }

    #[test]
    fn the_palette_matches_every_word_of_the_query() {
        let mut core = Core::default();
        core.register(cmd("file.save", "Save file", "CmdOrCtrl+S"))
            .unwrap();
        core.register(cmd("file.save_all", "Save all files", ""))
            .unwrap();
        core.register(cmd("view.zoom", "Zoom in", "")).unwrap();
        core.register(cmd(PALETTE, "Show all commands", ""))
            .unwrap();
        fn ids(core: &Core, q: &str) -> Vec<String> {
            core.matching(q).into_iter().map(|(id, ..)| id).collect()
        }
        assert_eq!(
            ids(&core, ""),
            vec!["file.save", "file.save_all", "view.zoom"]
        );
        assert_eq!(ids(&core, "SAVE file"), vec!["file.save", "file.save_all"]);
        assert_eq!(ids(&core, "all save"), vec!["file.save_all"]);
        assert!(ids(&core, "save zoom").is_empty());
        core.set_enabled("view.zoom", false).unwrap();
        assert!(
            ids(&core, "zoom").is_empty(),
            "disabled commands are left out"
        );
        assert!(
            !core.matching("save file")[0].2.is_empty(),
            "shows its chord"
        );
    }
}
