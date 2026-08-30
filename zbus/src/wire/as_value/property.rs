use crate::wire::{
    Array, Dict, LE, Signature, StructureBuilder, Value,
    serialized::{Context, Data},
};

/// Serializes a property value using `signature`.
///
/// An array or dictionary can declare its elements or values as D-Bus variants. If `signature`
/// names their concrete types, this function serializes the contained values without their variant
/// wrappers. Dictionary keys and structure fields must have exactly the signatures requested by
/// `signature`. These rules are applied recursively to nested arrays and dictionaries.
///
/// Returns [`crate::Error::SignatureMismatch`] if `value` cannot be represented by `signature`.
#[doc(hidden)]
pub fn serialized_for_property(
    value: &Value<'_>,
    signature: &Signature,
) -> crate::Result<Data<'static, 'static>> {
    let context = Context::new(LE, 0);
    if signature == &Signature::Variant || value.value_signature() == signature {
        return crate::wire::to_bytes(context, value);
    }

    let value = value_for_signature(value, signature, false)?;

    crate::wire::to_bytes(context, &value)
}

fn value_for_signature(
    value: &Value<'_>,
    signature: &Signature,
    unwrap_variant: bool,
) -> crate::Result<Value<'static>> {
    if !signatures_compatible(value.value_signature(), signature, unwrap_variant) {
        return Err(crate::Error::SignatureMismatch(
            value.value_signature().clone(),
            signature.to_string(),
        ));
    }

    let value = match value {
        Value::Value(value) if unwrap_variant && signature != &Signature::Variant => value,
        value => value,
    };

    if value.value_signature() == signature {
        return value.try_to_owned().map(Into::into);
    }

    match (value, signature) {
        (Value::Array(array), Signature::Array(element_signature)) => {
            let mut converted = Array::new_full_signature(signature);
            for element in array.inner() {
                converted.append(value_for_signature(
                    element,
                    element_signature.signature(),
                    true,
                )?)?;
            }

            Ok(Value::Array(converted))
        }
        (
            Value::Dict(dict),
            Signature::Dict {
                key: key_signature,
                value: value_signature,
            },
        ) => {
            let mut converted = Dict::new_full_signature(signature);
            for (key, value) in dict.iter() {
                converted.append(
                    value_for_signature(key, key_signature.signature(), false)?,
                    value_for_signature(value, value_signature.signature(), true)?,
                )?;
            }

            Ok(Value::Dict(converted))
        }
        (Value::Structure(structure), Signature::Structure(field_signatures))
            if structure.fields().len() == field_signatures.len() =>
        {
            let mut converted = StructureBuilder::new();
            for (field, signature) in structure.fields().iter().zip(field_signatures.iter()) {
                converted = converted.append_field(value_for_signature(field, signature, false)?);
            }

            converted.build().map(Value::Structure)
        }
        _ => Err(crate::Error::SignatureMismatch(
            value.value_signature().clone(),
            signature.to_string(),
        )),
    }
}

fn signatures_compatible(actual: &Signature, expected: &Signature, unwrap_variant: bool) -> bool {
    if actual == expected {
        return true;
    }
    if unwrap_variant && actual == &Signature::Variant && expected != &Signature::Variant {
        return true;
    }

    match (actual, expected) {
        (Signature::Array(actual), Signature::Array(expected)) => {
            signatures_compatible(actual.signature(), expected.signature(), true)
        }
        (
            Signature::Dict {
                key: actual_key,
                value: actual_value,
            },
            Signature::Dict {
                key: expected_key,
                value: expected_value,
            },
        ) => {
            signatures_compatible(actual_key.signature(), expected_key.signature(), false)
                && signatures_compatible(actual_value.signature(), expected_value.signature(), true)
        }
        (Signature::Structure(actual), Signature::Structure(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected.iter())
                    .all(|(actual, expected)| signatures_compatible(actual, expected, false))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde::de::DeserializeOwned;

    use super::*;
    use crate::wire::{OwnedValue, Type, as_value};

    fn deserialize<T>(value: &Value<'_>) -> T
    where
        T: DeserializeOwned + Type,
    {
        serialized_for_property(value, T::SIGNATURE)
            .unwrap()
            .deserialize::<as_value::Deserialize<'_, T>>()
            .unwrap()
            .0
            .0
    }

    #[test]
    fn unwraps_array_variants() {
        let mut array = Array::new(&Signature::Variant);
        array
            .append(Value::Value(Box::new(Value::from("one"))))
            .unwrap();
        array
            .append(Value::Value(Box::new(Value::from("two"))))
            .unwrap();

        assert_eq!(
            deserialize::<Vec<String>>(&Value::Array(array)),
            ["one", "two"]
        );
    }

    #[test]
    fn unwraps_variants_in_nested_containers() {
        let mut inner = Array::new(&Signature::Variant);
        inner
            .append(Value::Value(Box::new(Value::from("value"))))
            .unwrap();
        let mut outer = Array::new(inner.signature());
        outer.append(Value::Array(inner)).unwrap();
        let structure = StructureBuilder::new()
            .append_field(Value::Array(outer))
            .build()
            .unwrap();
        let decoded: (Vec<Vec<String>>,) = deserialize(&Value::Structure(structure));

        assert_eq!(decoded.0, [vec!["value".to_string()]]);
    }

    #[test]
    fn unwraps_dict_variants() {
        let mut dict = Dict::new(&Signature::Str, &Signature::Variant);
        dict.append(
            Value::from("key"),
            Value::Value(Box::new(Value::from("value"))),
        )
        .unwrap();

        assert_eq!(
            deserialize::<HashMap<String, String>>(&Value::Dict(dict))["key"],
            "value"
        );
    }

    #[test]
    fn unwraps_variants_in_nested_dicts() {
        let mut properties = Dict::new(&Signature::Str, &Signature::Variant);
        properties
            .append(
                Value::from("ssid"),
                Value::Value(Box::new(Value::from("home"))),
            )
            .unwrap();
        let mut interfaces = Dict::new(&Signature::Str, properties.signature());
        interfaces
            .append(Value::from("802-11-wireless"), Value::Dict(properties))
            .unwrap();

        let decoded: HashMap<String, HashMap<String, String>> =
            deserialize(&Value::Dict(interfaces));
        assert_eq!(decoded["802-11-wireless"]["ssid"], "home");
    }

    #[test]
    fn rejects_unrelated_empty_containers() {
        let array = Value::Array(Array::new(&Signature::I32));
        assert!(serialized_for_property(&array, Vec::<String>::SIGNATURE).is_err());

        let dict = Value::Dict(Dict::new(&Signature::I32, &Signature::Str));
        assert!(serialized_for_property(&dict, HashMap::<String, String>::SIGNATURE).is_err());
    }

    #[test]
    fn preserves_variant_values() {
        let value = Value::U32(42);
        let decoded = deserialize::<OwnedValue>(&value);
        assert_eq!(decoded, OwnedValue::from(42_u32));

        let value = Value::Value(Box::new(Value::U32(42)));
        let decoded = deserialize::<OwnedValue>(&value);
        assert_eq!(decoded, OwnedValue::try_from(value).unwrap());
    }
}
