//! `animations::animate`: a script's own animation of an element, the Web
//! Animations API's `element.animate(keyframes, options)` in every host.
//!
//! A script describes the animation once and the runtime drives it; the
//! script controls it through the handle `animate` returns and hears it end
//! through the element's `animationend` and `animationcancel` events, whose
//! detail names the handle under `animation`. Nothing here runs per frame in
//! the script.
//!
//! The functions only queue what the script asked for and read back what the
//! last tick recorded: a driver applies the queue each tick. On the desktop
//! that is the keyframe engine; in a browser it is the page's own
//! `element.animate`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::Resource;
use lumen_script::{ScriptFn, ScriptNs, ScriptStruct, ScriptTy as T, ScriptValue};

/// The namespace the functions live in: `animations::animate` in candela
/// and Rhai, `animations.animate` in Lua.
pub const NAMESPACE: &str = "animations";

/// The struct the options argument is, `animations::AnimateOptions` in
/// candela.
pub const OPTIONS: &str = "AnimateOptions";

/// One keyframe a script asked for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptFrame {
    /// `(property, value)` pairs, browser CSS names as in a `@keyframes`
    /// body.
    pub declarations: Vec<(String, String)>,
    /// Where it sits, `0` to `1`; `None` spaces it evenly between its
    /// neighbours.
    pub offset: Option<f32>,
    /// The timing function to the next frame.
    pub easing: Option<String>,
}

/// The timing a script asked for, the Web Animations `KeyframeEffect`
/// options.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptTiming {
    /// One iteration, in milliseconds.
    pub duration_ms: f64,
    /// The wait before it starts, in milliseconds.
    pub delay_ms: f64,
    /// How many iterations; infinite runs forever.
    pub iterations: f64,
    /// `normal`, `reverse`, `alternate`, `alternate-reverse`.
    pub direction: String,
    /// `none`, `forwards`, `backwards`, `both`.
    pub fill: String,
    /// The timing function across each iteration.
    pub easing: String,
}

/// What a script asked of one of its animations.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    /// `play()`.
    Play,
    /// `pause()`.
    Pause,
    /// `reverse()`.
    Reverse,
    /// `finish()`.
    Finish,
    /// `cancel()`.
    Cancel,
    /// `set_current_time(ms)`.
    Seek(f64),
    /// `set_playback_rate(rate)`.
    Rate(f64),
}

/// One request queued for the driver.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Start a new animation.
    Animate {
        /// Its handle.
        id: u64,
        /// The element, packed the way `NodeHandle::pack` packs it.
        node: u64,
        /// Its keyframes.
        frames: Vec<ScriptFrame>,
        /// Its timing.
        timing: ScriptTiming,
    },
    /// Control one already started.
    Control {
        /// Its handle.
        id: u64,
        /// What to do.
        control: Control,
    },
}

/// What the last tick recorded about one animation, for the getters.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// Milliseconds into its timeline.
    pub current_time_ms: f64,
    /// Its playback rate.
    pub playback_rate: f64,
    /// `idle`, `running`, `paused` or `finished`.
    pub play_state: &'static str,
}

/// The queue between the script functions and the driver, and what the
/// driver last saw.
#[derive(Default)]
pub struct Bus {
    next: AtomicU64,
    requests: Mutex<Vec<Request>>,
    snapshots: Mutex<HashMap<u64, Snapshot>>,
}

impl Bus {
    /// Take every request queued since the last call.
    pub fn take(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Record what an animation shows now.
    pub fn record(&self, id: u64, snapshot: Snapshot) {
        self.snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, snapshot);
    }

    fn push(&self, request: Request) {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(request);
    }

    fn snapshot(&self, id: u64) -> Option<Snapshot> {
        self.snapshots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
    }
}

/// The [`Bus`] of an app, as a resource its driver reads.
#[derive(Resource, Clone, Default)]
pub struct ScriptAnimations(pub Arc<Bus>);

/// The options argument, described once for every host: a candela struct,
/// a Rhai or Lua map.
pub fn options_type() -> ScriptStruct {
    ScriptStruct::new(OPTIONS)
        .field_default("duration", T::Float, 0.0)
        .field_default("delay", T::Float, 0.0)
        .field_default("iterations", T::Float, 1.0)
        .field_default("direction", T::Str, "normal")
        .field_default("fill", T::Str, "none")
        .field_default("easing", T::Str, "linear")
}

