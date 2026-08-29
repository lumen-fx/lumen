//! The plugin that puts a drawing surface into an app: the `<canvas>`
//! element, the `canvas` script namespace, and the pixels behind them.
//!
//! Three generic seams and nothing else. The functions register on the app's
//! `ScriptFnRegistry`, so every host binds them. The tag registers on the
//! shared tag registry, so the markup parser accepts `<canvas>`. And the
//! pixels reach the screen through the engine's native-paint seam, so the
//! renderer draws them without knowing what they are.
//!
//! Two rules shape the surface, the same two the other modules follow:
//!
//! - **The id is the handle.** A canvas is named by the `id` its element
//!   carries, and every call takes that id first. There is no context object
//!   to get, nothing to keep in a variable, and nothing to invalidate when
//!   the tree is rebuilt.
//! - **A refusal degrades, it does not raise.** A call naming a canvas no
//!   element answers for is kept anyway, because the element may not be
//!   spawned yet; if it never appears, the module says so once on stderr and
//!   the app keeps running.

use lumen_module::ModuleConfig;
use lumen_module::lumen_core::app::{App, EventLoopWaker, Plugin};
use lumen_module::lumen_core::app_paths;
use lumen_module::lumen_core::components::{ImageComponent, Length, LumenId, LumenTag, Style};
use lumen_module::lumen_core::prelude::*;
use lumen_module::lumen_core::render_world::FrameDirty;
use lumen_module::lumen_script::{
    ScriptFn, ScriptFnAppExt, ScriptNs, ScriptSet, ScriptTy as T, ScriptValue,
};
use lumen_module::lumen_text::ShaperService;

use crate::buffer::PixBuf;
use crate::color::{self, Rgba};
use crate::encode::{self, BlobCache};
use crate::ops::{FontSpec, LineCap, LineJoin, Op};
use crate::paint::{CanvasPainter, EXTENSION_ID, extract_canvases};
use crate::store::{self, Caps, UA_SIZE};

/// The namespace the functions live in: `canvas::fill_rect(..)` in Rhai and
/// candela, `canvas.fill_rect(..)` in Lua.
const NAMESPACE: &str = "canvas";

/// The markup tag this module answers for.
pub const TAG: &str = "canvas";

/// A `<canvas>` element this module has adopted.
///
/// Carries the encoded drawing rather than pointing back at the store: the
/// extract runs mid-frame and has no business taking a process-wide lock, and
/// this is also what makes the canvas participate in change detection like
/// any other component.
#[derive(Component)]
pub struct Canvas {
    /// The element's id, which is the name every call uses.
    pub id: String,
    /// The drawing space, in canvas units.
    pub logical: (f32, f32),
    /// The encoded scene the painter appends.
    pub scene: std::sync::Arc<lumen_module::lumen_render_wgpu::vello::Scene>,
    /// Bumped whenever the scene changes.
    pub revision: u64,
}

/// A drawing surface for a Lumen app: install it and `<canvas>` elements
/// exist, along with the `canvas` functions that draw on them.
///
/// Ships as the bundled `lumen-canvas` runtime module (an app declares
/// `lumen-canvas = { bundled = true, tags = ["canvas"] }` under
/// `[dependencies]`), and works the same added as an ordinary plugin in a
/// static build. Without it there is no `<canvas>` tag and no `canvas`
/// namespace.
#[derive(Default)]
pub struct CanvasPlugin {
    caps: Caps,
}

impl CanvasPlugin {
    /// Build from the module's `config` table. `region_cap`,
    /// `buffer_pixel_cap`, and `buffer_count_cap` are integer counts, each
    /// clamped into the range the module supports; anything else leaves the
    /// default in place.
    #[must_use]
    pub fn new(config: ModuleConfig) -> Self {
        let cap = |key: &str, min: u64, max: u64, default: u64| match config.int(key) {
            Some(asked) => u64::try_from(asked).unwrap_or(min).clamp(min, max),
            None => default,
        };
        CanvasPlugin {
            caps: Caps {
                region: cap(
                    "region_cap",
                    store::MIN_REGION_CAP,
                    store::MAX_REGION_CAP,
                    store::DEFAULT_REGION_CAP,
                ),
                buffer_pixels: cap(
                    "buffer_pixel_cap",
                    store::MIN_BUFFER_PIXEL_CAP,
                    store::MAX_BUFFER_PIXEL_CAP,
                    store::DEFAULT_BUFFER_PIXEL_CAP,
                ),
                buffer_count: cap(
                    "buffer_count_cap",
                    store::MIN_BUFFER_COUNT_CAP,
                    store::MAX_BUFFER_COUNT_CAP,
                    store::DEFAULT_BUFFER_COUNT_CAP,
                ),
            },
        }
    }

