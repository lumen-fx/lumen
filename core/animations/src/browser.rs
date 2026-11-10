//! `animations::animate` in a browser: the page plays a script's animation
//! with its own `element.animate`, and this answers the script's handle
//! from what the page reports. No keyframe engine runs here.

use std::collections::HashMap;

use bevy_ecs::message::{MessageRegistry, MessageWriter, Messages};
use bevy_ecs::prelude::*;
use lumen_core::node::NodeHandle;
use lumen_core::prelude::{App, Plugin, TickStage};
use lumen_script::{CustomEvent, ScriptFnAppExt};
use lumen_web_dom::NodeTable;
use wasm_bindgen::{JsCast, JsValue};

use crate::script::{Control, Request, ScriptAnimations, ScriptFrame, ScriptTiming, Snapshot};

/// Plugin: the `animations::` functions, played by the page.
pub struct BrowserAnimationsPlugin;

impl Plugin for BrowserAnimationsPlugin {
    fn build(self, app: &mut App) {
        if !app.world.contains_resource::<Messages<CustomEvent>>() {
            MessageRegistry::register_message::<CustomEvent>(&mut app.world);
        }
        let bus = ScriptAnimations::default();
        app.add_script_fns(crate::script::script_fns(bus.0.clone()));
        app.world.insert_resource(bus);
        app.world.insert_non_send(PageAnimations::default());
        app.add_systems(TickStage::Systems, drive_page_animations);
    }
}

/// The page's `Animation` object behind each handle, with its element and
/// the play state last seen.
#[derive(Default)]
struct PageAnimations(HashMap<u64, (Entity, JsValue, String)>);

fn get(of: &JsValue, key: &str) -> Option<JsValue> {
    js_sys::Reflect::get(of, &key.into()).ok()
}

fn set(of: &JsValue, key: &str, value: JsValue) {
    let _ = js_sys::Reflect::set(of, &key.into(), &value);
}

fn call(of: &JsValue, method: &str, args: &[JsValue]) -> Option<JsValue> {
    let f = get(of, method)?.dyn_into::<js_sys::Function>().ok()?;
    let list: js_sys::Array = args.iter().collect();
    f.apply(of, &list).ok()
}

/// A CSS property name as a script in a page spells it: `background-color`
/// is `backgroundColor`.
fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// A timing function as a browser reads it: a `spring()` becomes the
/// `linear()` curve it traces, the one the desktop plays, with the
/// milliseconds it takes to settle.
fn browser_easing(easing: &str) -> (String, Option<u32>) {
    let Some(inside) = easing
        .trim()
        .strip_prefix("spring(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return (easing.to_string(), None);
    };
    let nums: Vec<f32> = inside
        .split([',', ' '])
        .filter_map(|t| t.trim().parse().ok())
        .collect();
    let at = |i: usize, default: f32| nums.get(i).copied().unwrap_or(default);
    let spring = lumen_ir::layout_ir::EasingIr::Spring {
        mass: at(0, 1.0),
        stiffness: at(1, 100.0),
        damping: at(2, 10.0),
        velocity: at(3, 0.0),
    };
    match spring.spring_as_linear() {
        Some((curve, ms)) => (curve, Some(ms)),
        None => (easing.to_string(), None),
    }
}

fn keyframes_js(frames: &[ScriptFrame]) -> JsValue {
    let list = js_sys::Array::new();
    for frame in frames {
        let object = js_sys::Object::new();
        for (name, value) in &frame.declarations {
            set(&object, &camel(name), value.into());
        }
        if let Some(offset) = frame.offset {
            set(&object, "offset", f64::from(offset).into());
        }
        if let Some(easing) = &frame.easing {
            set(&object, "easing", browser_easing(easing).0.into());
        }
        list.push(&object);
    }
    list.into()
}

