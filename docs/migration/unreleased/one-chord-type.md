# Key chords are one type, `lumen_core::chord::Chord`

This affects Rust code that used the chord types Lumen had before.

- `lumen_dock::Chord` is gone; `lumen_dock::Keys` holds
  `lumen_core::chord::Chord` values. `Chord::parse("Ctrl+F6")` reads the same
  text as before, and also takes `Super`, `Cmd`, `CommandOrControl` and key
  names in any case. A dock chord now has to match the logo key too: `F6` no
  longer fires with Super held.
- `lumen_primitives::MenuAccelerators::matches` is gone. To ask whether the
  app answers a chord app-wide, read the `lumen_core::chord::ReservedChords`
  resource, which the menu bar and the commands module fill.
- `lumen_os_mime::KeyChord` is gone, and `lumen_os_mime::Action` keeps only
  `id` and `label`, what a tray menu entry and a notification button use:
  `icon`, `shortcut`, `enabled`, `checked` and their builder methods are gone.
  `lumen_os_menu` no longer re-exports `Action` and `KeyChord` and loses
  `MenuBuilder::action_entry` and `default_action_ids`. Build a menu entry as a
  `MenuEntry::Item` directly; declare commands with the `lumen-commands`
  module.
- `lumen_ir::menu::shortcut_label` reads the accelerator through `Chord`; text
  that is not a chord is shown as written.
