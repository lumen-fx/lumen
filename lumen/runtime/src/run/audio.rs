use super::*;

/// Marker resource holding the dedicated audio-player entity. The entity
/// carries an [`lumen_assets::AudioSource`] whenever a track is requested,
/// so track loading flows through the same async asset pipeline as images.
#[derive(Resource, Clone, Copy)]
pub(crate) struct AudioPlayerEntity(pub(crate) Entity);

/// One-shot flag: set by [`poll_audio`] when a playing track reaches its
/// end, consumed by [`fire_audio_ended`] to invoke the script's optional
/// `on_audio_end()` handler (auto-advance). Kept as a resource so the two
/// systems can sit at their correct schedule positions.
#[derive(Resource, Default)]
pub(crate) struct AudioEndedFlag(bool);

/// Applies audio transport [`ScriptCommand`]s. `AudioPlay` routes the
/// track through the `AssetServer` (set `AudioSource` on the player
/// entity, mirroring `SetSrc` for images); the transport controls act on
/// the `AudioService` directly.
pub(crate) fn apply_audio_commands(
    mut events: MessageReader<ScriptCommandEvent>,
    mut audio: NonSendMut<lumen_audio::AudioService>,
    player: Res<AudioPlayerEntity>,
    mut server: ResMut<lumen_assets::AssetServer>,
    mut commands: Commands,
    hot: Option<Res<HotReloadState>>,
) {
    let dir: PathBuf = hot
        .as_ref()
        .map(|h| h.dir.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    for ev in events.read() {
        match &ev.0 {
            ScriptCommand::AudioPlay { path } => {
                let p = Path::new(path);
                let resolved = if p.is_relative() {
                    dir.join(p)
                } else {
                    p.to_path_buf()
                };
                // Mirror the image SetSrc discipline: bump the request id
                // so an in-flight decode for the previous track is treated
                // as stale, strip prior results, install a fresh source.
                server.bump_request_id(player.0);
                let mut ent = commands.entity(player.0);
                ent.remove::<lumen_assets::LoadedAudio>();
                ent.remove::<lumen_assets::AudioLoadFailed>();
                ent.remove::<lumen_assets::Enqueued>();
                ent.insert(lumen_assets::AudioSource(resolved));
            }
            ScriptCommand::AudioPause => audio.pause(),
            ScriptCommand::AudioResume => audio.resume(),
            ScriptCommand::AudioStop => audio.stop(),
            ScriptCommand::AudioSeek { secs } => audio.seek(*secs),
            ScriptCommand::AudioVolume { level } => audio.set_volume(*level),
            _ => {}
        }
    }
}

/// When the player entity's [`lumen_assets::LoadedAudio`] resolves (cache
/// hit or decode completion - both surface as `Changed<LoadedAudio>`),
/// hand its bytes to the `AudioService` to start playback. Also logs a
/// resolved load failure once.
pub(crate) fn apply_loaded_audio(
    mut audio: NonSendMut<lumen_audio::AudioService>,
    loaded: Query<&lumen_assets::LoadedAudio, Changed<lumen_assets::LoadedAudio>>,
    failed: Query<&lumen_assets::AudioLoadFailed, Added<lumen_assets::AudioLoadFailed>>,
    player: Res<AudioPlayerEntity>,
) {
    if let Ok(track) = loaded.get(player.0)
        && let Err(e) = audio.play_bytes(track.0.bytes.clone())
    {
        eprintln!("lumen-audio: {e}");
    }
    if let Ok(fail) = failed.get(player.0) {
        eprintln!("lumen-audio: track failed to load: {}", fail.detail);
    }
}

/// Pushes the transport position/duration/playing into signals each woken
/// tick (Slint `invoke_from_event_loop` discipline: the off-thread ticker
/// only wakes the loop; this UI-thread system does the signal write).
/// Ordered before `sync_signals_into_host` so `derive()`s over these
/// signals recompute the same tick. Sets [`AudioEndedFlag`] on natural end.
pub(crate) fn poll_audio(
    mut audio: NonSendMut<lumen_audio::AudioService>,
    mut store: ResMut<lumen_core::property_store::PropertyStore>,
    mut ended: ResMut<AudioEndedFlag>,
    waker: Option<Res<lumen_core::app::EventLoopWaker>>,
) {
    // Wire the loop waker lazily (the resource appears after plugin build).
    if let Some(w) = waker.as_deref() {
        audio.set_waker(w.clone());
    }
    let snap = audio.refresh();
    // Stringified so the values flow through the same mirror/derive path
    // as script-written signals (`mirror_sync_str` parses them back into
    // the float/string the app's derives seeded).
    store.set_global_str("audio_position", format!("{:.3}", snap.position));
    store.set_global_str("audio_duration", format!("{:.3}", snap.duration));
    store.set_global_str("audio_playing", if snap.playing { "true" } else { "false" });
    if snap.ended {
        ended.0 = true;
    }
}

/// Invokes the script's optional `on_audio_end()` when a track finishes,
/// enabling auto-advance. Mirrors `fire_fetched_responses`: calls the host
/// and forwards the produced commands onto the bus.
pub(crate) fn fire_audio_ended<H: ScriptHost + Resource>(
    mut host: Option<ResMut<H>>,
    mut ended: ResMut<AudioEndedFlag>,
    mut out: MessageWriter<ScriptCommandEvent>,
) {
    if !ended.0 {
        return;
    }
    ended.0 = false;
    if let Some(host) = host.as_mut()
        && let Ok(outcome) = host.call("on_audio_end", &[])
    {
        for c in outcome.commands {
            out.write(ScriptCommandEvent(c));
        }
    }
}