    /// Build with explicit caps, clamped into the supported ranges. This is
    /// what a static build sets when it installs the plugin itself and has no
    /// `config` table to read.
    #[must_use]
    pub fn with_caps(region: u64, buffer_pixels: u64, buffer_count: u64) -> Self {
        CanvasPlugin {
            caps: Caps {
                region: region.clamp(store::MIN_REGION_CAP, store::MAX_REGION_CAP),
                buffer_pixels: buffer_pixels
                    .clamp(store::MIN_BUFFER_PIXEL_CAP, store::MAX_BUFFER_PIXEL_CAP),
                buffer_count: buffer_count
                    .clamp(store::MIN_BUFFER_COUNT_CAP, store::MAX_BUFFER_COUNT_CAP),
            },
        }
    }

    /// The caps this plugin will install.
    #[must_use]
    pub fn caps(&self) -> Caps {
        self.caps
    }
}

impl Plugin for CanvasPlugin {
    fn build(self, app: &mut App) {
        store::store().caps = self.caps;
        // The tag, published from here so a run that loads this module can
        // parse `<canvas>` markup. An app that also compiles ahead of time
        // declares the same tag in `lumen.toml`, which is what the compile
        // reads; both spellings register the same name.
        lumen_module::lumen_widget::register_widget_tag_owned(TAG);
        app.add_script_fns(script_fns());
        app.add_extract_fn(extract_canvases);
        app.register_native_painter(EXTENSION_ID, CanvasPainter);
        // Adoption runs before any handler does, so the first `on_ready` to
        // ask how big a canvas is gets the size its markup declared rather
        // than the default it would have had a moment earlier.
        app.add_systems(
            TickStage::Systems,
            adopt_canvases
                .before(ScriptSet::Dispatch)
                .before(ScriptSet::Ready)
                .before(ScriptSet::Frame),
        );
        // Encoding runs after the input dispatch, the frame hook, and the
        // timers: every handler that could draw has run, so one encode per
        // tick covers all of them.
        app.add_systems(
            TickStage::Systems,
            encode_canvases
                .after(adopt_canvases)
                .after(ScriptSet::Dispatch)
                .after(ScriptSet::Frame)
                .after(ScriptSet::Timers),
        );
    }
}

// ---------------------------------------------------------------------
// The per-tick system
// ---------------------------------------------------------------------

/// Where the encoder's per-buffer upload cache lives between ticks.
#[derive(Default)]
struct Encoder {
    blobs: BlobCache,
}

/// Elements that appeared this tick, with what a canvas needs off them.
type NewElements<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static LumenTag,
        Option<&'static LumenId>,
        Option<&'static Style>,
    ),
    Added<LumenTag>,
>;

/// Adopt the `<canvas>` elements that appeared this tick.
///
/// Runs before any handler, so a script that asks how big its canvas is gets
/// the size the markup declared. A canvas drawn on before its element existed
/// keeps what it recorded; only the size is the element's to state, and a
/// `canvas::resize` recorded afterwards still wins, because a resize is
/// replayed after this.
fn adopt_canvases(
    mut commands: Commands,
    new_elements: NewElements,
    waker: Option<Res<EventLoopWaker>>,
) {
    let mut store = store::store();
    // Wire the loop waker lazily: the resource appears once a windowing
    // backend runs, which is after this plugin built. Drawing calls made from
    // a script wake the loop through it.
    if store.waker.is_none()
        && let Some(waker) = waker.as_deref()
    {
        store.waker = Some(waker.clone());
    }

    for (entity, tag, id, style) in &new_elements {
        if &*tag.0 != TAG {
            continue;
        }
        let id = id.map(|i| i.0.clone()).unwrap_or_default();
        let logical = declared_size(style);
        store.surface(&id).logical = logical;
        store.answered.insert(id.clone());
        commands.entity(entity).insert((
            Canvas {
                id,
                logical,
                scene: std::sync::Arc::new(lumen_module::lumen_render_wgpu::vello::Scene::new()),
                revision: 0,
            },
            // What gives the element its box. An image with a natural size is
            // the one leaf the layout engine already sizes the way a canvas
            // needs: the declared drawing space is the default size, and CSS
            // width / height override it. Nothing loads: the asset pipeline
            // keys off `ImageSource`, which a canvas never carries.
            ImageComponent {
                source: format!("<{TAG}>"),
                natural_size: Some(glam::Vec2::new(logical.0, logical.1)),
            },
        ));
    }
}

