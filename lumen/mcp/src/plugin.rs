//! `LumenMcpPlugin`: wires snapshot systems into both worlds and launches the
//! TCP server on a dedicated OS thread.

use std::sync::{Arc, RwLock};

use base64::Engine as _;
use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::message::MessageReader;
use bevy_ecs::message::MessageWriter;
use bevy_ecs::prelude::*;
use bevy_ecs::system::NonSend;
use lumen_core::app::{App, Plugin};
use lumen_core::components::{
    BindText, Fill, LumenClasses, LumenId, LumenTag, Opacity, SliderValue, Style, TabIndex,
    TextAlign, TextContent, TextStyle, TextWrap, Toggleable, Transform, Visuals,
};
use lumen_core::input::{
    ClickEvent, FocusTracker, Focused, FocusedKey, Hovered, Key, KeyPressed, KeyReleased,
    Modifiers, ModifiersState, MouseWheel, NamedKey, PointerButton, PointerMoved, PointerPressed,
    PointerReleased, PointerState, Pressed, Scroll, ScrollAxis, ScrollOffset,
};
use lumen_core::property_store::{PropertyKey, PropertyStore, PropertyValue};

use crate::simulate::{SimulateKind, SimulateQueue};
use lumen_core::render_world::{
    ExtractedRect, ExtractedText, RenderStage, SurfaceCapture, Viewport,
};
use lumen_core::tick::TickStage;
use lumen_render_headless::HeadlessRenderer;

use lumen_assets::{ImageSource, LoadedImage, LoadedSvg};
use lumen_primitives::Interaction;

use crate::server::{serve_stdio, serve_tcp};
use crate::snapshot::{
    ColorView, EntityFingerprint, EntityInspect, EntityView, ExtractedRectView, ExtractedTextView,
    FillView, FocusOutlineView, FocusView, HISTORY_RING_CAP, HistorySnapshot, InteractionView,
    LoadedImageView, ModifiersView, PointerStateView, RecordedClickEvent, RecordedFocusedKey,
    RecordedKeyPressed, RecordedKeyReleased, RecordedMouseWheel, RecordedPointerMoved,
    RecordedPointerPressed, RecordedPointerReleased, ShadowView, SignalView, SliderValueView,
    Snapshot, SnapshotHandle, StyleView, TextStyleView, TransformView, V2, ViewportView,
    VisualsView,
};

/// Selects between the TCP listener (default, lets the inspector and
/// `lumen-mcp-server` bridge connect) and the canonical MCP stdio
/// transport (preferred for tools that launch lumen as a subprocess
/// and pipe MCP over stdin/stdout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransport {
    /// TCP listener on `127.0.0.1:port`. Default.
    Tcp(u16),
    /// Newline-JSON over stdin/stdout. The plugin's worker reads from
    /// `stdin` and writes responses to `stdout`. Implies the app is
    /// being driven as a subprocess and `stdout` is not used for any
    /// other purpose (logs must go to `stderr`).
    Stdio,
}

impl Default for McpTransport {
    fn default() -> Self {
        Self::Tcp(7878)
    }
}

/// Plugin: spins up an in-app MCP server for runtime introspection.
///
/// Default transport is TCP on `127.0.0.1:7878`. Switch via
/// [`LumenMcpPlugin::with_stdio`] to drive the app from an MCP client
/// over stdio (the canonical MCP transport per the spec).
///
/// Input simulation is off by default; opt in with
/// [`LumenMcpPlugin::with_simulate_enabled`] to let MCP clients (or the
/// `lumenc` CLI) drive pointer/key/scroll events. Without that flag the
/// `lumen.simulate` / `tools/call(name=lumen_simulate)` RPC short-circuits
/// to `enabled=false`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LumenMcpPlugin {
    /// Wire transport.
    pub transport: McpTransport,
    /// Drain the `SimulateQueue` each tick and inject the requested input
    /// events into the main world. Default `false`.
    pub simulate_enabled: bool,
}

impl LumenMcpPlugin {
    /// Override the bind port. Keeps `simulate_enabled = false`.
    pub fn with_port(port: u16) -> Self {
        Self {
            transport: McpTransport::Tcp(port),
            simulate_enabled: false,
        }
    }

    /// Switch to the MCP-over-stdio transport.
    pub fn with_stdio() -> Self {
        Self {
            transport: McpTransport::Stdio,
            simulate_enabled: false,
        }
    }

    /// Returns the configured TCP port if `transport` is TCP.
    pub fn port(&self) -> Option<u16> {
        match self.transport {
            McpTransport::Tcp(p) => Some(p),
            McpTransport::Stdio => None,
        }
    }

    /// Builder-style toggle for input simulation. When `on`, the plugin
    /// drains the `SimulateQueue` each tick.
    pub fn with_simulate_enabled(mut self, on: bool) -> Self {
        self.simulate_enabled = on;
        self
    }
}

/// Throttle controller for the 17 snap_* systems. The default 1 Hz
/// rate keeps a JSON-RPC client's view at most ~1 s stale while
/// dropping the per-tick MCP cost from ~570 us to ~10 us (only the
/// `should_snapshot_tick` gate fires).
///
/// Set the interval to `Duration::ZERO` to force every tick (debug),
/// or to a longer Duration for ultra-low overhead. The render-side
/// `write_render_snapshot` honours the same gate.
#[derive(Resource, Debug)]
pub struct McpSnapshotSchedule {
    /// Wall-clock moment of the last sample.
    pub last_at: std::time::Instant,
    /// Minimum interval between samples. `ZERO` = every tick.
    pub interval: std::time::Duration,
    /// Set true by `should_snapshot_tick` when a fresh sample is due
    /// this tick. The snap_* systems run only when this is true.
    pub due_this_tick: bool,
}

impl Default for McpSnapshotSchedule {
    fn default() -> Self {
        Self {
            // Start out due so the very first frame samples (so clients
            // connecting before the first tick interval elapses get
            // immediate data).
            last_at: std::time::Instant::now() - std::time::Duration::from_secs(86_400),
            interval: std::time::Duration::from_secs(1),
            due_this_tick: false,
        }
    }
}

/// First system in the MCP snapshot pipeline. Flips the
/// `due_this_tick` flag on [`McpSnapshotSchedule`] when the interval
/// has elapsed; every downstream snap_* system gates on the flag.
fn should_snapshot_tick(mut sched: ResMut<McpSnapshotSchedule>) {
    let now = std::time::Instant::now();
    sched.due_this_tick =
        sched.interval.is_zero() || now.duration_since(sched.last_at) >= sched.interval;
    if sched.due_this_tick {
        sched.last_at = now;
    }
}

/// Run-condition shared by every snap_* system. Cheap - one
/// `Res<McpSnapshotSchedule>` lookup + bool read.
fn snapshot_due(sched: Res<McpSnapshotSchedule>) -> bool {
    sched.due_this_tick
}

