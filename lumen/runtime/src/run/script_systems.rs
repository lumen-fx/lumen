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

/// Inert placeholder for [`audio::fire_audio_ended`] in a build compiled
/// without the `audio` feature (Part B tree-shaking). The real system lives in
/// the `audio` module (compiled only with the feature); this stub keeps the
/// `fire_audio_ended::<H>` type path resolvable so the `.after(..)` ordering
/// edge on `apply_script_commands` stays valid; it references an unregistered
/// system set and is therefore a no-op. Never added to any schedule.
#[cfg(not(feature = "audio"))]
pub(crate) fn fire_audio_ended<H: lumen_script::ScriptHost + Resource<Mutability = Mutable>>(
    _out: MessageWriter<ScriptCommandEvent>,
) {
}

/// Install every host-generic script system and its (RC-critical)
/// ordering edge against the concrete [`ScriptHost`] `H` the
/// `[script] engine` key selected. Monomorphised twice - once per host -
/// from the two match arms in [`build_app`]; every `.after(..)` /
/// `.before(..)` edge that anchors `apply_derivations::<H>` /
/// `tick_script::<H>` / `dispatch_clicks_and_doubles::<H>` must name the
/// SAME `H` that the installed host plugin registered, or the dirty-gating
/// order collapses (the anchor set is empty for the wrong host).
///
/// The binding / push systems (BLOCK A) are unconditional: they run even
/// with no script installed, in which case their host-anchor edges are
/// inert (the referenced `::<H>` systems have zero registrations). The
/// audio + `apply_script_commands` systems (`has_script`) only exist when
/// the app actually ships a script.
pub(crate) fn register_script_systems<
    H: lumen_script::ScriptHost + Resource<Mutability = Mutable>,
