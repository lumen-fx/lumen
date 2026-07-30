//! Typed, whitelisted component-introspection registry for the dynamic
//! DOM `n.components()` / `n.component("LayoutBox")` reads (design 4.7).
//!
//! Each exposable Lumen component registers a reader that turns its public
//! fields into a `(name, value)` string map. The read is bounded and typed
//! -- there is no raw transmute or arbitrary memory access, and a name that
//! is not in the registry is an error rather than an empty read. The
//! runtime runs the registry against every element each tick and publishes
//! the resulting maps into the cross-thread snapshot the script hosts read.

use crate::components::{
    Fill, InlineStyle, LumenAttributes, LumenClasses, Opacity, Style, TextContent, Transform,
    Visible, Visuals, ZIndex,
};
use bevy_ecs::world::EntityRef;

/// A field map for one component instance: `(field, value)` pairs.
pub type ComponentValueMap = Vec<(String, String)>;

/// Reader for one exposable component: `Some(map)` when the entity carries
/// it, `None` when absent.
pub type ComponentReader = fn(EntityRef) -> Option<ComponentValueMap>;

/// The whitelist of exposable components and their field readers. Built
/// once by the runtime (see `with_defaults`) and consulted per element.
pub struct ComponentIntrospection {
    readers: Vec<(&'static str, ComponentReader)>,
}

impl ComponentIntrospection {
    /// The starter whitelist: geometry, paint, text, identity, and the
    /// generic attribute / inline-style maps.
    pub fn with_defaults() -> Self {
        Self {
            readers: vec![
                ("LayoutBox", read_layout_box),
                ("Visuals", read_visuals),
                ("Opacity", read_opacity),
                ("ZIndex", read_z_index),
                ("Visible", read_visible),
                ("TextContent", read_text_content),
                ("LumenClasses", read_classes),
                ("LumenAttributes", read_attributes),
                ("InlineStyle", read_inline_style),
                ("Style", read_style),
            ],
        }
    }

    /// The names of every whitelisted component, in registry order.
    pub fn names(&self) -> Vec<&'static str> {
        self.readers.iter().map(|(n, _)| *n).collect()
    }

    /// Whether `name` is a whitelisted component.
    pub fn is_known(&self, name: &str) -> bool {
        self.readers.iter().any(|(n, _)| *n == name)
    }

    /// Every whitelisted component present on `entity`, with its field map.
    pub fn read_all(&self, entity: EntityRef) -> Vec<(String, ComponentValueMap)> {
        self.readers
            .iter()
            .filter_map(|(name, reader)| reader(entity).map(|map| (name.to_string(), map)))
            .collect()
    }
}

fn read_layout_box(e: EntityRef) -> Option<ComponentValueMap> {
    let t = e.get::<Transform>()?;
    let mut m = vec![
        ("x".into(), t.absolute.x.to_string()),
        ("y".into(), t.absolute.y.to_string()),
        ("width".into(), t.size.x.to_string()),
        ("height".into(), t.size.y.to_string()),
    ];
    if let Some(b) = t.baseline_y {
        m.push(("baseline_y".into(), b.to_string()));
    }
    Some(m)
}

fn hex(c: &crate::components::Color) -> String {
    let [r, g, b, a] = c.to_rgba8();
    if a == 0xff {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

fn read_visuals(e: EntityRef) -> Option<ComponentValueMap> {
    let v = e.get::<Visuals>()?;
    let mut m = vec![("radius".into(), v.radius.to_string())];
    match &v.fill {
        Some(Fill::Solid(c)) => m.push(("fill".into(), hex(c))),
        Some(_) => m.push(("fill".into(), "gradient".into())),
        None => {}
    }
    if let Some(border) = &v.border {
        m.push(("border_color".into(), hex(&border.color)));
        m.push(("border_width".into(), border.widths.top.to_string()));
    }
    m.push(("shadows".into(), v.shadows.len().to_string()));
    Some(m)
}

fn read_opacity(e: EntityRef) -> Option<ComponentValueMap> {
    let o = e.get::<Opacity>()?;
    Some(vec![("value".into(), o.0.to_string())])
}

fn read_z_index(e: EntityRef) -> Option<ComponentValueMap> {
    let z = e.get::<ZIndex>()?;
    Some(vec![("value".into(), z.0.to_string())])
}

fn read_visible(e: EntityRef) -> Option<ComponentValueMap> {
    let v = e.get::<Visible>()?;
    Some(vec![("value".into(), v.0.to_string())])
}

fn read_text_content(e: EntityRef) -> Option<ComponentValueMap> {
    let t = e.get::<TextContent>()?;
    Some(vec![("text".into(), t.0.clone())])
}

fn read_classes(e: EntityRef) -> Option<ComponentValueMap> {
    let c = e.get::<LumenClasses>()?;
    Some(vec![(
        "classes".into(),
        c.0.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(" "),
    )])
}

fn read_attributes(e: EntityRef) -> Option<ComponentValueMap> {
    let a = e.get::<LumenAttributes>()?;
    let mut m: ComponentValueMap = a.0.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    m.sort();
    Some(m)
}

fn read_inline_style(e: EntityRef) -> Option<ComponentValueMap> {
    let s = e.get::<InlineStyle>()?;
    Some(s.0.clone())
}

fn read_style(e: EntityRef) -> Option<ComponentValueMap> {
    let s = e.get::<Style>()?;
    Some(vec![
        ("display".into(), format!("{:?}", s.display)),
        ("width".into(), format!("{:?}", s.width)),
        ("height".into(), format!("{:?}", s.height)),
    ])
}
