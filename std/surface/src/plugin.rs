//! The plugin: the `<surface>` element, its systems, its events and its
//! script functions.
//!
//! Generic seams only. The tag registers on the shared tag registry, the
//! frames reach the screen through the native-paint seam and the render
//! backend's texture seam, the functions register on the `ScriptFnRegistry`,
//! the events travel as DOM custom events, and the default look is a
//! user-agent stylesheet.

use crate::api::{
    Entry, SurfaceElement, SurfaceEvent, SurfaceFrame, SurfaceState, Surfaces, find, registry,
};
use crate::gpu;
use crate::input::{ForwardState, forward_input};
use crate::paint::{EXTENSION_ID, SurfacePainter, extract_surfaces};
use crate::session::{self, Happened, Shared, SurfaceStateKind, Wanted};
use lumen_module::ModuleConfig;
use lumen_module::gpu::{GpuContext, GpuTextures};
use lumen_module::lumen_core::app::{App, EventLoopWaker, Plugin};
use lumen_module::lumen_core::components::{
    ImageComponent, InlineStyle, LumenAttributes, LumenClasses, LumenId, LumenTag, StyleDirty,
    TabIndex, Transform,
};
use lumen_module::lumen_core::input::PointerLockAsk;
use lumen_module::lumen_core::prelude::*;
use lumen_module::lumen_core::render_backend::RendererInfo;
use lumen_module::lumen_core::render_world::{FrameDirty, RenderStage};
use lumen_module::lumen_core::window::{Viewports, WindowRuntime};
use lumen_module::lumen_paint::TextureId;
use lumen_module::lumen_script::{
    CustomEvent, ScriptFnAppExt, ScriptFnBody, ScriptSet, ScriptValue,
};
use lumen_surface_protocol::wire::Pacing;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// The markup tag this module answers for.
pub const TAG: &str = "surface";

/// The module's name, as an app declares it.
pub const NAME: &str = "lumen-surface";

/// The web half's descriptor, which declares this module's functions.
pub const DESCRIPTOR: &str = include_str!("../web/lumen-addon.toml");

/// The default look: a block that is black until a frame shows, its frame
/// contained in its box. Token-reachable, and every app stylesheet overrides
/// it.
const UA_CSS: &str = "surface {
    display: block;
    object-fit: contain;
    background: var(--lumen-surface-background, #000000);
}
";

/// The default size of a surface with nothing else to go by, in logical
/// pixels: what a video element defaults to.
const DEFAULT_SIZE: (f32, f32) = (300.0, 150.0);

/// Shows frames another renderer draws on the GPU in `<surface>` elements.
///
/// Ships as the bundled `lumen-surface` runtime module (an app declares
/// `lumen-surface = { bundled = true }` under `[dependencies]`), and works the
/// same added as an ordinary plugin.
#[derive(Clone, Copy, Debug)]
pub struct SurfacePlugin {
    /// How producers pace frames when the element does not say:
    /// [`Pacing::Paced`] renders one frame per frame the surface shows.
    pub default_pacing: Pacing,
    /// How long a requested frame may take before the surface counts as
    /// stalled, when the element does not say.
    pub stall_timeout: Duration,
}

impl Default for SurfacePlugin {
    fn default() -> Self {
        Self {
            default_pacing: Pacing::Paced,
            stall_timeout: Duration::from_millis(1000),
        }
    }
}

impl SurfacePlugin {
    /// Build from the module's `config` table: `pacing` (`"paced"` or
    /// `"free"`) and `stall_timeout` (milliseconds).
    #[must_use]
    pub fn new(config: ModuleConfig) -> Self {
        let mut plugin = Self::default();
        if let Some(pacing) = config.str("pacing").and_then(parse_pacing) {
            plugin.default_pacing = pacing;
        }
        if let Some(ms) = config
            .int("stall_timeout")
            .and_then(|v| u64::try_from(v).ok())
        {
            plugin.stall_timeout = Duration::from_millis(ms.clamp(50, 60_000));
        }
        plugin
    }
}

/// What the plugin was built with, for the adoption system.
#[derive(Resource, Clone, Copy)]
struct Defaults(SurfacePlugin);

fn parse_pacing(s: &str) -> Option<Pacing> {
    match s {
        "paced" => Some(Pacing::Paced),
        "free" => Some(Pacing::Free),
        _ => None,
    }
}

/// System sets the module's systems run in.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceSet {
    /// New elements start their sessions.
    Adopt,
    /// Sessions report what happened; sizes and visibility go out.
    Sync,
    /// Input on an element goes to its producer.
    Input,
}