/// Render-world: advance the schedule each render tick.
fn tick_render_snapshot_schedule(mut sched: ResMut<McpSnapshotSchedule>) {
    let now = std::time::Instant::now();
    let due = sched.interval.is_zero() || now.duration_since(sched.last_at) >= sched.interval;
    sched.due_this_tick = due;
    if due {
        sched.last_at = now;
    }
}

/// Render-world run condition. Read-only so `run_if` accepts it.
fn render_snapshot_due(sched: Res<McpSnapshotSchedule>) -> bool {
    sched.due_this_tick
}

/// Wire [`lumen_core::app::EventLoopWaker`] into the [`SimulateQueue`] once
/// the resource shows up. The two are inserted by different owners on
/// different schedules - `LumenMcpPlugin::build` creates the queue at
/// plugin-build time, but the waker only exists once
/// `lumen_window_winit::run` constructs its `EventLoopProxy`, which
/// happens after the `App` (and thus every plugin) is already built. This
/// system closes that ordering gap: it runs every tick, but
/// `SimulateQueue::set_waker` is a `OnceLock` write, so every call after
/// the first is a no-op read. Headless/test apps that never insert
/// `EventLoopWaker` just keep hitting the `None` arm forever - the queue
/// still drains via the normal tick loop, it just can't wake a parked one
/// that doesn't exist.
fn wire_simulate_waker(
    queue: Res<SimulateQueue>,
    waker: Option<Res<lumen_core::app::EventLoopWaker>>,
) {
    if let Some(waker) = waker {
        queue.set_waker(waker.clone());
    }
}

/// Per-tick progress of the simulate pipeline: the sequence number of the
/// request injected THIS tick (0 = none). Written by
/// [`drain_simulate_queue`] in `TickStage::Input`, published to the
/// cross-thread [`SimulateQueue::completed_seq`] by
/// [`publish_simulate_completion`] in `TickStage::A11ySync` - i.e. only
/// once the request's tick has actually run (W6 T4).
#[derive(Resource, Default, Debug)]
struct SimulateProgress {
    drained_seq: u64,
}

/// Pop exactly ONE pending [`SimulateRequest`] (FIFO) and convert it into
/// the same `MessageWriter<...>` events the winit backend would emit. Runs
/// in `TickStage::Input` BEFORE the real input dispatch so the events are
/// visible to hit-test, focus-routing, and the click router on the same
/// tick.
///
/// One-request-per-tick (W6 T4): rapid-fire requests - e.g. a click
/// followed immediately by an Escape - previously drained into a single
/// tick, letting the Escape act BEFORE the click's systems (popup spawn,
/// focus move) had run. Now each request gets its own full tick, in push
/// order. If more requests remain queued, the loop is re-woken so the
/// next tick follows immediately instead of stalling in a parked event
/// loop.
#[allow(clippy::too_many_arguments)]
fn drain_simulate_queue(
    queue: Res<SimulateQueue>,
    mut progress: ResMut<SimulateProgress>,
    mut pointer: ResMut<PointerState>,
    mut moved: MessageWriter<PointerMoved>,
    mut pressed: MessageWriter<PointerPressed>,
    mut released: MessageWriter<PointerReleased>,
    mut wheel: MessageWriter<MouseWheel>,
    mut key_pressed: MessageWriter<KeyPressed>,
    mut key_released: MessageWriter<KeyReleased>,
) {
    let (popped, remaining) = queue.pop_front();
    let Some((seq, req)) = popped else {
        return;
    };
    progress.drained_seq = seq;
    if remaining {
        // Schedule the follow-up tick for the next queued request now -
        // the push-time wake was already consumed by this tick.
        queue.wake();
    }
    {
        match req.kind {
            SimulateKind::PointerMove { x, y } => {
                // Mirror the winit backend's `CursorMoved` handling: update
                // `PointerState.position` as well as writing the message, so
                // `hit_test` (which reads the resource, not the message ring)
                // sees the synthetic pointer and updates `Hovered`. Without
                // this, a simulated move/click is invisible to hit-testing and
                // input routes to wherever the real OS cursor last hovered.
                pointer.position = Some(glam::Vec2::new(x, y));
                moved.write(PointerMoved {
                    position: glam::Vec2::new(x, y),
                });
            }
            SimulateKind::Click { x, y, button } => {
                let b: PointerButton = button.as_deref().unwrap_or("primary").into();
                // Move the synthetic pointer first so this tick's `hit_test`
                // resolves `Hovered` to the entity under (x, y) before
                // `dispatch_clicks` turns the press/release into a click.
                pointer.position = Some(glam::Vec2::new(x, y));
                if matches!(b, PointerButton::Primary) {
                    pointer.primary_down = true;
                }
                moved.write(PointerMoved {
                    position: glam::Vec2::new(x, y),
                });
                pressed.write(PointerPressed {
                    position: glam::Vec2::new(x, y),
                    button: b,
                });
                released.write(PointerReleased {
                    position: glam::Vec2::new(x, y),
                    button: b,
                });
                if matches!(b, PointerButton::Primary) {
                    pointer.primary_down = false;
                }
            }
            SimulateKind::PointerDown { x, y, button } => {
                let b: PointerButton = button.as_deref().unwrap_or("primary").into();
                pointer.position = Some(glam::Vec2::new(x, y));
                if matches!(b, PointerButton::Primary) {
                    pointer.primary_down = true;
                }
                moved.write(PointerMoved {
                    position: glam::Vec2::new(x, y),
                });
                pressed.write(PointerPressed {
                    position: glam::Vec2::new(x, y),
                    button: b,
                });
            }
            SimulateKind::PointerUp { x, y, button } => {
                let b: PointerButton = button.as_deref().unwrap_or("primary").into();
                pointer.position = Some(glam::Vec2::new(x, y));
                released.write(PointerReleased {
                    position: glam::Vec2::new(x, y),
                    button: b,
                });
                if matches!(b, PointerButton::Primary) {
                    pointer.primary_down = false;
                }
            }
            SimulateKind::Key { key, modifiers } => {
                let k: Key = key.as_str().into();
                let m: Modifiers = modifiers.into();
                key_pressed.write(KeyPressed {
                    key: k.clone(),
                    modifiers: m,
                    repeat: false,
                });
                key_released.write(KeyReleased {
                    key: k,
                    modifiers: m,
                });
            }
            SimulateKind::Type { text } => {
                let m = Modifiers::default();
                for ch in text.chars() {
                    let k = Key::Character(ch.to_string());
                    key_pressed.write(KeyPressed {
                        key: k.clone(),
                        modifiers: m,
                        repeat: false,
                    });
                    key_released.write(KeyReleased {
                        key: k,
                        modifiers: m,
                    });
                }
            }
            SimulateKind::Scroll { x, y, dx, dy } => {
                wheel.write(MouseWheel {
                    delta: glam::Vec2::new(dx, dy),
                    position: glam::Vec2::new(x, y),
                });
            }
        }
    }
}

