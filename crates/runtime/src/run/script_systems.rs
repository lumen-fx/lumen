use super::*;

/// Rebuild the per-tick [`DomIndex`] snapshot from the live main-world
/// tree and publish it for cross-thread readers (script hosts, the C-ABI).
/// Every spawned element carries a [`LumenTag`], so the query walks all
/// selector-reachable entities; a parent or child that is not itself
/// tagged (e.g. the window root) is dropped from the element tree.
///
/// Runs in [`TickStage::Systems`] before the script / input dispatchers so
/// a `query()` issued from an event handler observes this tick's tree.
#[allow(clippy::type_complexity)]
pub(crate) fn build_dom_index(
    query: Query<(
        Entity,
        &LumenTag,
        Option<&LumenClasses>,
        Option<&LumenId>,
        Option<&ChildOf>,
        Option<&Children>,
    )>,
) {
    use std::collections::HashSet;
    let indexed: HashSet<u64> = query.iter().map(|(e, ..)| e.to_bits()).collect();
    let mut records: Vec<DomRecord> = Vec::with_capacity(indexed.len());
    for (entity, tag, classes, id, child_of, children) in query.iter() {
        let parent = child_of
            .map(|c| c.parent())
            .filter(|p| indexed.contains(&p.to_bits()));
        let kids: Vec<Entity> = children
            .map(|c| {
                c.iter()
                    .filter(|e| indexed.contains(&e.to_bits()))
                    .collect()
            })
            .unwrap_or_default();
        records.push(DomRecord {
            entity,
            generation: entity.generation().to_bits(),
            tag: tag.0.to_string(),
            id: id.map(|i| i.0.clone()),
            classes: classes
                .map(|c| c.0.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default(),
            parent,
            children: kids,
            child_index: 0,
            sibling_count: 0,
            doc_order: 0,
        });
    }
    lumen_core::node::publish_dom_index(DomIndex::build(records));
}

/// Install the host-neutral half of the script wiring: the DOM snapshot
/// publishers, the mutation pipeline, the two-way binding readers and pushes,
/// and the command applier. Called once per app however many hosts are active.
///
/// Every RC-critical ordering edge anchors on [`lumen_script::ScriptSet`], not
/// on a concrete `system::<H>`. That is what makes several hosts safe: an edge
/// naming one host's `apply_derivations` says nothing about the others, so the
/// one-tick dirty window would close unobserved for every host it did not name,
/// and dirty-gated readers would freeze at their spawn value.
///
/// The binding / push systems are unconditional: they run even with no script
/// installed, in which case their set edges are inert (the sets have zero
/// members). The audio + `apply_script_commands` systems (`has_script`) only
/// exist when the app ships a script.
pub(crate) fn register_script_common(app: &mut App, has_script: bool) {
    // Same-tick signal commit: a script `on_click` handler pushes its
    // `signals.x.set(..)` write onto the cross-thread property bus from
    // inside the `Systems`-stage dispatch. The global drain runs back in
    // `CommandDrain` (before `Systems`), so without a second drain here the
    // write would only land in `PropertyStore` on the NEXT tick - one full
    // frame of input latency before a `bind` reader sees it. Draining again
    // right after the script dispatch commits the write in-tick; the
    // binding readers below then run `.after` this drain and observe the
    // fresh value on the same tick the click fired.
    //
    // `.before(ScriptSet::Derivations)`: derivations consult the store's dirty
    // queue, and `clear_property_store_dirty` (A11ySync) empties it every
    // tick - so a typed dep write committed here must land BEFORE the
    // derivation pass or its one-tick dirty window closes unobserved and
    // the derived signal freezes (RC1). Edge is inert when no script is
    // installed (the set has no members), same as the `ScriptSet::Dispatch`
    // references above.
    // Dynamic DOM read side: rebuild + publish the query snapshot before
    // any handler runs, so `query()` / traversal from an `on_click` body
    // sees this tick's tree. Unconditional (runs with or without a script,
    // so the C-ABI / SDK can query a script-less app).
    app.add_systems(
        TickStage::Systems,
        build_dom_index
            .before(ScriptSet::Dispatch)
            .before(lumen_input::dispatch_focused_keys),
    );
    // Publish per-node detail (text / generic attrs / inline style) + the
    // cascade inputs `computed_style` consumes, alongside the DomIndex so a
    // read from a handler sees this tick's tree. Unconditional (works for a
    // script-less app queried over the C-ABI).
    app.add_systems(
        TickStage::Systems,
        crate::run::dom_commands::publish_node_details
            .before(ScriptSet::Dispatch)
            .before(lumen_input::dispatch_focused_keys),
    );
    // Phase-5 low-level introspection snapshot: geometry, component field
    // maps, pointer / frame state, signals. Same ordering discipline as the
    // detail publish so an inspection read from a handler sees this tick.
    // Unconditional (works for a script-less app inspected over the C-ABI).
    app.world
        .insert_resource(crate::run::dom_commands::FrameClock::default());
    app.add_systems(
        TickStage::Systems,
        crate::run::dom_commands::publish_introspection
            .before(ScriptSet::Dispatch)
            .before(lumen_input::dispatch_focused_keys),
    );
    // Dynamic DOM mutation pipeline. `collect_dom_commands` gathers this
    // tick's DOM / window commands from the script event stream and the
    // external (C-ABI / SDK) bus in issue order; `apply_dom_commands` is an
    // exclusive system that materializes spawns, reparents, and component
    // edits against `&mut World`. Both run unconditionally so a script-less
    // app still applies C-ABI / SDK mutations.
    //
    // `collect_dom_commands` reads `ScriptCommandEvent`; the script plugin
    // registers that message only when a script is installed, so a
    // script-less app must self-register it here or the reader fails
    // parameter validation.
    if !has_script {
        bevy_ecs::message::MessageRegistry::register_message::<ScriptCommandEvent>(&mut app.world);
        // No host means `register_script_host_systems` never runs, so install
        // the DOM-event dispatchers here against whichever host this build
        // carries. They take an optional host and deliver to C-ABI / SDK
        // native handlers, so any compiled host serves; a build with none
        // installs nothing and delivers no DOM events to native handlers.
        #[cfg(feature = "host-rhai")]
        register_dom_event_dispatchers::<RhaiHost>(app);
        #[cfg(all(not(feature = "host-rhai"), feature = "host-candela"))]
        register_dom_event_dispatchers::<CandelaHost>(app);
        #[cfg(all(
            not(feature = "host-rhai"),
            not(feature = "host-candela"),
            feature = "host-lua"
        ))]
        register_dom_event_dispatchers::<LuaHost>(app);
    }
    app.world
        .insert_resource(crate::run::dom_commands::PendingDomCommands::default());
    app.add_systems(
        TickStage::Systems,
        crate::run::dom_commands::collect_dom_commands
            .after(ScriptSet::Tick)
            .after(ScriptSet::Dispatch)
            .after(ScriptSet::Ready)
            .after(ScriptSet::DomEvents),
    );
    app.add_systems(
        TickStage::Systems,
        crate::run::dom_commands::apply_dom_commands
            .after(crate::run::dom_commands::collect_dom_commands),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::property_store::commit_external_properties
            .after(ScriptSet::Dispatch)
            .before(ScriptSet::Derivations),
    );
    // Signal -> TextContent must land BEFORE the keystroke / IME path so a
    // user's mid-tick keystroke always wins over an external signal write,
    // and so the gate (Without<Focused>, Without<ImeState>) inside
    // apply_text_bindings has the latest focus markers to consult. It also
    // runs AFTER `commit_external_properties` (hence after
    // `dispatch_clicks_and_doubles`) so a same-tick script write is already
    // committed to `PropertyStore` when the binding reads it. These two
    // constraints do not conflict: `type_into_focused` descends from
    // `dispatch_focused_keys`/`dispatch_clicks` on a separate chain that
    // also feeds it, so `dispatch_clicks_and_doubles -> commit -> binding ->
    // type_into_focused` is acyclic.
    // All three pull-binding readers additionally run `.after(
    // apply_derivations)` so a derived-signal recompute (which writes the
    // store directly, same tick) is observed while its dirty flag is
    // still set - the readers early-return on an empty dirty queue.
    // Every dirty-gated reader also runs `.after(push_*)`: a widget-driven
    // store write (toggle flip, slider drag, keystroke mirror) marks its
    // key dirty only until `clear_property_store_dirty` (A11ySync) empties
    // the queue at end of tick. Scheduled before the push on the write
    // tick, a reader would miss the flag and then fast-path-return every
    // tick after - the bound label freezes at its spawn value. (This is
    // the same hazard `apply_disabled_bindings` below documents; the
    // slider->`bind-text` label in the widget garden was the live repro.)
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::apply_text_bindings
            .after(lumen_core::property_store::commit_external_properties)
            .after(ScriptSet::Derivations)
            .after(lumen_core::signals::push_toggle_to_signal)
            .after(lumen_core::signals::push_slider_to_signal)
            .after(lumen_core::signals::push_textinput_to_signal)
            .before(lumen_input::type_into_focused),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::apply_checked_bindings
            .after(lumen_core::property_store::commit_external_properties)
            .after(ScriptSet::Derivations)
            .after(lumen_core::signals::push_toggle_to_signal)
            .after(lumen_core::signals::push_slider_to_signal)
            .after(lumen_core::signals::push_textinput_to_signal),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::apply_value_bindings
            .after(lumen_core::property_store::commit_external_properties)
            .after(ScriptSet::Derivations)
            .after(lumen_core::signals::push_toggle_to_signal)
            .after(lumen_core::signals::push_slider_to_signal)
            .after(lumen_core::signals::push_textinput_to_signal),
    );
    // W6 T6: `bind-scroll` pull half - signal (f32 px) drives the vertical
    // scroll offset. Same dirty-gated reader shape and ordering as the
    // value/checked bindings above (reader AFTER pushes + AFTER
    // apply_derivations, per the 7bfc0f2 rules), plus
    // `.after(push_scroll_to_signal)` so a user-scroll settle push on the
    // same tick is observed while its dirty flag is still set.
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::apply_scroll_bindings
            .after(lumen_core::property_store::commit_external_properties)
            .after(ScriptSet::Derivations)
            .after(lumen_core::signals::push_toggle_to_signal)
            .after(lumen_core::signals::push_slider_to_signal)
            .after(lumen_core::signals::push_textinput_to_signal)
            .after(lumen_core::signals::push_scroll_to_signal),
    );
    // Wave 3: `bind-disabled` - signal drives the `Disabled` marker so
    // scripts can enable / disable widgets live. Same dirty-gated pull
    // shape and ordering as the checked / value bindings above. The
    // marker add is observed by `eject_interaction_on_disable` +
    // `apply_state_visuals` (lumen-primitives) for state stripping and
    // the `:disabled` style swap.
    //
    // The three `.after(push_*)` edges matter because of the one-tick
    // dirty window: a widget-driven store write (toggle flip, slider
    // drag, keystroke) marks the key dirty only until `A11ySync` clears
    // the queue at end of tick. Without the edges this dirty-gated
    // reader could be scheduled *before* the push on the write tick and
    // the disable would be missed forever (`<toggle bind-checked="locked">`
    // gating `<button bind-disabled="locked">` is the canonical case).
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::apply_disabled_bindings
            .after(lumen_core::property_store::commit_external_properties)
            .after(ScriptSet::Derivations)
            .after(lumen_core::signals::push_toggle_to_signal)
            .after(lumen_core::signals::push_slider_to_signal)
            .after(lumen_core::signals::push_textinput_to_signal),
    );
    // Two-way binding push: when an input mutates its TextContent /
    // Toggleable / SliderValue, mirror back into the signal. The Pull
    // half above is idempotent so they don't fight - the No-op equality
    // checks in each function ensure stable state.
    // `.before(ScriptSet::Derivations)`: derivations are dirty-gated too, so a
    // control write pushed after the derivation pass on the write tick
    // would never recompute the derived signal (the garden's
    // `toggle_status` freezing on toggle flips was the live repro).
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::push_textinput_to_signal.before(ScriptSet::Derivations),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::push_toggle_to_signal.before(ScriptSet::Derivations),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::push_slider_to_signal.before(ScriptSet::Derivations),
    );
    // W6 T6: `bind-scroll` push half - the settled scroll offset mirrors
    // back into the signal (throttled to scroll-settle inside the system,
    // never per-frame). `.before(ScriptSet::Derivations)` per the 7bfc0f2 push
    // rules; the `.before(sync_signals_into_host)` edge lives in
    // lumen-script-rhai's registration (expressed as its
    // `.after(push_scroll_to_signal)`).
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::push_scroll_to_signal.before(ScriptSet::Derivations),
    );
    // W5 dialog contract (Qt QDialog): Enter-anywhere activates the default
    // button. Ordered after the focused-key fanout (same-tick keystroke) and
    // before the script click dispatch so the synthesized ClickEvent reaches
    // `on_click` handlers on this very tick.
    app.add_systems(
        TickStage::Systems,
        crate::spawn::activate_dialog_default_on_enter
            .after(lumen_input::dispatch_focused_keys)
            .before(ScriptSet::Dispatch),
    );
    if has_script {
        // `apply_script_commands` is the sole applier of script-produced
        // `SetSignal` / `SetArray` writes into `PropertyStore` /
        // `ArraySignals`. Its ordering is load-bearing post perf dirty-gating:
        //
        //  * `.after(ScriptSet::Tick)` / `.after(ScriptSet::Dispatch)` -
        //    those systems emit the `ScriptCommandEvent`s this drains (the
        //    `on_start` backlog and click-handler writes respectively).
        //    Running after them applies this tick's writes in-tick instead
        //    of lagging a frame through the message double-buffer.
        //
        //  * `.before(ScriptSet::Derivations)` - RC1 fix. Derivations consult the
        //    store's per-tick dirty queue, which `clear_property_store_dirty`
        //    (A11ySync) empties every tick - a signal write is dirty for
        //    exactly one tick. The previous `.after(ScriptSet::Derivations)` edge
        //    was exactly backwards: every `SetSignal` dep write landed after
        //    the derivation pass had already run, its dirty flag was gone by
        //    the next tick, and `derive()` signals froze at their startup
        //    value forever (counter stuck at "clicks: 0" through any number
        //    of clicks). With this edge the derivation pass sees the write
        //    the same tick it lands; `apply_derivations` writes derived
        //    results straight into the store (cascading in-tick), and the
        //    binding readers below run after it.
        //
        //  * `.before(apply_text_bindings / apply_checked_bindings /
        //    apply_value_bindings)` - the pull-binding readers early-return
        //    when the `PropertyStore` dirty queue is empty (perf gate). If a
        //    binding ran *before* this applier it would observe an empty
        //    dirty queue on the write's tick and never run again - the
        //    "blank bind-text after on_start" regression. This edge
        //    guarantees the write lands, and its dirty flag is still set,
        //    before the readers gate on it.
        //
        //  * `.before(reconcile_for_blocks)` - makes `<for>` rows appear on the
        //    same tick the backing array is populated rather than the next one
        //    (the reconciler is un-gated so it would converge regardless, but
        //    same-tick keeps first-frame output correct).
        //  * `.after(ScriptSet::Dispatch)` also covers `dispatch_close_to_script`
        //    - the `on_close` hook runs on the veto tick that follows an OS
        //    close request; when the close commits, that tick is the app's
        //    LAST. Without this edge the commands `on_close` emits (final
        //    signal writes, prints) would sit in the message buffer for a next
        //    tick that never runs and be silently dropped at exit.
        app.add_systems(
            TickStage::Systems,
            apply_script_commands
                .after(ScriptSet::Tick)
                .after(ScriptSet::Dispatch)
                .before(ScriptSet::Derivations)
                .before(lumen_core::signals::apply_text_bindings)
                .before(lumen_core::signals::apply_checked_bindings)
                .before(lumen_core::signals::apply_value_bindings)
                .before(crate::spawn::reconcile_for_blocks)
                // on_audio_end (auto-advance) may emit SetSignal commands.
                .after(ScriptSet::AudioEnded),
        );
        // Second applier, for the OS-host commands (notifications, clipboard,
        // launcher, sleep inhibit); its doc has why they are not arms of
        // `apply_script_commands`.
        app.add_systems(
            TickStage::Systems,
            apply_os_script_commands
                .after(ScriptSet::Tick)
                .after(ScriptSet::Dispatch),
        );
        // Audio transport wiring. COMPILE-TIME GATE (Part B tree-shaking):
        // only registered when the `audio` feature is compiled in. The
        // `.after(ScriptSet::AudioEnded)` edge on `apply_script_commands` above
        // stays valid in a no-audio build: the set then has no members and the
        // edge is a no-op.
        //
        // `poll_audio` pushes position/duration/playing into the store
        // *before* the host mirror sync so `derive()`s over them recompute
        // this tick (the same store->mirror->derive discipline every other
        // signal follows).
        #[cfg(feature = "audio")]
        {
            app.add_systems(
                TickStage::Systems,
                poll_audio.before(ScriptSet::SyncSignals),
            );
            // `apply_loaded_audio` starts playback once the AssetServer resolves
            // the track bytes; runs after the shared decode drain.
            app.add_systems(
                TickStage::Systems,
                apply_loaded_audio.after(lumen_assets::drain_completed_decodes),
            );
            // The end-of-track flag is cleared once, after every host has been
            // offered `on_audio_end`, so a second host still sees it.
            app.add_systems(
                TickStage::Systems,
                clear_audio_ended.after(ScriptSet::AudioEnded),
            );
            // `apply_audio_commands` applies transport commands + routes
            // `audio_play` through the AssetServer.
            app.add_systems(
                TickStage::Systems,
                apply_audio_commands
                    .after(ScriptSet::Tick)
                    .after(ScriptSet::Dispatch)
                    .after(ScriptSet::AudioEnded),
            );
        }
    }
}