/// Replay what the scripts recorded this tick, and say when a canvas changed.
fn encode_canvases(
    mut canvases: Query<(&mut Canvas, Option<&mut ImageComponent>)>,
    mut frame_dirty: Option<ResMut<FrameDirty>>,
    shaper: Option<NonSendMut<ShaperService>>,
    mut encoder: Local<Encoder>,
) {
    let mut store = store::store();
    let store = &mut *store;
    encoder.blobs.retain(&store.buffers);
    let mut shaper = shaper;
    let mut any_drew = false;
    for (mut canvas, image) in &mut canvases {
        let Some(surface) = store.surfaces.get_mut(&canvas.id) else {
            continue;
        };
        let ops = std::mem::take(&mut surface.pending);
        let resized = surface.logical != canvas.logical;
        let drew = encode::encode(
            surface,
            ops,
            &store.buffers,
            &mut encoder.blobs,
            shaper
                .as_deref_mut()
                .map(|s| &mut **s as &mut dyn lumen_module::lumen_text::TextShaper),
        );
        if drew {
            surface.revision += 1;
        }
        if !drew && !resized && std::sync::Arc::ptr_eq(&canvas.scene, &surface.scene) {
            continue;
        }
        canvas.scene = surface.scene.clone();
        canvas.logical = surface.logical;
        canvas.revision = surface.revision;
        if let Some(mut image) = image {
            let natural = glam::Vec2::new(surface.logical.0, surface.logical.1);
            if image.natural_size != Some(natural) {
                image.natural_size = Some(natural);
            }
        }
        any_drew = true;
    }
    if any_drew && let Some(frame_dirty) = frame_dirty.as_deref_mut() {
        frame_dirty.dirty = true;
    }

    // A canvas that was drawn on but has no element is usually a typo in the
    // id, and the drawing would otherwise vanish with no explanation. The
    // adopted set is what this reads rather than the component query: an
    // element adopted on this tick has no component until the commands
    // apply, and reporting it would be reporting a canvas that works.
    let answered = &store.answered;
    let orphans: Vec<String> = store
        .surfaces
        .iter()
        .filter(|(id, surface)| !answered.contains(*id) && !surface.pending.is_empty())
        .map(|(id, _)| id.clone())
        .collect();
    for id in orphans {
        let message = format!("no <{TAG}> element has id=\"{id}\", so its drawing is not shown");
        store.report_once(&id, &message);
        // The journal is kept, because the element may still be mounted; it
        // is bounded, because it may equally never be.
        let pending = &mut store.surface(&id).pending;
        if pending.len() > store::UNANSWERED_JOURNAL_CAP {
            let excess = pending.len() - store::UNANSWERED_JOURNAL_CAP;
            pending.drain(..excess);
        }
    }
}

/// The drawing space an element declares, or the size the HTML canvas has
/// always defaulted to.
///
/// `width` / `height` on a `<canvas>` are the drawing space, not the box:
/// they say how many units the script draws in, and CSS then scales that onto
/// whatever the box turns out to be. A declaration in any other unit says
/// nothing about the drawing space, so it takes the default.
fn declared_size(style: Option<&Style>) -> (f32, f32) {
    match style {
        Some(Style {
            width: Length::Px(w),
            height: Length::Px(h),
            ..
        }) if *w > 0.0 && *h > 0.0 => (*w, *h),
        _ => UA_SIZE,
    }
}

// ---------------------------------------------------------------------
// The script surface
// ---------------------------------------------------------------------

/// Record one op against the surface an id names.
fn record(id: String, op: Op) {
    store::store().record(&id, op);
}

/// Report a refusal and answer with what the script sees instead.
fn degrade<T>(outcome: Result<T, String>, fallback: T) -> T {
    match outcome {
        Ok(value) => value,
        Err(message) => {
            lumen_module::lumen_core::warn_line!("lumen-canvas: {message}");
            fallback
        }
    }
}

/// A path an app author wrote, against the app.
fn resolve(path: String) -> std::path::PathBuf {
    app_paths::resolve(path)
}