/// End-of-tick half of the W6 T4 contract: publish the sequence number of
/// the request injected this tick to the cross-thread
/// [`SimulateQueue::completed_seq`]. Runs in `TickStage::A11ySync`, so by
/// the time the TCP handler observes `completed >= seq` the request's
/// Input / CommandDrain / Systems / LayoutSync stages have ALL run - the
/// RPC response can no longer race the tick.
fn publish_simulate_completion(queue: Res<SimulateQueue>, progress: Res<SimulateProgress>) {
    if progress.drained_seq > 0 {
        queue.publish_completed(progress.drained_seq);
    }
}

impl Plugin for LumenMcpPlugin {
    fn build(self, app: &mut App) {
        // Bump the worker-thread budget by one so the 17 snap_*
        // systems can fan out across the executor's pool. Without
        // this lumen-core's 4-thread default starves them.
        app.request_threads_at_least(lumen_core::app::LUMEN_DEFAULT_THREADS + 1);

        app.world.init_resource::<McpSnapshotSchedule>();
        app.render_world.init_resource::<McpSnapshotSchedule>();

        let handle = SnapshotHandle(Arc::new(RwLock::new(Snapshot::default())));
        app.world.insert_resource(handle.clone());
        app.render_world.insert_resource(handle.clone());

        // SurfaceCapture coordinates on-screen screenshots between the MCP
        // handler thread (sets the flag, reads the result) and the on-screen
        // renderer in the render world (fills the framebuffer). Inserted
        // into both worlds so any backend can find it; the server thread
        // gets its own clone.
        let surface_capture = SurfaceCapture::default();
        app.world.insert_resource(surface_capture.clone());
        app.render_world.insert_resource(surface_capture.clone());

        // Simulate queue. Always inserted; only drained when
        // `simulate_enabled` is set (gates input-injection).
        let simulate_queue = SimulateQueue::default();
        app.world.insert_resource(simulate_queue.clone());
        // Waker wiring is unconditional (it used to gate on
        // `simulate_enabled`): `lumen.set_signal` reuses the queue's waker
        // to nudge a parked event loop after pushing onto the external
        // property bus, and that method is always available. The wire
        // system is a per-tick `OnceLock` read after the first hit - free.
        app.add_systems(TickStage::Input, wire_simulate_waker);
        if self.simulate_enabled {
            app.world.init_resource::<SimulateProgress>();
            app.add_systems(
                TickStage::Input,
                drain_simulate_queue.after(wire_simulate_waker),
            );
            // W6 T4: tick-completion publisher - the last stage of the
            // main schedule, so a handler observing the seq knows the
            // request's whole tick ran.
            app.add_systems(TickStage::A11ySync, publish_simulate_completion);
        }

        // Snapshot writer pipeline. Ordered: bump frame + resources first,
        // then entity/component sweeps (each adds to `inspect`), then the
        // entities-list finalizer.
        app.add_systems(TickStage::A11ySync, should_snapshot_tick);
        app.add_systems(
            TickStage::A11ySync,
            (
                snap_frame_and_resources.after(should_snapshot_tick),
                snap_transforms.after(snap_frame_and_resources),
                snap_styles.after(snap_frame_and_resources),
                snap_visuals_component.after(snap_frame_and_resources),
                snap_texts.after(snap_frame_and_resources),
                snap_text_styles.after(snap_frame_and_resources),
                snap_markers.after(snap_frame_and_resources),
                snap_tab_indices.after(snap_frame_and_resources),
                snap_scrolls.after(snap_frame_and_resources),
                snap_opacities.after(snap_frame_and_resources),
                snap_controls.after(snap_frame_and_resources),
                snap_images.after(snap_frame_and_resources),
                snap_state_tints.after(snap_frame_and_resources),
                snap_bindings.after(snap_frame_and_resources),
                snap_hierarchy.after(snap_frame_and_resources),
                snap_identity.after(snap_frame_and_resources),
                snap_signals.after(snap_frame_and_resources),
                snap_compute_fingerprints
                    .after(snap_identity)
                    .after(snap_transforms)
                    .after(snap_styles)
                    .after(snap_visuals_component)
                    .after(snap_texts)
                    .after(snap_text_styles)
                    .after(snap_markers)
                    .after(snap_tab_indices)
                    .after(snap_scrolls)
                    .after(snap_opacities)
                    .after(snap_controls)
                    .after(snap_images)
                    .after(snap_state_tints)
                    .after(snap_bindings)
                    .after(snap_hierarchy),
                snap_finalize_entities
                    .after(snap_compute_fingerprints)
                    .after(snap_identity)
                    .after(snap_transforms)
                    .after(snap_styles)
                    .after(snap_visuals_component)
                    .after(snap_texts)
                    .after(snap_text_styles)
                    .after(snap_markers)
                    .after(snap_tab_indices)
                    .after(snap_scrolls)
                    .after(snap_opacities)
                    .after(snap_controls)
                    .after(snap_images)
                    .after(snap_state_tints)
                    .after(snap_bindings)
                    .after(snap_hierarchy),
            )
                .run_if(snapshot_due),
        );

        // W6 T5: real tick timing - main-world half every tick, render
        // half on rendered ticks (see the two fn docs).
        app.add_systems(TickStage::A11ySync, record_main_tick_timing);
        app.add_render_systems(
            RenderStage::Render,
            finish_tick_timing_render.after(write_render_snapshot),
        );

        // Message rings.
        app.add_systems(
            TickStage::A11ySync,
            (
                record_pointer_moved,
                record_pointer_pressed,
                record_pointer_released,
                record_click_event,
                record_key_pressed,
                record_key_released,
                record_mouse_wheel,
                record_focused_key,
            ),
        );

        // Render-world snapshot system: writes rects/texts and (when present)
        // the headless screenshot. Same throttle - the render world has its
        // own `McpSnapshotSchedule` instance, flipped each tick by the
        // main-world `should_snapshot_tick` via the shared `SnapshotHandle`
        // semantics. For now we read the render-world copy independently;
        // it stays in lockstep because both default to 1 Hz.
        app.add_render_systems(RenderStage::Prepare, tick_render_snapshot_schedule);
        app.add_render_systems(
            RenderStage::Render,
            write_render_snapshot.run_if(render_snapshot_due),
        );

        // Spawn the server on a dedicated thread. TCP and stdio share
        // the same dispatch - the transport just picks the byte path.
        let transport = self.transport;
        let server_handle = handle.0.clone();
        let server_surface = surface_capture;
        let server_simulate = simulate_queue;
        let simulate_enabled = self.simulate_enabled;
        std::thread::Builder::new()
            .name("lumen-mcp-server".into())
            .spawn(move || match transport {
                McpTransport::Tcp(port) => serve_tcp(
                    port,
                    server_handle,
                    Some(server_surface),
                    server_simulate,
                    simulate_enabled,
                ),
                McpTransport::Stdio => serve_stdio(
                    server_handle,
                    Some(server_surface),
                    server_simulate,
                    simulate_enabled,
                ),
            })
            .expect("lumen-mcp: failed to spawn server thread");
    }
}