/// Dynamic DOM events (phase 4): turn input messages into DOM events and run
/// capture -> target -> bubble propagation over the binding registry.
///
/// Ordered like the legacy `on_click` dispatch: after the input producers and
/// the snapshot build, before `collect_dom_commands` so a handler's queued DOM
/// mutations apply this same tick. `.before(navigate_on_anchor_click)` lets a
/// `prevent_default` on a link click be observed before the anchor-navigation
/// executor runs.
///
/// Both dispatchers take an optional host, so a script-less app still installs
/// them (against any host this build compiled) and delivers to C-ABI / SDK
/// native handlers.
fn register_dom_event_dispatchers<H: lumen_script::ScriptHost + Resource<Mutability = Mutable>>(
    app: &mut App,
) {
    app.add_systems(
        TickStage::Systems,
        lumen_script::dispatch_pointer_and_key_events::<H>
            .in_set(ScriptSet::DomEvents)
            .after(build_dom_index)
            .after(lumen_input::dispatch_clicks)
            .after(lumen_input::dispatch_focused_keys)
            .before(crate::pages::navigate_on_anchor_click),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_script::dispatch_state_events::<H>
            .in_set(ScriptSet::DomEvents)
            .after(build_dom_index)
            // `input` is derived from the edit stream, so this reads the
            // messages the text mutator writes. Anchor the edge on the
            // shared set label: an edit applied this tick raises `input`
            // on the same tick, in one fixed order, instead of leaving
            // the reader and the writer ambiguous for the executor to
            // interleave however it likes. Inert when no text plugin is
            // installed (empty set).
            .after(lumen_core::text_events::TextEditSet::Apply),
    );
}