/// One array element as an integer, with the coercions every host argument
/// gets.
fn int_of(value: &ScriptValue) -> i64 {
    match value {
        ScriptValue::I64(v) => *v,
        ScriptValue::F64(v) => *v as i64,
        ScriptValue::Bool(b) => i64::from(*b),
        ScriptValue::Str(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// A script integer as a pixel value, saturating rather than wrapping.
fn pixel_of(value: i64) -> u32 {
    u32::try_from(value & 0xffff_ffff).unwrap_or(0)
}

/// The `canvas` surface, described once for every host. Names, parameters,
/// and docs are the contract a script writes against.
fn script_fns() -> Vec<ScriptFn> {
    let f = |name: &str, doc: &str| {
        ScriptFn::new(name)
            .ns(ScriptNs::Named(NAMESPACE.to_string()))
            .doc(doc)
    };
    let mut fns = Vec::new();
    fns.extend(surface_fns(&f));
    fns.extend(path_fns(&f));
    fns.extend(state_fns(&f));
    fns.extend(transform_fns(&f));
    fns.extend(text_fns(&f));
    fns.extend(buffer_fns(&f));
    fns
}

/// A builder for one function in the namespace.
type Describe<'a> = &'a dyn Fn(&str, &str) -> lumen_module::lumen_script::ScriptFnBuilder;

/// The canvas itself: how big it is, and how to empty it.
fn surface_fns(f: Describe<'_>) -> Vec<ScriptFn> {
    vec![
        f("width", "The canvas's drawing width, in canvas units.")
            .param("id", T::Str)
            .ret(T::Int)
            .build(|cx| Ok(ScriptValue::I64(pending_size(&cx.str_arg(0)).0 as i64))),
        f("height", "The canvas's drawing height, in canvas units.")
            .param("id", T::Str)
            .ret(T::Int)
            .build(|cx| Ok(ScriptValue::I64(pending_size(&cx.str_arg(0)).1 as i64))),
        f(
            "resize",
            "Set the drawing space, in canvas units. This empties the canvas.",
        )
        .param("id", T::Str)
        .param("width", T::Float)
        .param("height", T::Float)
        .build(|cx| {
            record(
                cx.str_arg(0),
                Op::Resize(
                    cx.float_arg(1).max(0.0) as f32,
                    cx.float_arg(2).max(0.0) as f32,
                ),
            );
            Ok(ScriptValue::Unit)
        }),
        f("clear", "Erase everything the canvas holds.")
            .param("id", T::Str)
            .build(|cx| {
                record(cx.str_arg(0), Op::Clear);
                Ok(ScriptValue::Unit)
            }),
    ]
}

/// The drawing space a script would see: the last size a pending `resize`
/// asked for, or the one the canvas already has.
///
/// A resize is journalled like every other call, so it has not been applied
/// yet when the next line of the same handler asks how wide the canvas is.
/// Answering with the old size there would make the common
/// `resize(..); for x in 0..width(..)` shape draw the wrong picture.
fn pending_size(id: &str) -> (f32, f32) {
    let mut store = store::store();
    let surface = store.surface(id);
    surface
        .pending
        .iter()
        .rev()
        .find_map(|op| match op {
            Op::Resize(w, h) => Some((*w, *h)),
            _ => None,
        })
        .unwrap_or(surface.logical)
}

/// Building a path, and filling or stroking it.
fn path_fns(f: Describe<'_>) -> Vec<ScriptFn> {
    let xy = |name: &str, doc: &str, op: fn(f64, f64) -> Op| {
        f(name, doc)
            .param("id", T::Str)
            .param("x", T::Float)
            .param("y", T::Float)
            .build(move |cx| {
                record(cx.str_arg(0), op(cx.float_arg(1), cx.float_arg(2)));
                Ok(ScriptValue::Unit)
            })
    };
    let rect = |name: &str, doc: &str, op: fn(f64, f64, f64, f64) -> Op| {
        f(name, doc)
            .param("id", T::Str)
            .param("x", T::Float)
            .param("y", T::Float)
            .param("width", T::Float)
            .param("height", T::Float)
            .build(move |cx| {
                record(
                    cx.str_arg(0),
                    op(
                        cx.float_arg(1),
                        cx.float_arg(2),
                        cx.float_arg(3),
                        cx.float_arg(4),
                    ),
                );
                Ok(ScriptValue::Unit)
            })
    };
    let bare = |name: &str, doc: &str, op: Op| {
        f(name, doc).param("id", T::Str).build(move |cx| {
            record(cx.str_arg(0), op.clone());
            Ok(ScriptValue::Unit)
        })
    };
    vec![
        bare("begin_path", "Start a new path.", Op::BeginPath),
        xy("move_to", "Start a subpath at a point.", Op::MoveTo),
        xy("line_to", "Straight segment to a point.", Op::LineTo),
        f("quad_to", "Quadratic segment through a control point.")
            .param("id", T::Str)
            .param("cx", T::Float)
            .param("cy", T::Float)
            .param("x", T::Float)
            .param("y", T::Float)
            .build(|cx| {
                record(
                    cx.str_arg(0),
                    Op::QuadTo(
                        cx.float_arg(1),
                        cx.float_arg(2),
                        cx.float_arg(3),
                        cx.float_arg(4),
                    ),
                );
                Ok(ScriptValue::Unit)
            }),
        f("bezier_to", "Cubic segment through two control points.")
            .param("id", T::Str)
            .param("c1x", T::Float)
            .param("c1y", T::Float)
            .param("c2x", T::Float)
            .param("c2y", T::Float)
            .param("x", T::Float)
            .param("y", T::Float)
            .build(|cx| {
                record(
                    cx.str_arg(0),
                    Op::BezierTo(
                        cx.float_arg(1),
                        cx.float_arg(2),
                        cx.float_arg(3),
                        cx.float_arg(4),
                        cx.float_arg(5),
                        cx.float_arg(6),
                    ),
                );
                Ok(ScriptValue::Unit)
            }),
        f("arc", "Circular arc, angles in radians.")
            .param("id", T::Str)
            .param("x", T::Float)
            .param("y", T::Float)
            .param("radius", T::Float)
            .param("start", T::Float)
            .param("end", T::Float)
            .build(|cx| {
                record(
                    cx.str_arg(0),
                    Op::Arc {
                        x: cx.float_arg(1),
                        y: cx.float_arg(2),
                        radius: cx.float_arg(3),
                        start: cx.float_arg(4),
                        end: cx.float_arg(5),
                    },
                );
                Ok(ScriptValue::Unit)
            }),
        rect("rect", "Add a closed rectangle to the path.", Op::Rect),
        bare(
            "close_path",
            "Close the current subpath back to its start.",
            Op::ClosePath,
        ),
        bare("fill", "Fill the current path.", Op::Fill),
        bare("stroke", "Stroke the current path.", Op::Stroke),
        rect(
            "fill_rect",
            "Fill one rectangle, leaving the path alone.",
            Op::FillRect,
        ),
        rect(
            "stroke_rect",
            "Stroke one rectangle, leaving the path alone.",
            Op::StrokeRect,
        ),
    ]
}

/// Colors, line style, alpha, and the state stack.
fn state_fns(f: Describe<'_>) -> Vec<ScriptFn> {
    let rgba = |name: &str, doc: &str, op: fn(Rgba) -> Op| {
        f(name, doc)
            .param("id", T::Str)
            .param("r", T::Float)
            .param("g", T::Float)
            .param("b", T::Float)
            .param("a", T::Float)
            .build(move |cx| {
                record(
                    cx.str_arg(0),
                    op(Rgba::new(
                        cx.float_arg(1),
                        cx.float_arg(2),
                        cx.float_arg(3),
                        cx.float_arg(4),
                    )),
                );
                Ok(ScriptValue::Unit)
            })
    };
    let style = |name: &str, doc: &str, op: fn(Rgba) -> Op| {
        f(name, doc)
            .param("id", T::Str)
            .param("color", T::Str)
            .ret(T::Bool)
            .build(move |cx| {
                let text = cx.str_arg(1);
                match color::parse_css(&text) {
                    Some(c) => {
                        record(cx.str_arg(0), op(c));
                        Ok(ScriptValue::Bool(true))
                    }
                    None => {
                        lumen_module::lumen_core::warn_line!(
                            "lumen-canvas: '{text}' is not a color this module understands; \
                             use a hex, rgb(), or rgba() value"
                        );
                        Ok(ScriptValue::Bool(false))
                    }
                }
            })
    };
    let bare = |name: &str, doc: &str, op: Op| {
        f(name, doc).param("id", T::Str).build(move |cx| {
            record(cx.str_arg(0), op.clone());
            Ok(ScriptValue::Unit)
        })
    };
    vec![
        rgba(
            "set_fill_rgba",
            "Set the fill color from four components, each 0..1.",
            Op::SetFill,
        ),
        style(
            "set_fill_style",
            "Set the fill color from CSS text; false when it is not understood.",
            Op::SetFill,
        ),
        rgba(
            "set_stroke_rgba",
            "Set the stroke color from four components, each 0..1.",
            Op::SetStroke,
        ),
        style(
            "set_stroke_style",
            "Set the stroke color from CSS text; false when it is not understood.",
            Op::SetStroke,
        ),
        f("set_line_width", "Set the stroke width, in canvas units.")
            .param("id", T::Str)
            .param("width", T::Float)
            .build(|cx| {
                record(cx.str_arg(0), Op::SetLineWidth(cx.float_arg(1)));
                Ok(ScriptValue::Unit)
            }),
        f(
            "set_line_cap",
            "Set the stroke's ends: butt, round, square.",
        )
        .param("id", T::Str)
        .param("cap", T::Str)
        .ret(T::Bool)
        .build(|cx| {
            let text = cx.str_arg(1);
            match LineCap::parse(&text) {
                Some(cap) => {
                    record(cx.str_arg(0), Op::SetLineCap(cap));
                    Ok(ScriptValue::Bool(true))
                }
                None => {
                    lumen_module::lumen_core::warn_line!(
                        "lumen-canvas: '{text}' is not a line cap; use butt, round, or square"
                    );
                    Ok(ScriptValue::Bool(false))
                }
            }
        }),
        f(
            "set_line_join",
            "Set the stroke's corners: miter, round, bevel.",
        )
        .param("id", T::Str)
        .param("join", T::Str)
        .ret(T::Bool)
        .build(|cx| {
            let text = cx.str_arg(1);
            match LineJoin::parse(&text) {
                Some(join) => {
                    record(cx.str_arg(0), Op::SetLineJoin(join));
                    Ok(ScriptValue::Bool(true))
                }
                None => {
                    lumen_module::lumen_core::warn_line!(
                        "lumen-canvas: '{text}' is not a line join; use miter, round, or bevel"
                    );
                    Ok(ScriptValue::Bool(false))
                }
            }
        }),
        f(
            "set_global_alpha",
            "Multiply every later draw by this alpha, 0..1.",
        )
        .param("id", T::Str)
        .param("alpha", T::Float)
        .build(|cx| {
            record(cx.str_arg(0), Op::SetGlobalAlpha(cx.float_arg(1)));
            Ok(ScriptValue::Unit)
        }),
        bare("save", "Push the drawing state.", Op::Save),
        bare("restore", "Pop the drawing state.", Op::Restore),
    ]
}

/// The transform every draw is placed by.
fn transform_fns(f: Describe<'_>) -> Vec<ScriptFn> {
    vec![
        f("translate", "Move the origin.")
            .param("id", T::Str)
            .param("x", T::Float)
            .param("y", T::Float)
            .build(|cx| {
                record(
                    cx.str_arg(0),
                    Op::Translate(cx.float_arg(1), cx.float_arg(2)),
                );
                Ok(ScriptValue::Unit)
            }),
        f("rotate", "Rotate the transform, in radians.")
            .param("id", T::Str)
            .param("radians", T::Float)
            .build(|cx| {
                record(cx.str_arg(0), Op::Rotate(cx.float_arg(1)));
                Ok(ScriptValue::Unit)
            }),
        f("scale", "Scale the transform.")
            .param("id", T::Str)
            .param("x", T::Float)
            .param("y", T::Float)
            .build(|cx| {
                record(cx.str_arg(0), Op::Scale(cx.float_arg(1), cx.float_arg(2)));
                Ok(ScriptValue::Unit)
            }),
        f("reset_transform", "Drop back to the identity transform.")
            .param("id", T::Str)
            .build(|cx| {
                record(cx.str_arg(0), Op::ResetTransform);
                Ok(ScriptValue::Unit)
            }),
        f("set_transform", "Replace the transform with a b c d e f.")
            .param("id", T::Str)
            .param("a", T::Float)
            .param("b", T::Float)
            .param("c", T::Float)
            .param("d", T::Float)
            .param("e", T::Float)
            .param("f", T::Float)
            .build(|cx| {
                record(
                    cx.str_arg(0),
                    Op::SetTransform([
                        cx.float_arg(1),
                        cx.float_arg(2),
                        cx.float_arg(3),
                        cx.float_arg(4),
                        cx.float_arg(5),
                        cx.float_arg(6),
                    ]),
                );
                Ok(ScriptValue::Unit)
            }),
    ]
}

/// Text, shaped with the app's own fonts.
fn text_fns(f: Describe<'_>) -> Vec<ScriptFn> {
    vec![
        f(
            "set_font",
            "Set the font as '[weight] <size>px [family]'; false when it is not understood.",
        )
        .param("id", T::Str)
        .param("font", T::Str)
        .ret(T::Bool)
        .build(|cx| {
            let text = cx.str_arg(1);
            match FontSpec::parse(&text) {
                Some(spec) => {
                    record(cx.str_arg(0), Op::SetFont(spec));
                    Ok(ScriptValue::Bool(true))
                }
                None => {
                    lumen_module::lumen_core::warn_line!(
                        "lumen-canvas: '{text}' is not a font; it needs a size, as in '16px'"
                    );
                    Ok(ScriptValue::Bool(false))
                }
            }
        }),
        f(
            "fill_text",
            "Draw text in the fill color, with (x, y) on the baseline.",
        )
        .param("id", T::Str)
        .param("text", T::Str)
        .param("x", T::Float)
        .param("y", T::Float)
        .build(|cx| {
            record(
                cx.str_arg(0),
                Op::FillText {
                    text: cx.str_arg(1),
                    x: cx.float_arg(2),
                    y: cx.float_arg(3),
                },
            );
            Ok(ScriptValue::Unit)
        }),
    ]
}

/// Pixel buffers: the read-write half.
fn buffer_fns(f: Describe<'_>) -> Vec<ScriptFn> {
    vec![
        f(
            "buffer_new",
            "Create a transparent pixel buffer; 0 when it was refused.",
        )
        .param("width", T::Int)
        .param("height", T::Int)
        .ret(T::Int)
        .build(|cx| {
            let width = u32::try_from(cx.int_arg(0)).unwrap_or(0);
            let height = u32::try_from(cx.int_arg(1)).unwrap_or(0);
            let handle = degrade(store::store().new_buffer(width, height), 0);
            Ok(ScriptValue::I64(i64::from(handle)))
        }),
        f(
            "buffer_free",
            "Release a buffer; false when it was unknown.",
        )
        .param("buffer", T::Int)
        .ret(T::Bool)
        .build(|cx| {
            let handle = u32::try_from(cx.int_arg(0)).unwrap_or(0);
            Ok(ScriptValue::Bool(
                store::store().buffers.remove(&handle).is_some(),
            ))
        }),
        f("buffer_width", "A buffer's width; 0 when it is unknown.")
            .param("buffer", T::Int)
            .ret(T::Int)
            .build(|cx| {
                Ok(ScriptValue::I64(with_buffer(cx.int_arg(0), 0, |b| {
                    i64::from(b.width())
                })))
            }),
        f("buffer_height", "A buffer's height; 0 when it is unknown.")
            .param("buffer", T::Int)
            .ret(T::Int)
            .build(|cx| {
                Ok(ScriptValue::I64(with_buffer(cx.int_arg(0), 0, |b| {
                    i64::from(b.height())
                })))
            }),
        f(
            "buffer_get_pixel",
            "One pixel as 0xRRGGBBAA; 0 outside the buffer.",
        )
        .param("buffer", T::Int)
        .param("x", T::Int)
        .param("y", T::Int)
        .ret(T::Int)
        .build(|cx| {
            let (x, y) = (cx.int_arg(1), cx.int_arg(2));
            Ok(ScriptValue::I64(with_buffer(cx.int_arg(0), 0, |b| {
                i64::from(b.get_pixel(x, y))
            })))
        }),
        f("buffer_set_pixel", "Write one pixel as 0xRRGGBBAA.")
            .param("buffer", T::Int)
            .param("x", T::Int)
            .param("y", T::Int)
            .param("rgba", T::Int)
            .build(|cx| {
                let (x, y, rgba) = (cx.int_arg(1), cx.int_arg(2), pixel_of(cx.int_arg(3)));
                with_buffer_mut(cx.int_arg(0), |b| b.set_pixel(x, y, rgba));
                Ok(ScriptValue::Unit)
            }),
        f(
            "buffer_get_region",
            "A rectangle of pixels, row-major, as 0xRRGGBBAA integers.",
        )
        .param("buffer", T::Int)
        .param("x", T::Int)
        .param("y", T::Int)
        .param("width", T::Int)
        .param("height", T::Int)
        .ret(T::Array(Box::new(T::Int)))
        .build(|cx| {
            let (x, y) = (cx.int_arg(1), cx.int_arg(2));
            let width = u32::try_from(cx.int_arg(3)).unwrap_or(0);
            let height = u32::try_from(cx.int_arg(4)).unwrap_or(0);
            let Some(()) = check_region(width, height) else {
                return Ok(ScriptValue::Array(Vec::new()));
            };
            let pixels = with_buffer(cx.int_arg(0), Vec::new(), |b| {
                b.get_region(x, y, width, height)
            });
            Ok(ScriptValue::Array(
                pixels
                    .into_iter()
                    .map(|p| ScriptValue::I64(i64::from(p)))
                    .collect(),
            ))
        }),
        f("buffer_put_region", "Write a rectangle of pixels back.")
            .param("buffer", T::Int)
            .param("x", T::Int)
            .param("y", T::Int)
            .param("width", T::Int)
            .param("height", T::Int)
            .param("pixels", T::Array(Box::new(T::Int)))
            .build(|cx| {
                let (x, y) = (cx.int_arg(1), cx.int_arg(2));
                let width = u32::try_from(cx.int_arg(3)).unwrap_or(0);
                let height = u32::try_from(cx.int_arg(4)).unwrap_or(0);
                let Some(()) = check_region(width, height) else {
                    return Ok(ScriptValue::Unit);
                };
                let pixels: Vec<u32> = match cx.arg_ref(5) {
                    ScriptValue::Array(items) => {
                        items.iter().map(|v| pixel_of(int_of(v))).collect()
                    }
                    _ => Vec::new(),
                };
                with_buffer_mut(cx.int_arg(0), |b| {
                    b.put_region(x, y, width, height, &pixels)
                });
                Ok(ScriptValue::Unit)
            }),
        f(
            "buffer_fill_rect",
            "Fill a rectangle of a buffer with one color.",
        )
        .param("buffer", T::Int)
        .param("x", T::Int)
        .param("y", T::Int)
        .param("width", T::Int)
        .param("height", T::Int)
        .param("rgba", T::Int)
        .build(|cx| {
            let (x, y) = (cx.int_arg(1), cx.int_arg(2));
            let width = u32::try_from(cx.int_arg(3)).unwrap_or(0);
            let height = u32::try_from(cx.int_arg(4)).unwrap_or(0);
            let rgba = pixel_of(cx.int_arg(5));
            let Some(()) = check_region(width, height) else {
                return Ok(ScriptValue::Unit);
            };
            with_buffer_mut(cx.int_arg(0), |b| b.fill_rect(x, y, width, height, rgba));
            Ok(ScriptValue::Unit)
        }),
        f(
            "buffer_load_png",
            "Read a PNG into a fresh buffer; 0 when it could not be read.",
        )
        .param("path", T::Str)
        .ret(T::Int)
        .build(|cx| {
            let path = resolve(cx.str_arg(0));
            let mut store = store::store();
            let cap = store.caps.buffer_pixels;
            let loaded = match PixBuf::load_png(&path, cap) {
                Ok(buf) => buf,
                Err(message) => {
                    lumen_module::lumen_core::warn_line!("lumen-canvas: {message}");
                    return Ok(ScriptValue::I64(0));
                }
            };
            let handle = degrade(store.new_buffer(loaded.width(), loaded.height()), 0);
            if handle != 0 {
                store.buffers.insert(handle, loaded);
            }
            Ok(ScriptValue::I64(i64::from(handle)))
        }),
        f("buffer_save_png", "Write a buffer out as a PNG.")
            .param("buffer", T::Int)
            .param("path", T::Str)
            .ret(T::Bool)
            .build(|cx| {
                let path = resolve(cx.str_arg(1));
                let store = store::store();
                let handle = u32::try_from(cx.int_arg(0)).unwrap_or(0);
                let Some(buffer) = store.buffers.get(&handle) else {
                    lumen_module::lumen_core::warn_line!(
                        "lumen-canvas: no buffer {handle} to save"
                    );
                    return Ok(ScriptValue::Bool(false));
                };
                Ok(ScriptValue::Bool(degrade(
                    buffer.save_png(&path).map(|()| true),
                    false,
                )))
            }),
        f(
            "draw_buffer",
            "Draw a buffer onto the canvas at its own size.",
        )
        .param("id", T::Str)
        .param("buffer", T::Int)
        .param("x", T::Float)
        .param("y", T::Float)
        .build(|cx| {
            record(
                cx.str_arg(0),
                Op::DrawBuffer {
                    buffer: u32::try_from(cx.int_arg(1)).unwrap_or(0),
                    x: cx.float_arg(2),
                    y: cx.float_arg(3),
                },
            );
            Ok(ScriptValue::Unit)
        }),
        f(
            "draw_buffer_scaled",
            "Draw a buffer onto the canvas stretched into a box.",
        )
        .param("id", T::Str)
        .param("buffer", T::Int)
        .param("x", T::Float)
        .param("y", T::Float)
        .param("width", T::Float)
        .param("height", T::Float)
        .build(|cx| {
            record(
                cx.str_arg(0),
                Op::DrawBufferScaled {
                    buffer: u32::try_from(cx.int_arg(1)).unwrap_or(0),
                    x: cx.float_arg(2),
                    y: cx.float_arg(3),
                    width: cx.float_arg(4),
                    height: cx.float_arg(5),
                },
            );
            Ok(ScriptValue::Unit)
        }),
    ]
}

/// Read something off a buffer, or answer the fallback when the handle names
/// nothing. A freed handle and an invented one read the same, on purpose.
fn with_buffer<T>(handle: i64, fallback: T, read: impl FnOnce(&PixBuf) -> T) -> T {
    let handle = u32::try_from(handle).unwrap_or(0);
    let store = store::store();
    match store.buffers.get(&handle) {
        Some(buffer) => read(buffer),
        None => fallback,
    }
}

/// Write to a buffer, or do nothing when the handle names none.
fn with_buffer_mut(handle: i64, write: impl FnOnce(&mut PixBuf)) {
    let handle = u32::try_from(handle).unwrap_or(0);
    let mut store = store::store();
    if let Some(buffer) = store.buffers.get_mut(&handle) {
        write(buffer);
    }
}

/// Whether a region is inside the cap, reporting it when it is not.
fn check_region(width: u32, height: u32) -> Option<()> {
    let cap = store::store().caps.region;
    let pixels = u64::from(width) * u64::from(height);
    if pixels > cap {
        lumen_module::lumen_core::warn_line!(
            "lumen-canvas: a {width}x{height} region is {pixels} pixels, over the {cap} the \
             app allows"
        );
        return None;
    }
    Some(())
}