/// W6 T5: main-world half of the real tick timing. Runs unthrottled in
/// `TickStage::A11ySync` (the last main stage): stamps the tick's start
/// instant into the snapshot (for the render-world half) and records the
/// main-schedule span as `last_tick_micros`. On ticks that render,
/// [`finish_tick_timing_render`] overwrites the value with the fuller
/// main + extract + encode span.
fn record_main_tick_timing(handle: Res<SnapshotHandle>, tick: Res<lumen_core::tick::Tick>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    snap.tick_started_at = Some(tick.now);
    snap.last_tick_micros = tick.now.elapsed().as_micros() as u64;
}

/// W6 T5: render-world half - extends the measurement across extract +
/// scene encode. Runs unthrottled at the end of `RenderStage::Render`,
/// reading the start instant bridged via the shared snapshot.
fn finish_tick_timing_render(handle: Res<SnapshotHandle>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    if let Some(start) = snap.tick_started_at {
        snap.last_tick_micros = start.elapsed().as_micros() as u64;
    }
}

fn snap_frame_and_resources(
    handle: Res<SnapshotHandle>,
    viewport: Res<Viewport>,
    pointer: Res<PointerState>,
    modifiers: Res<ModifiersState>,
    focus: Res<FocusTracker>,
) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    snap.frame = snap.frame.wrapping_add(1);
    snap.viewport = ViewportView {
        size: V2::from(viewport.size),
        clear: ColorView::from(viewport.clear),
    };
    snap.pointer = PointerStateView {
        position: pointer.position.map(V2::from),
        primary_down: pointer.primary_down,
    };
    snap.modifiers = modifiers.0.into();
    snap.focus = FocusView {
        entity: focus.0.map(|e| e.to_bits()),
    };
    // Reset inspect each tick so removed components disappear.
    snap.inspect.clear();
}

fn get_or_init(
    inspect: &mut std::collections::HashMap<u64, EntityInspect>,
    e: Entity,
) -> &mut EntityInspect {
    let id = e.to_bits();
    inspect.entry(id).or_insert_with(|| EntityInspect {
        id,
        ..Default::default()
    })
}

fn snap_transforms(handle: Res<SnapshotHandle>, q: Query<(Entity, &Transform)>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, t) in &q {
        let v = get_or_init(&mut snap.inspect, e);
        v.transform = Some(TransformView {
            absolute: V2::from(t.absolute),
            size: V2::from(t.size),
        });
    }
}

fn snap_styles(handle: Res<SnapshotHandle>, q: Query<(Entity, &Style)>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, s) in &q {
        let v = get_or_init(&mut snap.inspect, e);
        v.style = Some(s.into());
    }
}

fn snap_visuals_component(handle: Res<SnapshotHandle>, q: Query<(Entity, &Visuals)>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, vis) in &q {
        let entry = get_or_init(&mut snap.inspect, e);
        entry.visuals = Some(VisualsView {
            fill: vis.fill.as_ref().map(|f| match f {
                Fill::Solid(c) => FillView::Solid {
                    color: ColorView::from(*c),
                },
                Fill::Linear { angle_deg, stops } => FillView::Linear {
                    angle_deg: *angle_deg,
                    stops: stops
                        .iter()
                        .map(|(o, c)| (*o, ColorView::from(*c)))
                        .collect(),
                },
                Fill::Radial { radius, stops } => FillView::Radial {
                    radius: *radius,
                    stops: stops
                        .iter()
                        .map(|(o, c)| (*o, ColorView::from(*c)))
                        .collect(),
                },
                Fill::Conic { from_deg, stops } => FillView::Conic {
                    from_deg: *from_deg,
                    stops: stops
                        .iter()
                        .map(|(o, c)| (*o, ColorView::from(*c)))
                        .collect(),
                },
            }),
            radius: vis.radius,
            shadows: vis
                .shadows
                .iter()
                .map(|s| ShadowView {
                    offset_x: s.offset_x,
                    offset_y: s.offset_y,
                    blur: s.blur,
                    color: ColorView::from(s.color),
                    inner: s.inner,
                })
                .collect(),
        });
    }
}

fn snap_text_styles(handle: Res<SnapshotHandle>, q: Query<(Entity, &TextStyle)>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, ts) in &q {
        get_or_init(&mut snap.inspect, e).text_style = Some(TextStyleView {
            color: ColorView::from(ts.color),
            size_px: ts.size_px,
            align: match ts.align {
                TextAlign::Start => "start",
                TextAlign::Center => "center",
                TextAlign::End => "end",
            },
            wrap: match ts.wrap {
                TextWrap::None => "none",
                TextWrap::Word => "word",
                TextWrap::Glyph => "glyph",
            },
            max_lines: ts.max_lines,
        });
    }
}

fn snap_texts(handle: Res<SnapshotHandle>, q: Query<(Entity, &TextContent)>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, tc) in &q {
        let v = get_or_init(&mut snap.inspect, e);
        v.text_content = Some(tc.0.clone());
    }
}

fn snap_markers(
    handle: Res<SnapshotHandle>,
    hovered: Query<Entity, With<Hovered>>,
    focused: Query<Entity, With<Focused>>,
    pressed: Query<Entity, With<Pressed>>,
) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for e in &hovered {
        get_or_init(&mut snap.inspect, e).hovered = true;
    }
    for e in &focused {
        get_or_init(&mut snap.inspect, e).focused = true;
    }
    for e in &pressed {
        get_or_init(&mut snap.inspect, e).pressed = true;
    }
}

fn snap_tab_indices(handle: Res<SnapshotHandle>, q: Query<(Entity, &TabIndex)>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, tab) in &q {
        get_or_init(&mut snap.inspect, e).tab_index = Some(tab.0);
    }
}

fn snap_scrolls(
    handle: Res<SnapshotHandle>,
    scrolls: Query<(Entity, &Scroll)>,
    scroll_offsets: Query<(Entity, &ScrollOffset)>,
) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, sc) in &scrolls {
        get_or_init(&mut snap.inspect, e).scroll = Some(match sc.axis {
            ScrollAxis::X => "x",
            ScrollAxis::Y => "y",
            ScrollAxis::Both => "both",
        });
    }
    for (e, off) in &scroll_offsets {
        get_or_init(&mut snap.inspect, e).scroll_offset = Some(V2::from(off.0));
    }
}

fn snap_opacities(handle: Res<SnapshotHandle>, q: Query<(Entity, &Opacity)>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, o) in &q {
        get_or_init(&mut snap.inspect, e).opacity = Some(o.0);
    }
}

fn snap_controls(
    handle: Res<SnapshotHandle>,
    toggles: Query<(Entity, &Toggleable)>,
    sliders: Query<(Entity, &SliderValue)>,
) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, t) in &toggles {
        get_or_init(&mut snap.inspect, e).toggleable = Some(t.checked);
    }
    for (e, sv) in &sliders {
        get_or_init(&mut snap.inspect, e).slider_value = Some(SliderValueView {
            value: sv.value,
            min: sv.min,
            max: sv.max,
        });
    }
}