>(
    app: &mut App,
    has_script: bool,
    hot_reload_enabled: bool,
) {
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
    // `.before(apply_derivations::<H>)`: derivations consult the store's dirty
    // queue, and `clear_property_store_dirty` (A11ySync) empties it every
    // tick - so a typed dep write committed here must land BEFORE the
    // derivation pass or its one-tick dirty window closes unobserved and
    // the derived signal freezes (RC1). Edge is inert when no script is
    // installed (`apply_derivations` unregistered => empty set), same as
    // the `dispatch_clicks_and_doubles` reference above.
    // Dynamic DOM read side: rebuild + publish the query snapshot before
    // any handler runs, so `query()` / traversal from an `on_click` body
    // sees this tick's tree. Unconditional (runs with or without a script,
    // so the C-ABI / SDK can query a script-less app).
    app.add_systems(
        TickStage::Systems,
        build_dom_index
            .before(dispatch_clicks_and_doubles::<H>)
            .before(lumen_input::dispatch_focused_keys),
    );
    // Publish per-node detail (text / generic attrs / inline style) + the
    // cascade inputs `computed_style` consumes, alongside the DomIndex so a
    // read from a handler sees this tick's tree. Unconditional (works for a
    // script-less app queried over the C-ABI).
    app.add_systems(
        TickStage::Systems,
        crate::run::dom_commands::publish_node_details
            .before(dispatch_clicks_and_doubles::<H>)
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
            .before(dispatch_clicks_and_doubles::<H>)
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
    }
    app.world
        .insert_resource(crate::run::dom_commands::PendingDomCommands::default());
    app.add_systems(
        TickStage::Systems,
        crate::run::dom_commands::collect_dom_commands
            .after(tick_script::<H>)
            .after(dispatch_clicks_and_doubles::<H>)
            .after(dispatch_close_to_script::<H>),
    );
    app.add_systems(
        TickStage::Systems,
        crate::run::dom_commands::apply_dom_commands
            .after(crate::run::dom_commands::collect_dom_commands),
    );
    // Post-mount lifecycle: dispatch `on_ready` once, after the first
    // `build_dom_index` publish so a DOM query inside it sees the mounted
    // static tree, and before `collect_dom_commands` so any tree the handler
    // builds is materialized on the same first tick. Script-gated: only an app
    // with a host has the `on_ready` seam (and the `OnReadyFired` latch the
    // ScriptPlugin installs). A missing `on_ready` is a no-op, so `on_start`-
    // only apps are unaffected.
    if has_script {
        app.add_systems(
            TickStage::Systems,
            fire_on_ready::<H>
                .after(build_dom_index)
                .after(crate::run::dom_commands::publish_node_details)
                .before(crate::run::dom_commands::collect_dom_commands),
        );
    }
    // Dynamic DOM events (phase 4): turn input messages into DOM events and
    // run capture -> target -> bubble propagation over the binding registry.
    // Ordered like the legacy `on_click` dispatch - after the input
    // producers and the snapshot build, before `collect_dom_commands` so a
    // handler's queued DOM mutations apply this same tick. `.before(
    // navigate_on_anchor_click)` lets a `prevent_default` on a link click be
    // observed before the anchor-navigation executor runs. Unconditional so
    // a script-less app still delivers to C-ABI / SDK native handlers.
    app.add_systems(
        TickStage::Systems,
        lumen_script::dispatch_pointer_and_key_events::<H>
            .after(build_dom_index)
            .after(lumen_input::dispatch_clicks)
            .after(lumen_input::dispatch_focused_keys)
            .before(crate::run::dom_commands::collect_dom_commands)
            .before(crate::pages::navigate_on_anchor_click),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_script::dispatch_state_events::<H>
            .after(build_dom_index)
            .before(crate::run::dom_commands::collect_dom_commands),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::property_store::commit_external_properties
            .after(dispatch_clicks_and_doubles::<H>)
            .before(apply_derivations::<H>),
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
            .after(apply_derivations::<H>)
            .after(lumen_core::signals::push_toggle_to_signal)
            .after(lumen_core::signals::push_slider_to_signal)
            .after(lumen_core::signals::push_textinput_to_signal)
            .before(lumen_input::type_into_focused),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::apply_checked_bindings
            .after(lumen_core::property_store::commit_external_properties)
            .after(apply_derivations::<H>)
            .after(lumen_core::signals::push_toggle_to_signal)
            .after(lumen_core::signals::push_slider_to_signal)
            .after(lumen_core::signals::push_textinput_to_signal),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::apply_value_bindings
            .after(lumen_core::property_store::commit_external_properties)
            .after(apply_derivations::<H>)
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
            .after(apply_derivations::<H>)
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
            .after(apply_derivations::<H>)
            .after(lumen_core::signals::push_toggle_to_signal)
            .after(lumen_core::signals::push_slider_to_signal)
            .after(lumen_core::signals::push_textinput_to_signal),
    );
    // Two-way binding push: when an input mutates its TextContent /
    // Toggleable / SliderValue, mirror back into the signal. The Pull
    // half above is idempotent so they don't fight - the No-op equality
    // checks in each function ensure stable state.
    // `.before(apply_derivations::<H>)`: derivations are dirty-gated too, so a
    // control write pushed after the derivation pass on the write tick
    // would never recompute the derived signal (the garden's
    // `toggle_status` freezing on toggle flips was the live repro).
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::push_textinput_to_signal.before(apply_derivations::<H>),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::push_toggle_to_signal.before(apply_derivations::<H>),
    );
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::push_slider_to_signal.before(apply_derivations::<H>),
    );
    // W6 T6: `bind-scroll` push half - the settled scroll offset mirrors
    // back into the signal (throttled to scroll-settle inside the system,
    // never per-frame). `.before(apply_derivations::<H>)` per the 7bfc0f2 push
    // rules; the `.before(sync_signals_into_host)` edge lives in
    // lumen-script-rhai's registration (expressed as its
    // `.after(push_scroll_to_signal)`).
    app.add_systems(
        TickStage::Systems,
        lumen_core::signals::push_scroll_to_signal.before(apply_derivations::<H>),
    );
    // W5 dialog contract (Qt QDialog): Enter-anywhere activates the default
    // button. Ordered after the focused-key fanout (same-tick keystroke) and
    // before the script click dispatch so the synthesized ClickEvent reaches
    // `on_click` handlers on this very tick.
    app.add_systems(
        TickStage::Systems,
        crate::spawn::activate_dialog_default_on_enter
            .after(lumen_input::dispatch_focused_keys)
            .before(dispatch_clicks_and_doubles::<H>),
    );
    if has_script {
        // `apply_script_commands` is the sole applier of script-produced
        // `SetSignal` / `SetArray` writes into `PropertyStore` /
        // `ArraySignals`. Its ordering is load-bearing post perf dirty-gating:
        //
        //  * `.after(tick_script::<H>)` / `.after(dispatch_clicks_and_doubles::<H>)` -
        //    those systems emit the `ScriptCommandEvent`s this drains (the
        //    `on_start` backlog and click-handler writes respectively).
        //    Running after them applies this tick's writes in-tick instead
        //    of lagging a frame through the message double-buffer.
        //
        //  * `.before(apply_derivations::<H>)` - RC1 fix. Derivations consult the
        //    store's per-tick dirty queue, which `clear_property_store_dirty`
        //    (A11ySync) empties every tick - a signal write is dirty for
        //    exactly one tick. The previous `.after(apply_derivations::<H>)` edge
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
        //  * `.after(dispatch_close_to_script)` - the `on_close` hook runs
        //    on the veto tick that follows an OS close request; when the
        //    close commits, that tick is the app's LAST. Without this edge
        //    the commands `on_close` emits (final signal writes, prints)
        //    would sit in the message buffer for a next tick that never
        //    runs and be silently dropped at exit.
        app.add_systems(
            TickStage::Systems,
            apply_script_commands
                .after(tick_script::<H>)
                .after(dispatch_clicks_and_doubles::<H>)
                .after(dispatch_close_to_script::<H>)
                .before(apply_derivations::<H>)
                .before(lumen_core::signals::apply_text_bindings)
                .before(lumen_core::signals::apply_checked_bindings)
                .before(lumen_core::signals::apply_value_bindings)
                .before(crate::spawn::reconcile_for_blocks)
                // on_audio_end (auto-advance) may emit SetSignal commands.
                .after(fire_audio_ended::<H>),
        );
        // Audio transport wiring. COMPILE-TIME GATE (Part B tree-shaking):
        // only registered when the `audio` feature is compiled in. The
        // `.after(fire_audio_ended::<H>)` ordering edge on `apply_script_commands`
        // above stays valid in a no-audio build because an inert
        // `fire_audio_ended` stub (below) keeps the `::<H>` path resolvable --
        // the edge then references an unregistered system set and is a no-op.
        //
        // `poll_audio` pushes position/duration/playing into the store
        // *before* the host mirror sync so `derive()`s over them recompute
        // this tick (the same store->mirror->derive discipline every other
        // signal follows).
        #[cfg(feature = "audio")]
        {
            app.add_systems(
                TickStage::Systems,
                poll_audio.before(sync_signals_into_host::<H>),
            );
            // `apply_loaded_audio` starts playback once the AssetServer resolves
            // the track bytes; runs after the shared decode drain.
            app.add_systems(
                TickStage::Systems,
                apply_loaded_audio.after(lumen_assets::drain_completed_decodes),
            );
            // `fire_audio_ended` invokes the optional `on_audio_end()` after the
            // script tick; its emitted commands are drained by the two appliers
            // below (both ordered `.after(fire_audio_ended::<H>)`).
            app.add_systems(
                TickStage::Systems,
                fire_audio_ended::<H>.after(tick_script::<H>),
            );
            // `apply_audio_commands` applies transport commands + routes
            // `audio_play` through the AssetServer.
            app.add_systems(
                TickStage::Systems,
                apply_audio_commands
                    .after(tick_script::<H>)
                    .after(dispatch_clicks_and_doubles::<H>)
                    .after(fire_audio_ended::<H>),
            );
        }
        // RC6: a script that failed to load at plugin build leaves
        // `ScriptLoadFailure` behind. Mirror it into the in-app error
        // banner so the failure is visible in the window itself, not
        // only in the stderr banner the plugin printed.
        if let Some(fail) = app.world.get_resource::<lumen_script::ScriptLoadFailure>() {
            let msg = format!("script load failed: {}", fail.0);
            app.world.resource_mut::<ErrorBanner>().0 = Some(msg);
        }
    }
    #[cfg(feature = "runtime-parse")]
    if hot_reload_enabled {
        app.add_systems(TickStage::Systems, hot_reload::<H>);
    }
    #[cfg(not(feature = "runtime-parse"))]
    let _ = hot_reload_enabled;
}