impl Plugin for SurfacePlugin {
    fn build(self, app: &mut App) {
        lumen_module::lumen_widget::register_widget_tag_owned(TAG);
        app.world.insert_resource(Defaults(self));
        app.world.insert_resource(Surfaces);
        app.world.init_resource::<ForwardState>();
        app.add_ua_stylesheet(NAME, UA_CSS);
        message::<SurfaceEvent>(&mut app.world);
        // Registered by the DOM event layer in a full app; an app assembled
        // without it (a test, a bare embedder) still runs the module.
        message::<CustomEvent>(&mut app.world);
        match script_fns() {
            Ok(fns) => {
                app.add_script_fns(fns);
            }
            Err(e) => lumen_module::lumen_core::warn_line!("lumen-surface: {e}"),
        }
        app.add_extract_fn(extract_surfaces);
        app.register_native_painter(EXTENSION_ID, SurfacePainter);
        app.add_render_systems(RenderStage::Prepare, take_gpu);
        app.add_systems(
            TickStage::Systems,
            (adopt_surfaces, release_surfaces)
                .in_set(SurfaceSet::Adopt)
                .before(ScriptSet::Dispatch)
                .before(ScriptSet::Ready),
        );
        app.add_systems(
            TickStage::Systems,
            sync_surfaces
                .in_set(SurfaceSet::Sync)
                .after(SurfaceSet::Adopt)
                .before(ScriptSet::DomState),
        );
        app.add_systems(
            TickStage::Systems,
            (forward_input, ask_for_frames)
                .chain()
                .in_set(SurfaceSet::Input)
                .after(SurfaceSet::Sync)
                .after(lumen_module::lumen_input::pointer_lock::grant_pointer_lock),
        );
    }
}

/// Register message `T` unless something already did.
fn message<T: bevy_ecs::message::Message>(world: &mut World) {
    if !world.contains_resource::<bevy_ecs::message::Messages<T>>() {
        bevy_ecs::message::MessageRegistry::register_message::<T>(world);
    }
}

/// Copy the device the renderer published to where sessions read it, and
/// say which GPU it is in every endpoint string.
fn take_gpu(context: Option<Res<GpuContext>>, textures: Option<Res<GpuTextures>>) {
    let (Some(context), Some(textures)) = (context, textures) else {
        return;
    };
    if !gpu::publish(&context, &textures) {
        return;
    }
    let hint = gpu::current().as_ref().and_then(gpu::adapter_hint);
    if let Some(entries) = registry().as_ref() {
        for entry in entries.values() {
            entry.shared.set_adapter_hint(hint.clone());
        }
    }
}

/// Elements that appeared this tick.
type NewElements<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static LumenTag,
        Option<&'static LumenId>,
        Option<&'static LumenAttributes>,
        Has<TabIndex>,
    ),
    Added<LumenTag>,
>;

/// The sessions of elements alive, by entity, so one whose element went away
/// is closed.
#[derive(Default)]
struct Adopted(HashMap<Entity, (Arc<Shared>, TextureId)>);

/// Start a session for each `<surface>` that appeared.
fn adopt_surfaces(
    mut commands: Commands,
    new: NewElements,
    defaults: Res<Defaults>,
    mut unnamed: Local<u64>,
    renderer: Option<Res<RendererInfo>>,
) {
    let cpu = renderer.is_some_and(|r| r.backend == "cpu");
    for (entity, tag, id, attrs, focusable) in &new {
        if &*tag.0 != TAG {
            continue;
        }
        let id = match id {
            Some(id) if !id.0.is_empty() => id.0.clone(),
            _ => {
                *unnamed += 1;
                format!("surface-{}", *unnamed)
            }
        };
        let attr = |name: &str| attrs.and_then(|a| a.get(name)).map(str::to_string);
        let channel = attr("channel").unwrap_or_else(|| id.clone());
        let texture = TextureId::new();
        let shared = match session::start(&id, &channel, gpu::backend(texture)) {
            Ok(shared) => shared,
            Err(e) => {
                lumen_module::lumen_core::warn_line!(
                    "lumen-surface: <surface id=\"{id}\"> could not listen for a producer: {e}"
                );
                continue;
            }
        };
        let pacing = attr("pacing")
            .as_deref()
            .and_then(parse_pacing)
            .unwrap_or(defaults.0.default_pacing);
        shared.set_pacing(pacing);
        let stall = attr("stall-timeout")
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(defaults.0.stall_timeout, Duration::from_millis);
        shared.set_stall_timeout(stall);
        if let Some(g) = gpu::current() {
            shared.set_adapter_hint(gpu::adapter_hint(&g));
        }
        if cpu {
            shared.set_unsupported("the app renders on the CPU, which cannot show shared frames");
        }
        registry().get_or_insert_with(HashMap::new).insert(
            id.clone(),
            Entry {
                shared: shared.clone(),
                texture,
            },
        );
        let mut e = commands.entity(entity);
        e.insert((
            SurfaceElement {
                shared: shared.clone(),
                texture,
            },
            SurfaceState(shared.state()),
            SurfaceFrame::default(),
            // A replaced element: until CSS sizes it, it is the size of a
            // video element.
            ImageComponent {
                source: format!("<{TAG}>"),
                natural_size: Some(glam::Vec2::new(DEFAULT_SIZE.0, DEFAULT_SIZE.1)),
            },
        ));
        if !focusable {
            e.insert(TabIndex(0));
        }
        let state = shared.state();
        commands.queue(move |world: &mut World| reflect_state(world, entity, state));
    }
}