fn snap_images(
    handle: Res<SnapshotHandle>,
    sources: Query<(Entity, &ImageSource)>,
    loaded: Query<(Entity, &LoadedImage)>,
    svgs: Query<(Entity, &LoadedSvg)>,
) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, s) in &sources {
        get_or_init(&mut snap.inspect, e).image_source = Some(s.0.display().to_string());
    }
    for (e, li) in &loaded {
        get_or_init(&mut snap.inspect, e).loaded_image = Some(LoadedImageView {
            width: li.width,
            height: li.height,
        });
    }
    for (e, sv) in &svgs {
        get_or_init(&mut snap.inspect, e).loaded_svg = Some(V2 {
            x: sv.intrinsic.x,
            y: sv.intrinsic.y,
        });
    }
}

fn snap_state_tints(handle: Res<SnapshotHandle>, interactions: Query<(Entity, &Interaction)>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, ix) in &interactions {
        let entry = get_or_init(&mut snap.inspect, e);
        entry.interaction = Some(InteractionView {
            hover_tint: ix.hover_tint.map(ColorView::from),
            press_tint: ix.press_tint.map(ColorView::from),
            focus_outline: ix.focus_outline.map(|o| FocusOutlineView {
                width: o.width,
                color: ColorView::from(o.color),
            }),
        });
    }
}

/// Compute per-entity fingerprints from the just-populated `snap.inspect`
/// map, push the PREVIOUS fingerprints into the history ring, and swap the
/// new ones in. This runs before `snap_finalize_entities` so the history
/// reflects only the snap pipeline's view of an entity (not partial state).
fn snap_compute_fingerprints(handle: Res<SnapshotHandle>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    let mut new_fingerprints: std::collections::HashMap<u64, EntityFingerprint> =
        std::collections::HashMap::with_capacity(snap.inspect.len());
    for inv in snap.inspect.values() {
        new_fingerprints.insert(inv.id, fingerprint_of(inv));
    }
    // Push previous fingerprints into history before replacing.
    if !snap.fingerprints.is_empty() {
        let prev_frame = snap.frame.wrapping_sub(1);
        let prev = std::mem::take(&mut snap.fingerprints);
        snap.history.push_back(HistorySnapshot {
            frame: prev_frame,
            fingerprints: prev,
        });
        while snap.history.len() > HISTORY_RING_CAP {
            snap.history.pop_front();
        }
    }
    snap.fingerprints = new_fingerprints;
}

fn fingerprint_of(inv: &EntityInspect) -> EntityFingerprint {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(t) = inv.transform {
        t.absolute.x.to_bits().hash(&mut h);
        t.absolute.y.to_bits().hash(&mut h);
        t.size.x.to_bits().hash(&mut h);
        t.size.y.to_bits().hash(&mut h);
    }
    if let Some(s) = inv.style.as_ref() {
        s.flex_direction.hash(&mut h);
        s.width.hash(&mut h);
        s.width_value.to_bits().hash(&mut h);
        s.height.hash(&mut h);
        s.height_value.to_bits().hash(&mut h);
    }
    if let Some(text) = inv.text_content.as_deref() {
        text.hash(&mut h);
    }
    if let Some(bt) = inv.bind_text.as_deref() {
        bt.hash(&mut h);
    }
    inv.hovered.hash(&mut h);
    inv.focused.hash(&mut h);
    inv.pressed.hash(&mut h);
    if let Some(tab) = inv.tab_index {
        tab.hash(&mut h);
    }
    if let Some(o) = inv.opacity {
        o.to_bits().hash(&mut h);
    }
    if let Some(t) = inv.toggleable {
        t.hash(&mut h);
    }
    inv.parent.hash(&mut h);
    inv.children.hash(&mut h);
    EntityFingerprint(h.finish())
}

fn snap_hierarchy(
    handle: Res<SnapshotHandle>,
    parents: Query<(Entity, &ChildOf)>,
    children: Query<(Entity, &Children)>,
) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, parent) in &parents {
        get_or_init(&mut snap.inspect, e).parent = Some(parent.parent().to_bits());
    }
    for (e, kids) in &children {
        let ids: Vec<u64> = kids.iter().map(|c| c.to_bits()).collect();
        if !ids.is_empty() {
            get_or_init(&mut snap.inspect, e).children = ids;
        }
    }
}

fn snap_bindings(handle: Res<SnapshotHandle>, q: Query<(Entity, &BindText)>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, b) in &q {
        get_or_init(&mut snap.inspect, e).bind_text = Some(b.0.to_string());
    }
}

/// Markup identity sweep: tag name (`LumenTag`), stable string id
/// (`LumenId`), and class list (`LumenClasses`). These power the
/// inspector's element-tree labels and the `lumen.snapshot_tree` node
/// shape (`tag` / `lumen_id` / `classes`).
fn snap_identity(
    handle: Res<SnapshotHandle>,
    tags: Query<(Entity, &LumenTag)>,
    ids: Query<(Entity, &LumenId)>,
    classes: Query<(Entity, &LumenClasses)>,
) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    for (e, tag) in &tags {
        get_or_init(&mut snap.inspect, e).tag = Some(tag.0.to_string());
    }
    for (e, id) in &ids {
        get_or_init(&mut snap.inspect, e).lumen_id = Some(id.0.clone());
    }
    for (e, cls) in &classes {
        if !cls.0.is_empty() {
            get_or_init(&mut snap.inspect, e).classes =
                cls.0.iter().map(|c| c.to_string()).collect();
        }
    }
}

/// Stringify one [`PropertyValue`] for the signals panel. Matches the
/// coercions `Signals::get` readers observe for scalars; non-scalar
/// variants render a debug-ish shape instead of the legacy empty string
/// so the panel stays informative.
fn signal_value_string(v: &PropertyValue) -> String {
    match v {
        PropertyValue::Str(s) => s.to_string(),
        PropertyValue::Bool(b) => if *b { "true" } else { "false" }.into(),
        PropertyValue::I64(n) => n.to_string(),
        PropertyValue::F64(n) => n.to_string(),
        PropertyValue::Color(c) => format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, c.a),
        PropertyValue::Vec2(v) => format!("({}, {})", v.x, v.y),
        PropertyValue::Custom(_) => "<custom>".into(),
    }
}

fn signal_kind(v: &PropertyValue) -> &'static str {
    match v {
        PropertyValue::Str(_) => "str",
        PropertyValue::Bool(_) => "bool",
        PropertyValue::I64(_) => "i64",
        PropertyValue::F64(_) => "f64",
        PropertyValue::Color(_) => "color",
        PropertyValue::Vec2(_) => "vec2",
        PropertyValue::Custom(_) => "custom",
    }
}

