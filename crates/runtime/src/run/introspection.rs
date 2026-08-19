//! Publishing the tree for a reader: the per-node detail snapshot the
//! scripting and C-ABI surfaces read through, and the low-level introspection
//! the devtools and MCP inspector read.
//!
//! Both run each tick alongside the DOM index publish, so a read issued from
//! an event handler sees this tick's state. They are here rather than with the
//! rest of the DOM pipeline because what they publish is the desktop's answer:
//! geometry comes from a layout Lumen ran, and the cascade inputs come from
//! the stylesheet Lumen resolved. A page has neither, and its own engine
//! answers both questions.

use super::*;
use bevy_ecs::hierarchy::ChildOf;
use lumen_core::components::{InlineStyle, LumenAttributes, TextContent};
use lumen_script::node_query::{DomDetails, NodeDetail, publish_dom_details};
use std::collections::HashMap;

use crate::run::restyle::{LastMediaContext, RuntimeStylesheet};

/// Monotonic frame counter + previous-tick timestamp backing
/// `frame_info()`. Advanced by [`publish_introspection`].
#[derive(Resource)]
pub(crate) struct FrameClock {
    frame: u64,
    last: std::time::Instant,
}

impl Default for FrameClock {
    fn default() -> Self {
        Self {
            frame: 0,
            last: std::time::Instant::now(),
        }
    }
}