/// Close the session of each element that went away.
fn release_surfaces(
    mut gone: RemovedComponents<SurfaceElement>,
    mut adopted: Local<Adopted>,
    added: Query<(Entity, &SurfaceElement), Added<SurfaceElement>>,
) {
    for (entity, element) in &added {
        adopted
            .0
            .insert(entity, (element.shared.clone(), element.texture));
    }
    for entity in gone.read() {
        if let Some((shared, texture)) = adopted.0.remove(&entity) {
            shared.close();
            gpu::forget(texture);
            let mut registry = registry();
            if let Some(entries) = registry.as_mut()
                && entries
                    .get(&shared.id)
                    .is_some_and(|e| Arc::ptr_eq(&e.shared, &shared))
            {
                entries.remove(&shared.id);
            }
        }
    }
}

/// Put `state` on the element where CSS and scripts see it: the `state`
/// attribute, and the class `surface-<state>` (CSS selects by class).
fn reflect_state(world: &mut World, entity: Entity, state: SurfaceStateKind) {
    let Ok(mut e) = world.get_entity_mut(entity) else {
        return;
    };
    let mut attrs = e.take::<LumenAttributes>().unwrap_or_default();
    attrs.set("state", state.as_str());
    let mut classes = e.take::<LumenClasses>().unwrap_or_default();
    classes.0.retain(|c| !c.starts_with("surface-"));
    classes.0.push(format!("surface-{}", state.as_str()).into());
    e.insert((attrs, classes, SurfaceState(state), StyleDirty));
}

/// Report what the sessions did, and keep each producer told what its
/// element wants.
fn sync_surfaces(
    mut commands: Commands,
    mut elements: Query<(
        Entity,
        &SurfaceElement,
        &SurfaceState,
        &mut SurfaceFrame,
        Option<&Transform>,
    )>,
    viewports: Viewports,
    runtimes: Query<&WindowRuntime>,
    (mut events, mut custom, mut locks): (
        MessageWriter<SurfaceEvent>,
        MessageWriter<CustomEvent>,
        MessageWriter<PointerLockAsk>,
    ),
    mut frame_dirty: Option<ResMut<FrameDirty>>,
    (waker, renderer): (Option<Res<EventLoopWaker>>, Option<Res<RendererInfo>>),
) {
    let cpu = renderer.is_some_and(|r| r.backend == "cpu");
    for (entity, element, state, mut frame, transform) in &mut elements {
        let shared = &element.shared;
        if let Some(waker) = waker.as_deref() {
            let waker = waker.clone();
            shared.set_waker(Arc::new(move || waker.wake()));
        }
        if cpu {
            shared.set_unsupported("the app renders on the CPU, which cannot show shared frames");
        }
        // The size the producer is asked for: the element's box in device
        // pixels, so each frame texel lands on one screen pixel. A box with
        // no area (`display: none`) keeps the size asked last: the element
        // is hidden, which the visibility message says, and its frames come
        // back at that size when it shows again.
        let scale = viewports.of(entity).map_or(1.0, |v| v.scale_factor);
        if let Some(t) = transform.filter(|t| t.size.x > 0.0 && t.size.y > 0.0) {
            let wanted = Wanted {
                width: (t.size.x * scale).round().max(1.0) as u32,
                height: (t.size.y * scale).round().max(1.0) as u32,
                scale,
            };
            if shared.wanted() != wanted {
                if shared.set_wanted(wanted) {
                    let (width, height) = (wanted.width, wanted.height);
                    events.write(SurfaceEvent::Resized {
                        entity,
                        width,
                        height,
                    });
                    custom.write(
                        CustomEvent::new(entity, "surfaceresize")
                            .with("width", i64::from(width))
                            .with("height", i64::from(height)),
                    );
                } else {
                    // Not connected: the size goes out with the next welcome.
                }
            }
        }
        // Anyone can see it when it was drawn and its window shows.
        let window_shows = viewports
            .window_of(entity)
            .and_then(|w| runtimes.get(w).ok())
            .is_none_or(|r| !r.occluded && !r.minimized);
        shared.set_visible(shared.placement().is_some() && window_shows);

        let (happened, fresh) = shared.take();
        if fresh {
            if let Some(info) = shared.frame() {
                frame.0 = info;
            }
            // Only the texture changed: put the frame up again without
            // extracting the scene.
            if let Some(fd) = frame_dirty.as_deref_mut() {
                fd.present = true;
            }
            // Asked for once this tick's input went out ([`ask_for_frames`]).
            shared.ask_after_input();
        }
        for what in happened {
            report(
                entity,
                what,
                &mut events,
                &mut custom,
                &mut locks,
                &mut commands,
            );
        }
        let now = shared.state();
        if now != state.0 {
            commands.queue(move |world: &mut World| reflect_state(world, entity, now));
            // The leaf's stamp moves with the first frame, the last frame
            // stays on a loss: either way the scene is extracted again.
            if let Some(fd) = frame_dirty.as_deref_mut() {
                fd.dirty = true;
            }
        }
    }
}