fn options_js(t: &ScriptTiming) -> JsValue {
    let object = js_sys::Object::new();
    let (easing, settle) = browser_easing(&t.easing);
    let duration = match settle {
        Some(ms) if t.duration_ms <= 0.0 => f64::from(ms),
        _ => t.duration_ms,
    };
    set(&object, "duration", duration.into());
    set(&object, "delay", t.delay_ms.into());
    set(&object, "iterations", t.iterations.into());
    set(&object, "direction", t.direction.as_str().into());
    set(&object, "fill", t.fill.as_str().into());
    set(&object, "easing", easing.into());
    object.into()
}

fn event(target: Entity, name: &str, id: u64, elapsed_ms: f64) -> CustomEvent {
    CustomEvent::new(target, name)
        .with("animation_name", String::new())
        .with("animation", id as i64)
        .with("elapsed_time", elapsed_ms.max(0.0) / 1000.0)
        .with("pseudo_element", String::new())
}

/// Start what the scripts asked for on the page, pass their controls to the
/// page's animations, and read back what the page reports, raising the end
/// and the cancel of each as the desktop does.
fn drive_page_animations(
    table: NonSend<NodeTable>,
    mut page: NonSendMut<PageAnimations>,
    bus: Res<ScriptAnimations>,
    entities: &bevy_ecs::entity::Entities,
    mut events: MessageWriter<CustomEvent>,
) {
    for request in bus.0.take() {
        match request {
            Request::Animate {
                id,
                node,
                frames,
                timing,
            } => {
                let Some(entity) = NodeHandle::unpack(node).and_then(|h| h.validate(entities))
                else {
                    continue;
                };
                let Some(element) = table.element(entity) else {
                    continue;
                };
                let Some(animation) = call(
                    element.as_ref(),
                    "animate",
                    &[keyframes_js(&frames), options_js(&timing)],
                ) else {
                    continue;
                };
                page.0
                    .insert(id, (entity, animation, "running".to_string()));
            }
            Request::Control { id, control } => {
                let Some((entity, animation, _)) = page.0.get(&id) else {
                    continue;
                };
                match control {
                    Control::Play => {
                        call(animation, "play", &[]);
                    }
                    Control::Pause => {
                        call(animation, "pause", &[]);
                    }
                    Control::Reverse => {
                        call(animation, "reverse", &[]);
                    }
                    Control::Finish => {
                        call(animation, "finish", &[]);
                    }
                    Control::Seek(ms) => set(animation, "currentTime", ms.into()),
                    Control::Rate(rate) => {
                        call(animation, "updatePlaybackRate", &[rate.into()]);
                    }
                    Control::Cancel => {
                        let at = get(animation, "currentTime")
                            .and_then(|t| t.as_f64())
                            .unwrap_or(0.0);
                        call(animation, "cancel", &[]);
                        events.write(event(*entity, "animationcancel", id, at));
                        bus.0.record(
                            id,
                            Snapshot {
                                current_time_ms: 0.0,
                                playback_rate: 1.0,
                                play_state: "idle",
                            },
                        );
                        page.0.remove(&id);
                    }
                }
            }
        }
    }
    for (id, (entity, animation, last)) in page.0.iter_mut() {
        let state = get(animation, "playState")
            .and_then(|s| s.as_string())
            .unwrap_or_default();
        let at = get(animation, "currentTime")
            .and_then(|t| t.as_f64())
            .unwrap_or(0.0);
        let rate = get(animation, "playbackRate")
            .and_then(|r| r.as_f64())
            .unwrap_or(1.0);
        if state == "finished" && *last != "finished" {
            events.write(event(*entity, "animationend", *id, at));
        }
        bus.0.record(
            *id,
            Snapshot {
                current_time_ms: at,
                playback_rate: rate,
                play_state: match state.as_str() {
                    "paused" => "paused",
                    "finished" => "finished",
                    "idle" => "idle",
                    _ => "running",
                },
            },
        );
        *last = state;
    }
}