/// Sample every globally-keyed [`PropertyStore`] cell into
/// [`Snapshot::signals`], tracking the snapshot frame of each cell's last
/// generation bump so `lumen.signals` can report `last_changed_frame`.
/// Cells seen for the first time report `0` (never observed changing).
/// No-op (clears the list) when the app carries no `PropertyStore`.
fn snap_signals(handle: Res<SnapshotHandle>, store: Option<Res<PropertyStore>>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    let Some(store) = store else {
        snap.signals.clear();
        return;
    };
    let frame = snap.frame;
    let mut views: Vec<SignalView> = Vec::with_capacity(store.len());
    for (key, _value) in store.iter() {
        let PropertyKey::Global(name) = key else {
            continue;
        };
        let Some(cell) = store.cell(key) else {
            continue;
        };
        let name = name.to_string();
        let last_changed_frame = match snap.signal_changes.get(&name) {
            Some((known_gen, at_frame)) if *known_gen == cell.generation => *at_frame,
            Some(_) => frame,
            // First observation: report 0 ("not seen changing") rather
            // than pretending the cell changed on connect.
            None => 0,
        };
        snap.signal_changes
            .insert(name.clone(), (cell.generation, last_changed_frame));
        views.push(SignalView {
            name,
            value: signal_value_string(&cell.value),
            kind: signal_kind(&cell.value),
            generation: cell.generation,
            last_changed_frame,
        });
    }
    views.sort_by(|a, b| a.name.cmp(&b.name));
    snap.signals = views;
}

fn snap_finalize_entities(handle: Res<SnapshotHandle>) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };
    let mut entities: Vec<EntityView> = snap
        .inspect
        .iter()
        .map(|(id, inv)| {
            let mut components: Vec<&'static str> = Vec::new();
            if inv.transform.is_some() {
                components.push("lumen_core::components::Transform");
            }
            if inv.style.is_some() {
                components.push("lumen_core::components::Style");
            }
            if inv.visuals.is_some() {
                components.push("lumen_core::components::Visuals");
            }
            if inv.text_content.is_some() {
                components.push("lumen_core::components::TextContent");
            }
            if inv.text_style.is_some() {
                components.push("lumen_core::components::TextStyle");
            }
            if inv.hovered {
                components.push("lumen_core::input::Hovered");
            }
            if inv.focused {
                components.push("lumen_core::input::Focused");
            }
            if inv.pressed {
                components.push("lumen_core::input::Pressed");
            }
            if inv.tab_index.is_some() {
                components.push("lumen_core::components::TabIndex");
            }
            if inv.scroll.is_some() {
                components.push("lumen_core::input::Scroll");
            }
            if inv.scroll_offset.is_some() {
                components.push("lumen_core::input::ScrollOffset");
            }
            if inv.opacity.is_some() {
                components.push("lumen_core::components::Opacity");
            }
            if inv.image_source.is_some() {
                components.push("lumen_assets::ImageSource");
            }
            if inv.loaded_image.is_some() {
                components.push("lumen_assets::LoadedImage");
            }
            if inv.loaded_svg.is_some() {
                components.push("lumen_assets::LoadedSvg");
            }
            if inv.toggleable.is_some() {
                components.push("lumen_core::components::Toggleable");
            }
            if inv.slider_value.is_some() {
                components.push("lumen_core::components::SliderValue");
            }
            if inv.interaction.is_some() {
                components.push("lumen_primitives::Interaction");
            }
            if inv.bind_text.is_some() {
                components.push("lumen_core::components::BindText");
            }
            if inv.tag.is_some() {
                components.push("lumen_core::components::LumenTag");
            }
            if inv.lumen_id.is_some() {
                components.push("lumen_core::components::LumenId");
            }
            if !inv.classes.is_empty() {
                components.push("lumen_core::components::LumenClasses");
            }
            EntityView {
                id: *id,
                components,
            }
        })
        .collect();
    entities.sort_by_key(|e| e.id);
    snap.entities = entities;
}

fn write_render_snapshot(
    handle: Res<SnapshotHandle>,
    rects: Query<&ExtractedRect>,
    texts: Query<&ExtractedText>,
    headless: Option<NonSend<HeadlessRenderer>>,
) {
    let Ok(mut snap) = handle.0.write() else {
        return;
    };

    snap.rects = rects
        .iter()
        .map(|r| {
            // MCP snapshot exposes a single representative color even for
            // gradient-painted rects (first stop) - it's a debugging
            // surface, not a render command. Clients that want the
            // gradient shape walk the live entity tree instead.
            let color = match &r.brush {
                lumen_core::render_world::Brush::Solid(c) => *c,
                lumen_core::render_world::Brush::Linear { stops, .. }
                | lumen_core::render_world::Brush::Radial { stops, .. }
                | lumen_core::render_world::Brush::Conic { stops, .. } => {
                    stops.first().map(|(_, c)| *c).unwrap_or_default()
                }
            };
            ExtractedRectView {
                origin: V2::from(r.origin),
                size: V2::from(r.size),
                fill: ColorView::from(color),
                radius: r.radius,
            }
        })
        .collect();
    snap.texts = texts
        .iter()
        .map(|t| ExtractedTextView {
            origin: V2::from(t.origin),
            text: t.text.clone(),
            size_px: t.size_px,
            fill: ColorView::from(t.fill),
        })
        .collect();

    snap.screenshot_png_base64 = headless.and_then(|h| {
        let (w, hpx) = h.size();
        let fb = h.framebuffer();
        encode_png_base64(w, hpx, fb)
    });
}

fn encode_png_base64(width: u32, height: u32, rgba8: &[u8]) -> Option<String> {
    use image::ImageEncoder;
    use image::codecs::png::PngEncoder;
    let mut out: Vec<u8> = Vec::with_capacity(rgba8.len() / 2);
    let encoder = PngEncoder::new(&mut out);
    if encoder
        .write_image(rgba8, width, height, image::ExtendedColorType::Rgba8)
        .is_err()
    {
        return None;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(&out))
}

impl From<Modifiers> for ModifiersView {
    fn from(m: Modifiers) -> Self {
        ModifiersView {
            shift: m.shift,
            ctrl: m.ctrl,
            alt: m.alt,
            super_: m.super_,
        }
    }
}

impl From<&Style> for StyleView {
    fn from(s: &Style) -> Self {
        fn len(l: &lumen_core::components::Length) -> (&'static str, f32) {
            match l {
                lumen_core::components::Length::Auto => ("auto", 0.0),
                lumen_core::components::Length::Px(v) => ("px", *v),
                lumen_core::components::Length::Percent(v) => ("percent", *v),
            }
        }
        let (wk, wv) = len(&s.width);
        let (hk, hv) = len(&s.height);
        StyleView {
            width: wk,
            width_value: wv,
            height: hk,
            height_value: hv,
            flex_direction: match s.flex_direction {
                lumen_core::components::FlexDirection::Row => "row",
                lumen_core::components::FlexDirection::Column => "column",
                // W5.5: logical-axis reverse variants exposed via the
                // [`FlexDirection::resolved`] resolver - the snapshot
                // surface lists the authored value, not the resolved one.
                lumen_core::components::FlexDirection::RowReverse => "row-reverse",
                lumen_core::components::FlexDirection::ColumnReverse => "column-reverse",
            },
            padding: [
                s.padding.left,
                s.padding.right,
                s.padding.top,
                s.padding.bottom,
            ],
            margin: [s.margin.left, s.margin.right, s.margin.top, s.margin.bottom],
        }
    }
}

