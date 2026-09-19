//! The encoded form of the script surface.
//!
//! A script host does not have to live in the engine's address space. When it
//! does not, the values it is called with, the commands it emits, and the
//! signatures it was described with all travel as bytes, so their encoding is
//! part of the contract rather than an implementation detail.
//!
//! Most types derive their encoding. The two that cannot are
//! [`PropertyKey`](lumen_core::property_store::PropertyKey) and
//! [`PropertyValue`](lumen_core::property_store::PropertyValue), which
//! [`ScriptCommand::SetProperty`](crate::ScriptCommand::SetProperty) carries:
//! they are storage types, and the adapters live here so the storage layer
//! stays free of an encoding it has no other use for.

/// Encoding version of the script surface.
///
/// Covers [`ScriptValue`](crate::ScriptValue),
/// [`ScriptCommand`](crate::ScriptCommand),
/// [`FileDialogKind`](crate::FileDialogKind),
/// [`ScriptTy`](crate::script_fn::ScriptTy),
/// [`ScriptParam`](crate::script_fn::ScriptParam),
/// [`ScriptSig`](crate::script_fn::ScriptSig),
/// [`ScriptNs`](crate::script_fn::ScriptNs),
/// [`HostSet`](crate::script_fn::HostSet),
/// [`ScriptPrelude`](crate::script_fn::ScriptPrelude), and [`PluginEvent`].
///
/// Enum variants are append-only. The encoding writes a variant by its index,
/// so inserting or reordering one silently reinterprets every variant after it
/// as a different command. Add new variants at the end of their enum. Any other
/// change to a shape listed above (a renamed or retyped field, a removed
/// variant) bumps this constant.
pub const SCRIPT_WIRE_VERSION: u16 = 2;

/// [`PropertyKey`](lumen_core::property_store::PropertyKey) on the wire.
///
/// An entity travels as its packed bits; a value naming an entity that cannot
/// exist is rejected rather than reconstructed.
pub mod property_key {
    use bevy_ecs::prelude::Entity;
    use lumen_core::property_store::PropertyKey;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    enum KeyWire {
        Global(String),
        Entity(u64, String),
    }

    /// Encode a key.
    pub fn serialize<S: Serializer>(key: &PropertyKey, s: S) -> Result<S::Ok, S::Error> {
        let wire = match key {
            PropertyKey::Global(name) => KeyWire::Global(name.to_string()),
            PropertyKey::Entity(entity, name) => {
                KeyWire::Entity(entity.to_bits(), name.to_string())
            }
        };
        wire.serialize(s)
    }

    /// Decode a key.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PropertyKey, D::Error> {
        match KeyWire::deserialize(d)? {
            KeyWire::Global(name) => Ok(PropertyKey::Global(name.into())),
            KeyWire::Entity(bits, name) => {
                let entity = Entity::try_from_bits(bits)
                    .ok_or_else(|| D::Error::custom(format!("not an entity id: {bits}")))?;
                Ok(PropertyKey::Entity(entity, name.into()))
            }
        }
    }
}

/// [`PropertyValue`](lumen_core::property_store::PropertyValue) on the wire.
///
/// The enumerated variants carry plain data.
/// [`Custom`](lumen_core::property_store::PropertyValue::Custom) holds an
/// `Arc<dyn Any>`, which has no encoding at all. A command carrying one fails
/// to encode and says why; dropping the value instead would hand the far side a
/// write that looks like it happened and did not.
pub mod property_value {
    use glam::Vec2;
    use lumen_core::components::Color;
    use lumen_core::property_store::PropertyValue;
    use serde::ser::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    enum ValueWire {
        Bool(bool),
        I64(i64),
        F64(f64),
        Str(String),
        Color([f32; 4]),
        Vec2([f32; 2]),
    }

    /// Encode a value. Errors on
    /// [`Custom`](PropertyValue::Custom).
    pub fn serialize<S: Serializer>(value: &PropertyValue, s: S) -> Result<S::Ok, S::Error> {
        let wire = match value {
            PropertyValue::Bool(v) => ValueWire::Bool(*v),
            PropertyValue::I64(v) => ValueWire::I64(*v),
            PropertyValue::F64(v) => ValueWire::F64(*v),
            PropertyValue::Str(v) => ValueWire::Str(v.to_string()),
            PropertyValue::Color(c) => ValueWire::Color([c.r, c.g, c.b, c.a]),
            PropertyValue::Vec2(v) => ValueWire::Vec2([v.x, v.y]),
            PropertyValue::Custom(_) => {
                return Err(S::Error::custom(
                    "PropertyValue::Custom cannot cross the plugin boundary",
                ));
            }
        };
        wire.serialize(s)
    }

    /// Decode a value. Never yields [`Custom`](PropertyValue::Custom).
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PropertyValue, D::Error> {
        Ok(match ValueWire::deserialize(d)? {
            ValueWire::Bool(v) => PropertyValue::Bool(v),
            ValueWire::I64(v) => PropertyValue::I64(v),
            ValueWire::F64(v) => PropertyValue::F64(v),
            ValueWire::Str(v) => PropertyValue::Str(v.into()),
            ValueWire::Color([r, g, b, a]) => PropertyValue::Color(Color { r, g, b, a }),
            ValueWire::Vec2([x, y]) => PropertyValue::Vec2(Vec2::new(x, y)),
        })
    }
}

/// Something a portable plugin pushes at the engine outside a call.
///
/// This is the direction a worker thread uses: a plugin that watches a file,
/// polls a device, or waits on a socket delivers what it found without being
/// asked. The bytes cross the plugin boundary onto
/// [`lumen_core::plugin_events`], and the script layer's per-tick drain
/// decodes them here and routes each one
/// ([`crate::runtime::collect_plugin_events`]).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PluginEvent {
    /// Call a handler in the app's script.
    Call {
        /// The handler name, as an app's script spells it.
        event: String,
        /// Identifier the handler receives as its first argument, so one
        /// handler can serve several sources; a per-key
        /// `on(event, key, fn)` registration wins over the fallback.
        key: String,
        /// Handler to call when the app defines no `event`; empty for none.
        fallback: String,
        /// Arguments for the handler, after the key.
        args: Vec<crate::ScriptValue>,
    },
    /// Apply commands, as if a call had emitted them.
    Commands(Vec<crate::ScriptCommand>),
}

/// Push one event onto the in-process bus a portable plugin's worker would
/// use, from inside the engine's own address space.
///
/// This is the delivery path for an in-process plugin or an engine-locked
/// runtime module: encode the event with the same codec the boundary uses and
/// hand it to [`lumen_core::plugin_events`], where the script layer's per-tick
/// drain picks it up and routes it exactly like one a dlopened plugin pushed.
/// The bus wakes a parked event loop, so an event pushed while the app idles
/// in `Wait` runs on the tick it triggers, the same as the dlopen path.
/// Returns `false` when the event does not encode or the bus is gone.
#[cfg(not(target_arch = "wasm32"))]
pub fn push_plugin_event(event: &PluginEvent) -> bool {
    match lumen_plugin_abi::codec::encode(event) {
        Ok(bytes) => lumen_core::plugin_events::push_plugin_event(bytes),
        Err(e) => {
            lumen_core::warn_line!("lumen-script: a plugin event did not encode: {e}");
            false
        }
    }
}