/// Ask each producer whose frame showed this tick for its next one, now
/// that the tick's input has gone ahead of the request.
fn ask_for_frames(elements: Query<&SurfaceElement>) {
    for element in &elements {
        element.shared.send_asked();
    }
}

/// Turn one thing a session did into the element's event and message.
fn report(
    entity: Entity,
    what: Happened,
    events: &mut MessageWriter<SurfaceEvent>,
    custom: &mut MessageWriter<CustomEvent>,
    locks: &mut MessageWriter<PointerLockAsk>,
    commands: &mut Commands,
) {
    match what {
        Happened::Connected { engine, pid } => {
            custom.write(
                CustomEvent::new(entity, "surfaceconnect")
                    .with("engine", engine.clone())
                    .with("pid", i64::from(pid)),
            );
            events.write(SurfaceEvent::Connected {
                entity,
                engine,
                pid,
            });
        }
        Happened::Stalled { waited_ms } => {
            custom.write(
                CustomEvent::new(entity, "surfacestall")
                    .with("waited_ms", i64::try_from(waited_ms).unwrap_or(i64::MAX)),
            );
            events.write(SurfaceEvent::Stalled { entity, waited_ms });
        }
        Happened::Resumed => {
            custom.write(CustomEvent::new(entity, "surfaceresume"));
            events.write(SurfaceEvent::Resumed { entity });
        }
        Happened::Disconnected { reason, detail } => {
            custom.write(
                CustomEvent::new(entity, "surfacedisconnect")
                    .with("reason", reason)
                    .with("detail", detail.clone()),
            );
            events.write(SurfaceEvent::Disconnected {
                entity,
                reason: reason.into(),
                detail,
            });
        }
        Happened::Error { reason, detail } => {
            lumen_module::lumen_core::warn_line!("lumen-surface: {detail}");
            custom.write(
                CustomEvent::new(entity, "surfaceerror")
                    .with("reason", reason)
                    .with("detail", detail.clone()),
            );
            events.write(SurfaceEvent::Error {
                entity,
                reason: reason.into(),
                detail,
            });
        }
        Happened::Cursor(keyword) => {
            commands.queue(move |world: &mut World| {
                if let Ok(mut e) = world.get_entity_mut(entity) {
                    let mut style = e.take::<InlineStyle>().unwrap_or_default();
                    style.set("cursor", keyword);
                    e.insert((style, StyleDirty));
                }
            });
        }
        Happened::PointerLock(wanted) => {
            locks.write(PointerLockAsk {
                entity: wanted.then_some(entity),
            });
        }
        Happened::User(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let list: Vec<ScriptValue> = bytes
                .iter()
                .map(|b| ScriptValue::I64(i64::from(*b)))
                .collect();
            custom.write(
                CustomEvent::new(entity, "surfacemessage")
                    .with("text", text)
                    .with("bytes", ScriptValue::Array(list)),
            );
            events.write(SurfaceEvent::Message { entity, bytes });
        }
    }
}

