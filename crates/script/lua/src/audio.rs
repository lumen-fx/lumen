//! Audio-control builtins for the Lua host.
//!
//! Mirror of the Rhai host's `audio` module: every function is a thin
//! enqueue that pushes a backend-agnostic [`ScriptCommand`] onto the
//! shared command sink and returns. The actual playback happens in the
//! embedder's applier against the `lumen-audio` `AudioService`. Because
//! the capability is a `ScriptCommand` - not an engine-only primitive -
//! every binding drives the exact same seam.
//!
//! Lua numbers are dynamically typed, so a single `f64`-accepting
//! function covers both `audio_seek(30.5)` and `audio_seek(30)` (the
//! integer literal coerces to `f64` on the FFI boundary) - no need for
//! the paired int/float overloads the statically-dispatched Rhai host
//! registers.

use std::sync::Arc;

use lumen_script::ScriptCommand;
use mlua::Lua;
use parking_lot::Mutex;

/// Register `audio_*` control builtins on `lua`, each pushing onto the
/// shared `sink`. Mirror of Qt's `QMediaPlayer` control slots.
pub(crate) fn register(lua: &Lua, sink: &Arc<Mutex<Vec<ScriptCommand>>>) -> mlua::Result<()> {
    let globals = lua.globals();

    let s = sink.clone();
    globals.set(
        "audio_play",
        lua.create_function(move |_, path: String| {
            s.lock().push(ScriptCommand::AudioPlay { path });
            Ok(())
        })?,
    )?;

    let s = sink.clone();
    globals.set(
        "audio_pause",
        lua.create_function(move |_, ()| {
            s.lock().push(ScriptCommand::AudioPause);
            Ok(())
        })?,
    )?;

    let s = sink.clone();
    globals.set(
        "audio_resume",
        lua.create_function(move |_, ()| {
            s.lock().push(ScriptCommand::AudioResume);
            Ok(())
        })?,
    )?;

    let s = sink.clone();
    globals.set(
        "audio_stop",
        lua.create_function(move |_, ()| {
            s.lock().push(ScriptCommand::AudioStop);
            Ok(())
        })?,
    )?;

    let s = sink.clone();
    globals.set(
        "audio_seek",
        lua.create_function(move |_, secs: f64| {
            s.lock().push(ScriptCommand::AudioSeek { secs });
            Ok(())
        })?,
    )?;

    let s = sink.clone();
    globals.set(
        "audio_volume",
        lua.create_function(move |_, level: f64| {
            s.lock().push(ScriptCommand::AudioVolume {
                level: level as f32,
            });
            Ok(())
        })?,
    )?;

    Ok(())
}