/// Install the per-host half of the script wiring, once for each active
/// [`ScriptHost`]. Every system here joins a [`lumen_script::ScriptSet`], so
/// the host-neutral edges in [`register_script_common`] cover it without
/// naming its concrete type.
pub(crate) fn register_script_host_systems<
    H: lumen_script::ScriptHost + Resource<Mutability = Mutable>,
>(
    app: &mut App,
    multi_host: bool,
) {
    // RC6: a script that failed to load at plugin build leaves
    // `ScriptLoadFailure` behind. Mirror it into the in-app error banner so the
    // failure is visible in the window itself, not only in the stderr banner
    // the plugin printed. Read here, right after this host's plugin installed.
    if let Some(fail) = app.world.get_resource::<lumen_script::ScriptLoadFailure>() {
        let msg = format!("script load failed: {}", fail.0);
        app.world.resource_mut::<ErrorBanner>().0 = Some(msg);
    }
    // Cross-host signal reads. A host keeps its own mirror current as its
    // builtins run, so with one host the early `ScriptSet::SyncSignals` pass
    // is all that is needed and this stays unregistered. With two, a signal
    // written in one language reaches `PropertyStore` only when
    // `apply_script_commands` runs, and its dirty flag is cleared at end of
    // tick - so the other host's mirror must be refreshed here, inside that
    // one-tick window, or the write is invisible to it forever.
    if multi_host {
        app.add_systems(
            TickStage::Systems,
            lumen_script::sync_signals_into_host::<H>
                .in_set(ScriptSet::SyncSignalsLate)
                .after(apply_script_commands)
                .before(ScriptSet::Derivations),
        );
    }
    // Post-mount lifecycle: dispatch `on_ready` once per host, after the first
    // `build_dom_index` publish so a DOM query inside it sees the mounted
    // static tree, and before `collect_dom_commands` so any tree the handler
    // builds is materialized on the same first tick. A missing `on_ready` is a
    // no-op, so `on_start`-only apps are unaffected.
    //
    // `.after(ScriptSet::SyncSignals)`: both write the host's signal mirror on
    // the tick where a value is still dirty, and the sync rewrites entries from
    // the store. Unordered, the sync can run after the dispatch and overwrite
    // the values `on_ready` just wrote with the pre-dispatch store state,
    // leaving the mirror stale for every later handler read.
    app.add_systems(
        TickStage::Systems,
        fire_on_ready::<H>
            .in_set(ScriptSet::Ready)
            .after(build_dom_index)
            .after(crate::run::dom_commands::publish_node_details)
            .after(ScriptSet::SyncSignals),
    );
    register_dom_event_dispatchers::<H>(app);
    // `fire_audio_ended` invokes the optional `on_audio_end()` after the script
    // tick; its emitted commands are drained by the appliers ordered
    // `.after(ScriptSet::AudioEnded)`.
    #[cfg(feature = "audio")]
    app.add_systems(
        TickStage::Systems,
        fire_audio_ended::<H>
            .in_set(ScriptSet::AudioEnded)
            .after(ScriptSet::Tick),
    );
}
