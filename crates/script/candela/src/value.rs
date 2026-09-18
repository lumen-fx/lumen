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
        // An enum travels out of a script only (candela refuses one as a call
        // argument), and the host value has no variant of its own, so it
        // arrives externally tagged, the shape a JSON reader already expects: a
        // variant with no payload is its own name, and one with a payload is a
        // single-entry map from the name to the payload in declaration order.
        Value::Enum { variant, payload } => {
            if payload.is_empty() {
                ScriptValue::Str(variant.clone())
            } else {
                ScriptValue::Map(HashMap::from([(
                    variant.clone(),
                    ScriptValue::Array(payload.iter().map(candela_value_to_script).collect()),
                )]))
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variant_with_no_payload_is_its_own_name() {
        let value = Value::Enum {
            variant: "None".to_owned(),
            payload: Vec::new(),
        };
        assert_eq!(
            candela_value_to_script(&value),
            ScriptValue::Str("None".to_owned())
        );
    }

    #[test]
    fn a_variant_with_a_payload_is_tagged_by_its_name() {
        let value = Value::Enum {
            variant: "Some".to_owned(),
            payload: vec![Value::Int(7), Value::String("x".to_owned())],
        };
        let ScriptValue::Map(fields) = candela_value_to_script(&value) else {
            panic!("a variant carrying a payload reads as a map");
        };
        assert_eq!(
            fields.get("Some"),
            Some(&ScriptValue::Array(vec![
                ScriptValue::I64(7),
                ScriptValue::Str("x".to_owned()),
            ]))
        );
    }
}