fn button_name(b: PointerButton) -> String {
    match b {
        PointerButton::Primary => "primary".into(),
        PointerButton::Secondary => "secondary".into(),
        PointerButton::Middle => "middle".into(),
        PointerButton::Other(n) => format!("other({n})"),
    }
}

fn key_name(k: &Key) -> String {
    match k {
        Key::Named(n) => match n {
            NamedKey::Tab => "Tab".into(),
            NamedKey::Enter => "Enter".into(),
            NamedKey::Escape => "Escape".into(),
            NamedKey::Backspace => "Backspace".into(),
            NamedKey::Space => "Space".into(),
            NamedKey::ArrowUp => "ArrowUp".into(),
            NamedKey::ArrowDown => "ArrowDown".into(),
            NamedKey::ArrowLeft => "ArrowLeft".into(),
            NamedKey::ArrowRight => "ArrowRight".into(),
            NamedKey::Home => "Home".into(),
            NamedKey::End => "End".into(),
            NamedKey::Delete => "Delete".into(),
        },
        Key::Character(s) => s.clone(),
    }
}

fn record_pointer_moved(handle: Res<SnapshotHandle>, mut rdr: MessageReader<PointerMoved>) {
    if let Ok(mut snap) = handle.0.write() {
        for m in rdr.read() {
            snap.pointer_moved.push(RecordedPointerMoved {
                position: V2::from(m.position),
            });
        }
    }
}

fn record_pointer_pressed(handle: Res<SnapshotHandle>, mut rdr: MessageReader<PointerPressed>) {
    if let Ok(mut snap) = handle.0.write() {
        for m in rdr.read() {
            snap.pointer_pressed.push(RecordedPointerPressed {
                position: V2::from(m.position),
                button: button_name(m.button),
            });
        }
    }
}

fn record_pointer_released(handle: Res<SnapshotHandle>, mut rdr: MessageReader<PointerReleased>) {
    if let Ok(mut snap) = handle.0.write() {
        for m in rdr.read() {
            snap.pointer_released.push(RecordedPointerReleased {
                position: V2::from(m.position),
                button: button_name(m.button),
            });
        }
    }
}

fn record_click_event(handle: Res<SnapshotHandle>, mut rdr: MessageReader<ClickEvent>) {
    if let Ok(mut snap) = handle.0.write() {
        for m in rdr.read() {
            snap.click_event.push(RecordedClickEvent {
                entity: m.entity.to_bits(),
                position: V2::from(m.position),
                button: button_name(m.button),
            });
        }
    }
}

fn record_key_pressed(handle: Res<SnapshotHandle>, mut rdr: MessageReader<KeyPressed>) {
    if let Ok(mut snap) = handle.0.write() {
        for m in rdr.read() {
            snap.key_pressed.push(RecordedKeyPressed {
                key: key_name(&m.key),
                modifiers: m.modifiers.into(),
                repeat: m.repeat,
            });
        }
    }
}

fn record_key_released(handle: Res<SnapshotHandle>, mut rdr: MessageReader<KeyReleased>) {
    if let Ok(mut snap) = handle.0.write() {
        for m in rdr.read() {
            snap.key_released.push(RecordedKeyReleased {
                key: key_name(&m.key),
                modifiers: m.modifiers.into(),
            });
        }
    }
}

fn record_mouse_wheel(handle: Res<SnapshotHandle>, mut rdr: MessageReader<MouseWheel>) {
    if let Ok(mut snap) = handle.0.write() {
        for m in rdr.read() {
            snap.mouse_wheel.push(RecordedMouseWheel {
                delta: V2::from(m.delta),
                position: V2::from(m.position),
            });
        }
    }
}

fn record_focused_key(handle: Res<SnapshotHandle>, mut rdr: MessageReader<FocusedKey>) {
    if let Ok(mut snap) = handle.0.write() {
        for m in rdr.read() {
            snap.focused_key.push(RecordedFocusedKey {
                entity: m.entity.to_bits(),
                key: key_name(&m.key),
                modifiers: m.modifiers.into(),
                repeat: m.repeat,
            });
        }
    }
}

#[cfg(test)]
mod signal_snapshot_tests {
    use super::*;
    use lumen_core::app::App;
    use lumen_core::tick::TickStage;

    /// `snap_signals` samples global PropertyStore cells, stringifies the
    /// typed variants, and tracks the snapshot frame of each cell's last
    /// generation bump. First observation reports `0` ("never seen
    /// changing"); a later write reports the frame the change surfaced at.
    #[test]
    fn snap_signals_tracks_last_changed_frame() {
        let mut app = App::new();
        let handle = SnapshotHandle::default();
        handle.0.write().unwrap().frame = 5;
        app.world.insert_resource(handle.clone());
        let mut store = PropertyStore::default();
        store.set(PropertyKey::global("count"), PropertyValue::from("1"));
        store.set(PropertyKey::global("volume"), PropertyValue::F64(0.5));
        app.world.insert_resource(store);
        app.add_systems(TickStage::A11ySync, snap_signals);

        app.tick();
        {
            let snap = handle.0.read().unwrap();
            let count = snap.signals.iter().find(|s| s.name == "count").unwrap();
            assert_eq!(count.value, "1");
            assert_eq!(count.kind, "str");
            assert_eq!(count.last_changed_frame, 0, "first observation is 0");
            let vol = snap.signals.iter().find(|s| s.name == "volume").unwrap();
            assert_eq!(vol.kind, "f64");
            assert_eq!(vol.value, "0.5");
        }

        handle.0.write().unwrap().frame = 6;
        app.world
            .resource_mut::<PropertyStore>()
            .set(PropertyKey::global("count"), PropertyValue::from("2"));
        app.tick();
        let snap = handle.0.read().unwrap();
        let count = snap.signals.iter().find(|s| s.name == "count").unwrap();
        assert_eq!(count.value, "2");
        assert_eq!(count.last_changed_frame, 6, "change stamps current frame");
        let vol = snap.signals.iter().find(|s| s.name == "volume").unwrap();
        assert_eq!(vol.last_changed_frame, 0, "unchanged cell keeps its stamp");
    }
}

#[cfg(test)]
mod simulate_tests {
    use super::*;
    use crate::simulate::{SimulateKind, SimulateRequest};
    use bevy_ecs::message::Messages;
    use lumen_core::app::App;
    use lumen_core::tick::TickStage;

    /// A simulated click must update `PointerState.position` (not just write
    /// a `PointerMoved` message), so that `hit_test` - which reads the
    /// resource, not the message ring - sees the synthetic pointer and
    /// routes the click to the element under (x, y). Before the fix the
    /// drain only wrote the message, so synthetic clicks dispatched to
    /// wherever the real OS cursor last hovered (the wrong-element symptom).
    fn simulate_test_app(queue: &SimulateQueue) -> App {
        let mut app = App::new();
        app.world.insert_resource(queue.clone());
        app.world.init_resource::<SimulateProgress>();
        app.add_systems(TickStage::Input, drain_simulate_queue);
        app.add_systems(TickStage::A11ySync, publish_simulate_completion);
        app
    }

