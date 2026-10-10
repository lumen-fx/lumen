# OS capability events are plugin events, not core message types

This affects Rust code that reads or writes the events the OS capabilities
report: tray clicks, global hotkeys, notification buttons, file dialog
results, clipboard reads, recent-files and autostart reads, and second
launches of a single-instance app. Scripts are not affected; `on_tray(id)`,
`on("hotkey", name, fn)` and the other handlers fire as before.

The message types `TrayClicked`, `HotkeyFired`, `HotkeyReleased`,
`NotificationActionInvoked`, `FilePicked`, `ClipboardRead`,
`RecentFilesRead`, `AutostartRead` and `SecondInstanceLaunched` are gone from
`lumen_core::input`, along with the re-exports `lumen_os_tray::TrayClicked`,
`lumen_os_hotkey::{HotkeyPressed, HotkeyReleased}`,
`lumen_os_notify::NotificationActionInvoked` and
`lumen_os_filedialog::FileDialogResult`. Each capability now writes a
`lumen_script::PluginEvent`, built by a constructor the capability exports.
`lumen_os_filedialog::FileDialogService::open` and
`drain_file_dialog_results` are removed; use `open_single` and
`FileDialogPlugin` (or `register_result_handler`).

Before:

```rust
app.world.write_message(lumen_core::input::TrayClicked { id: "main".into() });
```

After:

```rust
app.world.write_message(lumen_os_tray::tray_click_event("main".into()));
```

The constructors are `lumen_os_tray::tray_click_event`,
`lumen_os_hotkey::hotkey_event`, `lumen_os_notify::action_event`,
`lumen_os_lifecycle::{second_instance_event, recent_files_event,
autostart_event}`, `lumen_script::clipboard::clipboard_read_event`, and
`PluginEvent::from(FileDialogResultCommand)`. To observe these events from
Rust, read `MessageReader<PluginEvent>` and match on the `event` name.