/// Publish the phase-5 low-level introspection snapshot: post-layout
/// geometry (rect / content_rect / scroll / visibility / z-index), typed
/// component field maps, pointer / frame state, and the signal set. Runs
/// each tick alongside the [`DomIndex`] publish so a read issued from a
/// handler sees this tick's tree. Geometry reflects the last committed
/// layout (this tick's `sync_layout` runs later in `LayoutSync`), matching
/// `computed_style`'s one-tick lag. An inspection pass, not a render path.
#[allow(clippy::type_complexity)]
pub(crate) fn publish_introspection(world: &mut World) {
    use lumen_core::components::{
        DirtyLayout, Display, LumenTag, Style, Transform, Visible, Visuals, ZIndex,
    };
    use lumen_core::input::{ModifiersState, PointerState, ScrollOffset};
    use lumen_core::introspect::ComponentIntrospection;
    use lumen_script::introspect::{
        FrameInfo, IntrospectSnapshot, NodeGeometry, NodeRect, NodeScroll, PointerSnapshot,
        publish_introspection as publish,
    };
    use std::collections::HashMap;

    let registry = ComponentIntrospection::with_defaults();
    let known = registry.names().iter().map(|s| s.to_string()).collect();

    // Absolute transform + parent lookups, computed once.
    let transforms: HashMap<Entity, (glam::Vec2, glam::Vec2)> = {
        let mut q = world.query::<(Entity, &Transform)>();
        q.iter(world)
            .map(|(e, t)| (e, (t.absolute, t.size)))
            .collect()
    };
    let parents: HashMap<Entity, Entity> = {
        let mut q = world.query::<(Entity, &ChildOf)>();
        q.iter(world).map(|(e, c)| (e, c.parent())).collect()
    };
    let mut children_of: HashMap<Entity, Vec<Entity>> = HashMap::new();
    for (child, parent) in &parents {
        children_of.entry(*parent).or_default().push(*child);
    }

    // Raw per-element geometry inputs, for indexed (tagged) elements only.
    struct Raw {
        entity: Entity,
        abs: glam::Vec2,
        size: glam::Vec2,
        pad: (f32, f32, f32, f32),
        border: (f32, f32, f32, f32),
        display_none: bool,
        visible: bool,
        z: i32,
        scroll_off: Option<glam::Vec2>,
    }
    let raws: Vec<Raw> = {
        let mut q = world.query_filtered::<(
            Entity,
            &Transform,
            Option<&Style>,
            Option<&Visuals>,
            Option<&ZIndex>,
            Option<&Visible>,
            Option<&ScrollOffset>,
        ), With<LumenTag>>();
        q.iter(world)
            .map(|(e, t, style, visuals, z, vis, scroll)| {
                let pad = style
                    .map(|s| {
                        (
                            s.padding.left,
                            s.padding.right,
                            s.padding.top,
                            s.padding.bottom,
                        )
                    })
                    .unwrap_or((0.0, 0.0, 0.0, 0.0));
                let border = visuals
                    .and_then(|v| v.border.as_ref())
                    .map(|b| (b.widths.left, b.widths.right, b.widths.top, b.widths.bottom))
                    .unwrap_or((0.0, 0.0, 0.0, 0.0));
                Raw {
                    entity: e,
                    abs: t.absolute,
                    size: t.size,
                    pad,
                    border,
                    display_none: style.map(|s| s.display == Display::None).unwrap_or(false),
                    visible: vis.map(|v| v.0).unwrap_or(true),
                    z: z.map(|z| z.0).unwrap_or(0),
                    scroll_off: scroll.map(|s| s.0),
                }
            })
            .collect()
    };

    let mut geometry: HashMap<u64, NodeGeometry> = HashMap::with_capacity(raws.len());
    for r in &raws {
        let handle = lumen_core::node::NodeHandle::new(r.entity).pack();
        let parent_abs = parents
            .get(&r.entity)
            .and_then(|p| transforms.get(p))
            .map(|(a, _)| *a)
            .unwrap_or(glam::Vec2::ZERO);
        let local = r.abs - parent_abs;
        let rect = NodeRect {
            x: local.x,
            y: local.y,
            width: r.size.x,
            height: r.size.y,
            client_x: r.abs.x,
            client_y: r.abs.y,
        };
        let (pl, pr, pt, pb) = r.pad;
        let (bl, br, bt, bb) = r.border;
        let inset_x = pl + bl;
        let inset_y = pt + bt;
        let content_rect = NodeRect {
            x: local.x + inset_x,
            y: local.y + inset_y,
            width: (r.size.x - pl - pr - bl - br).max(0.0),
            height: (r.size.y - pt - pb - bt - bb).max(0.0),
            client_x: r.abs.x + inset_x,
            client_y: r.abs.y + inset_y,
        };
        // Scroll extent from the bbox of direct children relative to self
        // (the same rule `clamp_scroll_offsets` applies).
        let scroll = match r.scroll_off {
            Some(off) => {
                let mut ext = glam::Vec2::ZERO;
                if let Some(kids) = children_of.get(&r.entity) {
                    for kid in kids {
                        if let Some((kabs, ksize)) = transforms.get(kid) {
                            let rel = (*kabs - r.abs) + *ksize;
                            ext = ext.max(rel);
                        }
                    }
                }
                NodeScroll {
                    x: off.x,
                    y: off.y,
                    max_x: (ext.x - r.size.x).max(0.0),
                    max_y: (ext.y - r.size.y).max(0.0),
                }
            }
            None => NodeScroll::default(),
        };
        geometry.insert(
            handle,
            NodeGeometry {
                rect,
                content_rect,
                scroll,
                visible: r.visible && !r.display_none,
                z_index: r.z,
            },
        );
    }

    // Component field maps via the whitelist registry.
    let mut components: HashMap<u64, Vec<(String, Vec<(String, String)>)>> =
        HashMap::with_capacity(raws.len());
    for r in &raws {
        let handle = lumen_core::node::NodeHandle::new(r.entity).pack();
        let maps = registry.read_all(world.entity(r.entity));
        if !maps.is_empty() {
            components.insert(handle, maps);
        }
    }

    // Pointer state.
    let pointer = {
        let ps = world
            .get_resource::<PointerState>()
            .copied()
            .unwrap_or_default();
        let m = world
            .get_resource::<ModifiersState>()
            .map(|m| m.0)
            .unwrap_or_default();
        let pos = ps.position.unwrap_or(glam::Vec2::ZERO);
        PointerSnapshot {
            x: pos.x,
            y: pos.y,
            inside: ps.position.is_some(),
            buttons: u32::from(ps.primary_down),
            shift: m.shift,
            ctrl: m.ctrl,
            alt: m.alt,
            super_: m.super_,
        }
    };

    // Signal set (global scalar cells).
    let signals: Vec<(String, String)> = world
        .get_resource::<lumen_core::property_store::PropertyStore>()
        .map(|store| {
            let mut out: Vec<(String, String)> = store
                .iter()
                .filter_map(|(k, v)| match k {
                    lumen_core::property_store::PropertyKey::Global(name) => {
                        Some((name.to_string(), property_value_string(v)))
                    }
                    _ => None,
                })
                .collect();
            out.sort();
            out
        })
        .unwrap_or_default();

    // Frame counters.
    let dirty_count = {
        let mut q = world.query_filtered::<Entity, With<DirtyLayout>>();
        q.iter(world).count() as u64
    };
    let frame = {
        let now = std::time::Instant::now();
        let mut clock = world.get_resource_or_insert_with(FrameClock::default);
        let dt_ms = now.duration_since(clock.last).as_secs_f64() * 1000.0;
        clock.frame = clock.frame.wrapping_add(1);
        clock.last = now;
        FrameInfo {
            frame: clock.frame,
            dt_ms,
            dirty_count,
        }
    };

    publish(IntrospectSnapshot::new(
        geometry, components, known, pointer, frame, signals,
    ));
}