    #[test]
    fn simulate_click_updates_pointer_state_position() {
        let queue = SimulateQueue::default();
        let mut app = simulate_test_app(&queue);

        queue.push(SimulateRequest {
            kind: SimulateKind::Click {
                x: 200.0,
                y: 48.0,
                button: None,
            },
            wait_for: None,
        });
        app.tick();

        let pos = app.world.resource::<PointerState>().position;
        assert_eq!(
            pos,
            Some(glam::Vec2::new(200.0, 48.0)),
            "simulate Click must set PointerState.position so hit_test tracks it"
        );
        // The press/release pair must also reach the message bus for
        // dispatch_clicks to consume on this same tick.
        let pressed = app
            .world
            .resource::<Messages<PointerPressed>>()
            .iter_current_update_messages()
            .count();
        let released = app
            .world
            .resource::<Messages<PointerReleased>>()
            .iter_current_update_messages()
            .count();
        assert_eq!(pressed, 1, "one PointerPressed emitted");
        assert_eq!(released, 1, "one PointerReleased emitted");
    }

    /// A simulated `PointerMove` likewise updates `PointerState.position`.
    #[test]
    fn simulate_pointer_move_updates_position() {
        let queue = SimulateQueue::default();
        let mut app = simulate_test_app(&queue);

        queue.push(SimulateRequest {
            kind: SimulateKind::PointerMove { x: 10.0, y: 20.0 },
            wait_for: None,
        });
        app.tick();

        assert_eq!(
            app.world.resource::<PointerState>().position,
            Some(glam::Vec2::new(10.0, 20.0)),
        );
    }

    /// W6 T4 core contract: rapid-fire requests drain ONE per tick in
    /// FIFO order - a second request (the "Escape right after a click"
    /// shape) can never share the first request's tick.
    #[test]
    fn requests_drain_one_per_tick_in_fifo_order() {
        let queue = SimulateQueue::default();
        let mut app = simulate_test_app(&queue);

        let seq_a = queue.push(SimulateRequest {
            kind: SimulateKind::PointerMove { x: 1.0, y: 1.0 },
            wait_for: None,
        });
        let seq_b = queue.push(SimulateRequest {
            kind: SimulateKind::PointerMove { x: 2.0, y: 2.0 },
            wait_for: None,
        });
        assert_eq!((seq_a, seq_b), (1, 2), "monotonic sequence numbers");

        // Tick 1: only the FIRST request applies.
        app.tick();
        assert_eq!(
            app.world.resource::<PointerState>().position,
            Some(glam::Vec2::new(1.0, 1.0)),
            "tick 1 injects request A only"
        );
        assert_eq!(queue.completed_seq(), seq_a, "A's tick completed");

        // Tick 2: the second request follows.
        app.tick();
        assert_eq!(
            app.world.resource::<PointerState>().position,
            Some(glam::Vec2::new(2.0, 2.0)),
            "tick 2 injects request B"
        );
        assert_eq!(queue.completed_seq(), seq_b, "B's tick completed");
    }

    /// W6 T4: the completion seq publishes only AFTER the request's tick
    /// has run to `TickStage::A11ySync` - never at push time. This is
    /// what the TCP handler polls, so a response cannot race the tick.
    #[test]
    fn completion_seq_publishes_only_after_the_tick_runs() {
        let queue = SimulateQueue::default();
        let mut app = simulate_test_app(&queue);

        let seq = queue.push(SimulateRequest {
            kind: SimulateKind::PointerMove { x: 3.0, y: 4.0 },
            wait_for: None,
        });
        assert_eq!(
            queue.completed_seq(),
            0,
            "push alone must not mark completion"
        );
        app.tick();
        assert_eq!(queue.completed_seq(), seq, "tick publishes completion");
    }

    /// W6 T4: when the one-per-tick pop leaves requests queued, the drain
    /// re-fires the waker so the platform loop schedules the follow-up
    /// tick instead of parking with work pending.
    #[test]
    fn queued_backlog_rewakes_the_loop_for_a_follow_up_tick() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let queue = SimulateQueue::default();
        let mut app = simulate_test_app(&queue);
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        queue.set_waker(lumen_core::app::EventLoopWaker(std::sync::Arc::new(
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
            },
        )));

        queue.push(SimulateRequest {
            kind: SimulateKind::PointerMove { x: 1.0, y: 1.0 },
            wait_for: None,
        });
        queue.push(SimulateRequest {
            kind: SimulateKind::PointerMove { x: 2.0, y: 2.0 },
            wait_for: None,
        });
        assert_eq!(count.load(Ordering::SeqCst), 2, "one wake per push");

        app.tick(); // pops request 1, leaves request 2 queued
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "drain re-wakes for the queued backlog"
        );
        app.tick(); // pops request 2, queue empty
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "no re-wake once the queue is empty"
        );
    }

    /// End-to-end wiring: once `wire_simulate_waker` has run at least one
    /// tick with `EventLoopWaker` present as a resource, a push from
    /// outside the tick loop (standing in for the MCP server thread) must
    /// both (a) invoke the waker and (b) drain on the very next tick -
    /// i.e. the same tick a real winit loop would process after
    /// `EventLoopProxy::send_event` delivers the `Wake` user event. This is
    /// the regression this fix closes: simulated input no longer needs an
    /// unrelated OS event to surface.
    #[test]
    fn simulate_push_wakes_and_drains_within_one_proxied_event() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let queue = SimulateQueue::default();
        let mut app = simulate_test_app(&queue);
        app.add_systems(
            TickStage::Input,
            wire_simulate_waker.before(drain_simulate_queue),
        );

        let woken = std::sync::Arc::new(AtomicBool::new(false));
        let woken_in_callback = woken.clone();
        app.world
            .insert_resource(lumen_core::app::EventLoopWaker(std::sync::Arc::new(
                move || {
                    woken_in_callback.store(true, Ordering::SeqCst);
                },
            )));

        // Tick once with no pending work: wires the waker, but must not
        // fire it - idle quiescence, nothing was pushed yet.
        app.tick();
        assert!(
            !woken.load(Ordering::SeqCst),
            "wiring alone must not wake - only a push does"
        );

        // Simulate the MCP server thread pushing a request between ticks.
        queue.push(SimulateRequest {
            kind: SimulateKind::PointerMove { x: 5.0, y: 6.0 },
            wait_for: None,
        });
        assert!(
            woken.load(Ordering::SeqCst),
            "push must invoke the waker synchronously, before the next tick even runs"
        );

        // The next tick (standing in for the one a real winit loop runs
        // after its parked wait returns from the proxied Wake event) must
        // drain the request in full.
        app.tick();
        assert_eq!(
            app.world.resource::<PointerState>().position,
            Some(glam::Vec2::new(5.0, 6.0)),
            "the pushed request drains on the very next tick"
        );
    }
}