/// The `surface` namespace, with the signatures the web half declares.
fn script_fns() -> Result<Vec<lumen_module::lumen_script::ScriptFn>, String> {
    let id = |cx: &lumen_module::lumen_script::ScriptFnCx<'_>| find(&cx.str_arg(0));
    let bodies: Vec<(String, ScriptFnBody)> = vec![
        (
            "endpoint".into(),
            Arc::new(move |cx| {
                Ok(ScriptValue::Str(
                    id(cx).and_then(|e| e.shared.endpoint()).unwrap_or_default(),
                ))
            }),
        ),
        (
            "state".into(),
            Arc::new(move |cx| {
                Ok(ScriptValue::Str(
                    id(cx)
                        .map(|e| e.shared.state().as_str().to_string())
                        .unwrap_or_default(),
                ))
            }),
        ),
        (
            "frame_info".into(),
            Arc::new(move |cx| {
                let mut map = HashMap::new();
                if let Some(f) = id(cx).and_then(|e| e.shared.frame()) {
                    let int = |v: u64| ScriptValue::I64(i64::try_from(v).unwrap_or(i64::MAX));
                    map.insert("frame".into(), int(f.frame));
                    map.insert("width".into(), int(u64::from(f.size.0)));
                    map.insert("height".into(), int(u64::from(f.size.1)));
                    map.insert("generation".into(), int(u64::from(f.generation)));
                    map.insert("input_seq".into(), int(f.input_seq));
                    map.insert("meta_len".into(), int(f.meta.len() as u64));
                }
                Ok(ScriptValue::Map(map))
            }),
        ),
        (
            "frame_meta".into(),
            Arc::new(move |cx| {
                let bytes = id(cx)
                    .and_then(|e| e.shared.frame())
                    .map(|f| f.meta)
                    .unwrap_or_default();
                Ok(ScriptValue::Array(
                    bytes
                        .into_iter()
                        .map(|b| ScriptValue::I64(i64::from(b)))
                        .collect(),
                ))
            }),
        ),
        (
            "request_frame".into(),
            Arc::new(move |cx| {
                if let Some(e) = id(cx) {
                    e.shared.request_frame();
                }
                Ok(ScriptValue::Unit)
            }),
        ),
        (
            "send".into(),
            Arc::new(move |cx| {
                let message = cx.str_arg(1);
                Ok(ScriptValue::Bool(
                    id(cx).is_some_and(|e| e.shared.send_user(message.as_bytes())),
                ))
            }),
        ),
        (
            "disconnect".into(),
            Arc::new(move |cx| {
                if let Some(e) = id(cx) {
                    e.shared.disconnect();
                }
                Ok(ScriptValue::Unit)
            }),
        ),
    ];
    lumen_module::web_half_fns(NAME, DESCRIPTOR, bodies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_module::lumen_script::ScriptNs;

    /// One surface on every target: the desktop half registers exactly the
    /// functions the web half declares, with the same signatures.
    #[test]
    fn the_desktop_functions_are_the_web_half_s() {
        let addon =
            lumen_module::describe_web_half(NAME, DESCRIPTOR).expect("the descriptor reads");
        let fns = script_fns().expect("every function has a body");
        assert_eq!(fns.len(), addon.functions.len());
        for (native, declared) in fns.iter().zip(&addon.functions) {
            assert_eq!(native.name, declared.name);
            assert_eq!(native.ns, ScriptNs::Named(addon.namespace.clone()));
        }
        assert_eq!(addon.elements.len(), 1);
        assert_eq!(addon.elements[0].tag, TAG);
    }

    /// A surface no element answers for reads as empty, and calls on it do
    /// nothing.
    #[test]
    fn calls_on_an_unknown_surface_answer_empty() {
        let fns = script_fns().unwrap();
        let call = |name: &str, args: &[ScriptValue]| {
            let f = fns.iter().find(|f| f.name == name).unwrap();
            let mut out = Vec::new();
            (f.body)(&mut lumen_module::lumen_script::ScriptFnCx::new(
                args, &mut out,
            ))
            .unwrap()
        };
        let none = ScriptValue::Str("no-such-surface".into());
        assert_eq!(
            call("endpoint", std::slice::from_ref(&none)),
            ScriptValue::Str(String::new())
        );
        assert_eq!(
            call("state", std::slice::from_ref(&none)),
            ScriptValue::Str(String::new())
        );
        assert_eq!(
            call("send", &[none.clone(), ScriptValue::Str("x".into())]),
            ScriptValue::Bool(false)
        );
        assert_eq!(call("frame_meta", &[none]), ScriptValue::Array(Vec::new()));
    }
}