fn number(v: Option<&ScriptValue>) -> Option<f64> {
    match v? {
        ScriptValue::F64(f) => Some(*f),
        ScriptValue::I64(i) => Some(*i as f64),
        ScriptValue::Str(s) => s.parse().ok(),
        _ => None,
    }
}

fn text(v: &ScriptValue) -> String {
    match v {
        ScriptValue::Str(s) => s.clone(),
        ScriptValue::F64(f) => f.to_string(),
        ScriptValue::I64(i) => i.to_string(),
        ScriptValue::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

/// Read the keyframes argument: a list of maps of property to value, where
/// `offset` and `easing` are the frame's own placement and timing.
pub fn frames_of(value: &ScriptValue) -> Result<Vec<ScriptFrame>, String> {
    let ScriptValue::Array(list) = value else {
        return Err("animations::animate: keyframes must be a list of maps".into());
    };
    if list.is_empty() {
        return Err("animations::animate: give at least one keyframe".into());
    }
    list.iter()
        .map(|frame| {
            let ScriptValue::Map(map) = frame else {
                return Err("animations::animate: each keyframe must be a map".to_string());
            };
            let mut out = ScriptFrame::default();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                let value = &map[key];
                match key.as_str() {
                    "offset" => {
                        let at = number(Some(value))
                            .ok_or("animations::animate: offset must be a number")?;
                        out.offset = Some(at as f32);
                    }
                    "easing" => out.easing = Some(text(value)),
                    name => out.declarations.push((name.to_string(), text(value))),
                }
            }
            Ok(out)
        })
        .collect()
}

/// Read the options argument.
pub fn timing_of(value: &ScriptValue) -> Result<ScriptTiming, String> {
    let ScriptValue::Map(map) = value else {
        return Err(format!("animations::animate: options must be an {OPTIONS}"));
    };
    let str_field = |key: &str, default: &str| {
        map.get(key)
            .map(text)
            .unwrap_or_else(|| default.to_string())
    };
    Ok(ScriptTiming {
        duration_ms: number(map.get("duration")).unwrap_or(0.0).max(0.0),
        delay_ms: number(map.get("delay")).unwrap_or(0.0),
        // A negative count is the `Infinity` of the Web Animations API, which
        // not every host can write.
        iterations: match number(map.get("iterations")).unwrap_or(1.0) {
            n if n < 0.0 => f64::INFINITY,
            n => n,
        },
        direction: str_field("direction", "normal"),
        fill: str_field("fill", "none"),
        easing: str_field("easing", "linear"),
    })
}

/// The functions, bound to `bus`.
pub fn script_fns(bus: Arc<Bus>) -> Vec<ScriptFn> {
    let ns = || ScriptNs::Named(NAMESPACE.to_string());
    let control = |name: &str, doc: &str, make: fn(&lumen_script::ScriptFnCx<'_>) -> Control| {
        let bus = bus.clone();
        ScriptFn::new(name)
            .ns(ns())
            .doc(doc)
            .param("animation", T::Int)
            .ret(T::Unit)
            .build(move |cx| {
                bus.push(Request::Control {
                    id: cx.int_arg(0) as u64,
                    control: make(cx),
                });
                Ok(ScriptValue::Unit)
            })
    };
    let read = |name: &str, doc: &str, ret: T, get: fn(Option<Snapshot>) -> ScriptValue| {
        let bus = bus.clone();
        ScriptFn::new(name)
            .ns(ns())
            .doc(doc)
            .param("animation", T::Int)
            .ret(ret)
            .build(move |cx| Ok(get(bus.snapshot(cx.int_arg(0) as u64))))
    };
    let animate = {
        let bus = bus.clone();
        ScriptFn::new("animate")
            .ns(ns())
            .doc(
                "Animate the element with the id `id` through `keyframes`, a list of maps of \
                 CSS property to value, timed by `options`. Returns the animation's handle.",
            )
            .param("id", T::Str)
            .param("keyframes", T::Array(Box::new(T::Map(Box::new(T::Str)))))
            .param("options", T::Struct(options_type()))
            .ret(T::Int)
            .build(move |cx| {
                let element = cx.str_arg(0);
                let node = lumen_script::node_query::run_get_by_id(&element).ok_or_else(|| {
                    format!("animations::animate: no element has the id '{element}'")
                })?;
                let frames = frames_of(cx.arg_ref(1))?;
                let timing = timing_of(cx.arg_ref(2))?;
                let id = bus.next.fetch_add(1, Ordering::Relaxed) + 1;
                bus.record(
                    id,
                    Snapshot {
                        current_time_ms: 0.0,
                        playback_rate: 1.0,
                        play_state: "running",
                    },
                );
                bus.push(Request::Animate {
                    id,
                    node,
                    frames,
                    timing,
                });
                Ok(ScriptValue::I64(id as i64))
            })
    };
    vec![
        animate,
        control(
            "play",
            "Play the animation, or start it over once it has finished.",
            |_| Control::Play,
        ),
        control("pause", "Hold the animation where it is.", |_| {
            Control::Pause
        }),
        control(
            "reverse",
            "Play the animation the other way from where it is.",
            |_| Control::Reverse,
        ),
        control("finish", "Jump to the animation's end.", |_| {
            Control::Finish
        }),
        control(
            "cancel",
            "Stop the animation and take its values off the element.",
            |_| Control::Cancel,
        ),
        {
            let bus = bus.clone();
            ScriptFn::new("set_current_time")
                .ns(ns())
                .doc("Seek the animation to this many milliseconds into its timeline.")
                .param("animation", T::Int)
                .param("ms", T::Float)
                .ret(T::Unit)
                .build(move |cx| {
                    bus.push(Request::Control {
                        id: cx.int_arg(0) as u64,
                        control: Control::Seek(cx.float_arg(1)),
                    });
                    Ok(ScriptValue::Unit)
                })
        },
        {
            let bus = bus.clone();
            ScriptFn::new("set_playback_rate")
                .ns(ns())
                .doc("Play the animation at this rate: 2 is twice as fast, -1 backwards.")
                .param("animation", T::Int)
                .param("rate", T::Float)
                .ret(T::Unit)
                .build(move |cx| {
                    bus.push(Request::Control {
                        id: cx.int_arg(0) as u64,
                        control: Control::Rate(cx.float_arg(1)),
                    });
                    Ok(ScriptValue::Unit)
                })
        },
        read(
            "current_time",
            "Milliseconds into the animation's timeline, as of the last frame.",
            T::Float,
            |s| ScriptValue::F64(s.map_or(0.0, |s| s.current_time_ms)),
        ),
        read(
            "playback_rate",
            "The rate the animation plays at.",
            T::Float,
            |s| ScriptValue::F64(s.map_or(1.0, |s| s.playback_rate)),
        ),
        read(
            "play_state",
            "`running`, `paused`, `finished`, or `idle` once cancelled.",
            T::Str,
            |s| ScriptValue::Str(s.map_or("idle", |s| s.play_state).to_string()),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_and_options_read_from_script_values() {
        let frames = ScriptValue::Array(vec![
            ScriptValue::Map(HashMap::from([
                ("opacity".to_string(), ScriptValue::Str("0".into())),
                (
                    "transform".to_string(),
                    ScriptValue::Str("scale(0.9)".into()),
                ),
            ])),
            ScriptValue::Map(HashMap::from([
                ("opacity".to_string(), ScriptValue::Str("1".into())),
                ("offset".to_string(), ScriptValue::Str("1".into())),
                ("easing".to_string(), ScriptValue::Str("ease-out".into())),
            ])),
        ]);
        let read = frames_of(&frames).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].declarations.len(), 2);
        assert_eq!(read[1].offset, Some(1.0));
        assert_eq!(read[1].easing.as_deref(), Some("ease-out"));
        let timing = timing_of(&ScriptValue::Map(HashMap::from([
            ("duration".to_string(), ScriptValue::F64(300.0)),
            ("iterations".to_string(), ScriptValue::F64(-1.0)),
        ])))
        .unwrap();
        assert_eq!(timing.duration_ms, 300.0);
        assert!(timing.iterations.is_infinite());
        assert_eq!(timing.fill, "none");
        assert!(frames_of(&ScriptValue::Array(vec![])).is_err());
    }
}