/// Render a scalar [`PropertyValue`](lumen_core::property_store::PropertyValue)
/// as a display string for `signals_all()`. Colors render as hex, vectors
/// as `x,y`; the enumerated scalars use their natural form.
fn property_value_string(v: &lumen_core::property_store::PropertyValue) -> String {
    use lumen_core::property_store::PropertyValue as V;
    match v {
        V::Bool(b) => b.to_string(),
        V::I64(n) => n.to_string(),
        V::F64(n) => n.to_string(),
        V::Str(s) => s.to_string(),
        V::Color(c) => {
            let [r, g, b, a] = c.to_rgba8();
            if a == 0xff {
                format!("#{r:02x}{g:02x}{b:02x}")
            } else {
                format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
            }
        }
        V::Vec2(p) => format!("{},{}", p.x, p.y),
        V::Custom(_) => String::new(),
    }
}

/// Publish the per-node detail snapshot (text / generic attrs / inline
/// style) plus the cascade inputs `computed_style` needs. Runs each tick
/// alongside the [`DomIndex`] publish so a read issued from a handler sees
/// this tick's state.
#[allow(clippy::type_complexity)]
pub(crate) fn publish_node_details(
    nodes: Query<(
        Entity,
        Option<&TextContent>,
        Option<&LumenAttributes>,
        Option<&InlineStyle>,
    )>,
    focused: Query<Entity, With<lumen_core::input::Focused>>,
    hovered: Query<Entity, With<lumen_core::input::Hovered>>,
    sheet: Option<Res<RuntimeStylesheet>>,
    media: Option<Res<LastMediaContext>>,
) {
    let pack = |e: Entity| lumen_core::node::NodeHandle::new(e).pack();
    lumen_script::node_query::publish_focus(
        focused.iter().next().map(pack),
        hovered.iter().next().map(pack),
    );
    let mut map: HashMap<u64, NodeDetail> = HashMap::new();
    for (entity, text, attrs, inline) in nodes.iter() {
        let detail = NodeDetail {
            text: text.map(|t| t.0.clone()),
            attributes: attrs
                .map(|a| a.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            inline_style: inline.map(|s| s.0.clone()).unwrap_or_default(),
        };
        map.insert(entity.to_bits(), detail);
    }
    let sheet = sheet.map(|s| std::sync::Arc::new(s.0.clone()));
    let media = media.map(|m| m.0).unwrap_or_default();
    publish_dom_details(DomDetails::new(map, sheet, media));
}
