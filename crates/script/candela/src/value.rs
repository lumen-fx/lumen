//! Marshalling between [`ScriptValue`] and candela's embedding [`Value`].
//!
//! Both directions recurse through the `Array` and `Map` variants, so a
//! structured value round-trips across a host call, a derivation recompute, and
//! the signal mirror without being flattened to text.

use std::collections::HashMap;

use candela_vm::Value;
use lumen_script::ScriptValue;

/// Marshal a [`ScriptValue`] into a candela [`Value`].
pub(crate) fn script_value_to_candela(v: &ScriptValue) -> Value {
    match v {
        ScriptValue::Unit => Value::Null,
        ScriptValue::Bool(b) => Value::Bool(*b),
        ScriptValue::I64(i) => Value::Int(*i),
        ScriptValue::F64(f) => Value::Float(*f),
        ScriptValue::Str(s) => Value::String(s.clone()),
        ScriptValue::Array(items) => {
            Value::Array(items.iter().map(script_value_to_candela).collect())
        }
        ScriptValue::Map(m) => Value::Map(
            m.iter()
                .map(|(k, val)| (k.clone(), script_value_to_candela(val)))
                .collect(),
        ),
    }
}

/// Marshal a candela [`Value`] back into a [`ScriptValue`].
pub(crate) fn candela_value_to_script(v: &Value) -> ScriptValue {
    match v {
        Value::Null => ScriptValue::Unit,
        Value::Int(i) => ScriptValue::I64(*i),
        Value::Float(f) => ScriptValue::F64(*f),
        Value::Bool(b) => ScriptValue::Bool(*b),
        Value::String(s) => ScriptValue::Str(s.clone()),
        Value::Array(items) => {
            ScriptValue::Array(items.iter().map(candela_value_to_script).collect())
        }
        Value::Map(m) => ScriptValue::Map(
            m.iter()
                .map(|(k, val)| (k.clone(), candela_value_to_script(val)))
                .collect(),
        ),
    }
}

/// Flatten an array of [`ScriptValue::Map`] records into the stringified
/// field rows a `SetArray` command carries. Non-map elements become a single
/// `{ "value": <stringified> }` row so scalars are still addressable.
pub(crate) fn array_to_rows(items: &[ScriptValue]) -> Vec<HashMap<String, String>> {
    items
        .iter()
        .map(|item| match item {
            ScriptValue::Map(m) => m.iter().map(|(k, v)| (k.clone(), v.stringify())).collect(),
            other => HashMap::from([("value".to_owned(), other.stringify())]),
        })
        .collect()
}
