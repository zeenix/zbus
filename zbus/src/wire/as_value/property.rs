use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};

use crate::wire::{Dict, Signature, Type, Value};

/// Deserializes a property value directly from its in-memory representation.
#[doc(hidden)]
pub fn deserialize_for_property<'de, T>(value: &'de Value<'_>) -> crate::Result<T>
where
    T: Type + serde::Deserialize<'de>,
{
    T::deserialize(ValueDeserializer::new(value, T::SIGNATURE, false))
}

struct ValueDeserializer<'de, 'sig> {
    value: &'de Value<'de>,
    expected: &'sig Signature,
    unwrap_variant: bool,
    variant_payload: bool,
}

impl<'de, 'sig> ValueDeserializer<'de, 'sig> {
    fn new(value: &'de Value<'de>, expected: &'sig Signature, unwrap_variant: bool) -> Self {
        Self {
            value,
            expected,
            unwrap_variant,
            variant_payload: false,
        }
    }

    fn actual(&self) -> &'de Value<'de> {
        if self.unwrap_variant && self.expected != &Signature::Variant {
            if let Value::Value(value) = self.value {
                return value;
            }
        }

        self.value
    }

    fn check(&self) -> crate::Result<&'de Value<'de>> {
        let value = self.actual();
        if self.expected == &Signature::Variant
            || signatures_compatible(value.value_signature(), self.expected, self.unwrap_variant)
        {
            Ok(value)
        } else {
            Err(crate::Error::SignatureMismatch(
                value.value_signature().clone(),
                self.expected.to_string(),
            ))
        }
    }

    fn variant_access(&self) -> VariantSeqAccess<'de> {
        if self.unwrap_variant || self.variant_payload {
            VariantSeqAccess::new_existing(self.actual())
        } else {
            VariantSeqAccess::new(self.actual())
        }
    }

    #[cfg(feature = "option-as-array")]
    fn child(
        &self,
        value: &'de Value<'de>,
        expected: &'sig Signature,
        unwrap_variant: bool,
    ) -> Self {
        Self::new(value, expected, unwrap_variant)
    }

    fn mismatch(&self, expected: &'static str) -> crate::Error {
        crate::Error::SignatureMismatch(
            self.actual().value_signature().clone(),
            expected.to_owned(),
        )
    }

    fn bytes(&self) -> crate::Result<Vec<u8>> {
        let expected = self.expected_array_element()?;
        if expected != &Signature::U8 {
            return Err(self.mismatch("an array of bytes"));
        }

        let Value::Array(array) = self.check()? else {
            return Err(self.mismatch("an array of bytes"));
        };
        array
            .inner()
            .iter()
            .map(|value| match Self::new(value, expected, true).check()? {
                Value::U8(value) => Ok(*value),
                _ => Err(self.mismatch("an array of bytes")),
            })
            .collect()
    }

    fn expected_array_element(&self) -> crate::Result<&'sig Signature> {
        match self.expected {
            Signature::Array(element) => Ok(element.signature()),
            _ => Err(self.mismatch("an array")),
        }
    }
}

impl<'de, 'sig> de::Deserializer<'de> for ValueDeserializer<'de, 'sig> {
    type Error = crate::Error;

    fn deserialize_any<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        if self.expected == &Signature::Variant {
            return visitor.visit_seq(self.variant_access());
        }

        match self.check()? {
            Value::U8(value) => visitor.visit_u8(*value),
            Value::Bool(value) => visitor.visit_bool(*value),
            Value::I16(value) => visitor.visit_i16(*value),
            Value::U16(value) => visitor.visit_u16(*value),
            Value::I32(value) => visitor.visit_i32(*value),
            Value::U32(value) => visitor.visit_u32(*value),
            Value::I64(value) => visitor.visit_i64(*value),
            Value::U64(value) => visitor.visit_u64(*value),
            Value::F64(value) => visitor.visit_f64(*value),
            Value::Str(value) => visitor.visit_borrowed_str(value.as_str()),
            Value::Signature(value) => visitor.visit_string(value.to_string()),
            Value::ObjectPath(value) => visitor.visit_borrowed_str(value.as_str()),
            Value::Array(array) => {
                visitor.visit_seq(ArrayAccess::new(array.inner(), self.expected))
            }
            Value::Dict(dict) => visitor.visit_map(DictAccess::new(dict, self.expected)),
            Value::Structure(structure) => {
                visitor.visit_seq(StructureAccess::new(structure.fields(), self.expected))
            }
            #[cfg(unix)]
            Value::Fd(fd) => visitor.visit_i32(std::os::fd::AsRawFd::as_raw_fd(fd)),
            Value::Value(_) => unreachable!("variants are handled above"),
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::Bool(value) => visitor.visit_bool(*value),
            _ => Err(self.mismatch("a boolean")),
        }
    }

    fn deserialize_i8<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_i16(visitor)
    }

    fn deserialize_i16<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::I16(value) => visitor.visit_i16(*value),
            _ => Err(self.mismatch("an int16")),
        }
    }

    fn deserialize_i32<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::I32(value) => visitor.visit_i32(*value),
            #[cfg(unix)]
            Value::Fd(value) => visitor.visit_i32(std::os::fd::AsRawFd::as_raw_fd(value)),
            _ => Err(self.mismatch("an int32")),
        }
    }

    fn deserialize_i64<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::I64(value) => visitor.visit_i64(*value),
            _ => Err(self.mismatch("an int64")),
        }
    }

    fn deserialize_u8<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::U8(value) => visitor.visit_u8(*value),
            _ => Err(self.mismatch("a byte")),
        }
    }

    fn deserialize_u16<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::U16(value) => visitor.visit_u16(*value),
            _ => Err(self.mismatch("a uint16")),
        }
    }

    fn deserialize_u32<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::U32(value) => visitor.visit_u32(*value),
            _ => Err(self.mismatch("a uint32")),
        }
    }

    fn deserialize_u64<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::U64(value) => visitor.visit_u64(*value),
            _ => Err(self.mismatch("a uint64")),
        }
    }

    fn deserialize_f32<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::F64(value) => {
                if value.is_finite() && *value > f32::MAX as f64 {
                    return Err(de::Error::invalid_value(
                        de::Unexpected::Float(*value),
                        &"Too large for f32",
                    ));
                }
                visitor.visit_f32(*value as f32)
            }
            _ => Err(self.mismatch("a float")),
        }
    }

    fn deserialize_f64<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::F64(value) => visitor.visit_f64(*value),
            _ => Err(self.mismatch("a double")),
        }
    }

    fn deserialize_char<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_str<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::Str(value) => visitor.visit_borrowed_str(value.as_str()),
            Value::Signature(value) => visitor.visit_string(value.to_string()),
            Value::ObjectPath(value) => visitor.visit_borrowed_str(value.as_str()),
            _ => Err(self.mismatch("a string")),
        }
    }

    fn deserialize_string<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let bytes = self.bytes()?;
        visitor.visit_byte_buf(bytes)
    }

    fn deserialize_byte_buf<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        #[cfg(feature = "option-as-array")]
        {
            let value = self.check()?;
            let array = match value {
                Value::Array(array) => array,
                _ => return Err(self.mismatch("an option array")),
            };
            let element_signature = self.expected_array_element()?;
            match array.inner() {
                [] => visitor.visit_none(),
                [value] => visitor.visit_some(self.child(value, element_signature, true)),
                _ => Err(de::Error::invalid_length(
                    array.len(),
                    &"an option array of 0 or 1 item",
                )),
            }
        }
        #[cfg(not(feature = "option-as-array"))]
        {
            let _ = visitor;
            Err(de::Error::custom(
                "Can only decode Option<T> from a Value if the `option-as-array` feature is \
                 enabled",
            ))
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.check()?;
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V>(self, _name: &'static str, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.check()?;
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        if self.expected == &Signature::Variant {
            return visitor.visit_seq(self.variant_access());
        }
        match self.check()? {
            Value::Array(array) => {
                visitor.visit_seq(ArrayAccess::new(array.inner(), self.expected))
            }
            Value::Structure(structure) => {
                visitor.visit_seq(StructureAccess::new(structure.fields(), self.expected))
            }
            Value::U8(_) if self.expected == &Signature::U8 => visitor.visit_seq(EmptyAccess),
            _ => Err(self.mismatch("an array or structure")),
        }
    }

    fn deserialize_map<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        match self.check()? {
            Value::Dict(dict) => visitor.visit_map(DictAccess::new(dict, self.expected)),
            _ => Err(self.mismatch("a dictionary")),
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        if matches!(self.expected, Signature::Dict { .. }) {
            self.deserialize_map(visitor)
        } else {
            self.deserialize_seq(visitor)
        }
    }

    fn deserialize_enum<V>(
        self,
        name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let value = self.check()?;
        let access = match value {
            Value::Structure(structure) => {
                ValueEnumAccess::structure(structure.fields(), self.expected)?
            }
            _ => ValueEnumAccess::scalar(value, self.expected),
        };
        let _ = name;
        visitor.visit_enum(access)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_any(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.deserialize_any(visitor)
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

struct ArrayAccess<'de> {
    elements: &'de [Value<'de>],
    index: usize,
    expected: Signature,
}

impl<'de> ArrayAccess<'de> {
    fn new(elements: &'de [Value<'de>], expected: &Signature) -> Self {
        let expected = match expected {
            Signature::Array(element) => element.signature().clone(),
            _ => expected.clone(),
        };
        Self {
            elements,
            index: 0,
            expected,
        }
    }
}

impl<'de> SeqAccess<'de> for ArrayAccess<'de> {
    type Error = crate::Error;

    fn next_element_seed<T>(&mut self, seed: T) -> crate::Result<Option<T::Value>>
    where
        T: DeserializeSeed<'de>,
    {
        let Some(value) = self.elements.get(self.index) else {
            return Ok(None);
        };
        self.index += 1;
        seed.deserialize(ValueDeserializer::new(value, &self.expected, true))
            .map(Some)
    }
}

struct DictAccess<'de> {
    entries: Box<dyn Iterator<Item = (&'de Value<'de>, &'de Value<'de>)> + 'de>,
    key: Signature,
    value: Signature,
    pending_value: Option<&'de Value<'de>>,
}

impl<'de> DictAccess<'de> {
    fn new(dict: &'de Dict<'de, 'de>, expected: &Signature) -> Self {
        let (key, value) = match expected {
            Signature::Dict { key, value } => (key.signature().clone(), value.signature().clone()),
            _ => (Signature::Variant, Signature::Variant),
        };
        Self {
            entries: Box::new(dict.iter()),
            key,
            value,
            pending_value: None,
        }
    }
}

impl<'de> MapAccess<'de> for DictAccess<'de> {
    type Error = crate::Error;

    fn next_key_seed<K>(&mut self, seed: K) -> crate::Result<Option<K::Value>>
    where
        K: DeserializeSeed<'de>,
    {
        let Some((key, value)) = self.entries.next() else {
            return Ok(None);
        };
        self.pending_value = Some(value);
        seed.deserialize(ValueDeserializer::new(key, &self.key, false))
            .map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> crate::Result<V::Value>
    where
        V: DeserializeSeed<'de>,
    {
        let value = self.pending_value.take().ok_or_else(|| {
            crate::Error::Failure("map value requested before map key".to_owned())
        })?;
        seed.deserialize(ValueDeserializer::new(value, &self.value, true))
    }
}

struct StructureAccess<'de> {
    fields: &'de [Value<'de>],
    signatures: Vec<Signature>,
    index: usize,
}

impl<'de> StructureAccess<'de> {
    fn new(fields: &'de [Value<'de>], expected: &Signature) -> Self {
        let signatures = match expected {
            Signature::Structure(signatures) => signatures.iter().cloned().collect(),
            _ => vec![],
        };
        Self {
            fields,
            signatures,
            index: 0,
        }
    }
}

impl<'de> SeqAccess<'de> for StructureAccess<'de> {
    type Error = crate::Error;

    fn next_element_seed<T>(&mut self, seed: T) -> crate::Result<Option<T::Value>>
    where
        T: DeserializeSeed<'de>,
    {
        let Some(value) = self.fields.get(self.index) else {
            return Ok(None);
        };
        let Some(signature) = self.signatures.get(self.index) else {
            return Err(de::Error::invalid_length(self.fields.len(), &"a structure"));
        };
        self.index += 1;
        seed.deserialize(ValueDeserializer::new(value, signature, true))
            .map(Some)
    }
}

struct EmptyAccess;

impl<'de> SeqAccess<'de> for EmptyAccess {
    type Error = crate::Error;

    fn next_element_seed<T>(&mut self, _seed: T) -> crate::Result<Option<T::Value>>
    where
        T: DeserializeSeed<'de>,
    {
        Ok(None)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(0)
    }
}

struct VariantSeqAccess<'de> {
    signature: Signature,
    value: &'de Value<'de>,
    index: usize,
}

impl<'de> VariantSeqAccess<'de> {
    fn new(value: &'de Value<'de>) -> Self {
        let signature = value.value_signature().clone();
        Self {
            signature,
            value,
            index: 0,
        }
    }

    fn new_existing(value: &'de Value<'de>) -> Self {
        match value {
            Value::Value(value) => Self::new(value),
            value => Self::new(value),
        }
    }
}

impl<'de> SeqAccess<'de> for VariantSeqAccess<'de> {
    type Error = crate::Error;

    fn next_element_seed<T>(&mut self, seed: T) -> crate::Result<Option<T::Value>>
    where
        T: DeserializeSeed<'de>,
    {
        match self.index {
            0 => {
                self.index = 1;
                seed.deserialize(serde::de::value::StringDeserializer::<crate::Error>::new(
                    self.signature.to_string(),
                ))
                .map(Some)
            }
            1 => {
                self.index = 2;
                let mut de = ValueDeserializer::new(self.value, &self.signature, false);
                de.variant_payload = self.signature == Signature::Variant;
                seed.deserialize(de).map(Some)
            }
            _ => Ok(None),
        }
    }
}

struct ValueEnumAccess<'de> {
    discriminant: &'de Value<'de>,
    discriminant_signature: Signature,
    payload: Option<&'de Value<'de>>,
    payload_signature: Option<Signature>,
}

impl<'de> ValueEnumAccess<'de> {
    fn scalar(value: &'de Value<'de>, expected: &Signature) -> Self {
        Self {
            discriminant: value,
            discriminant_signature: expected.clone(),
            payload: None,
            payload_signature: None,
        }
    }

    fn structure(value: &'de [Value<'de>], expected: &Signature) -> crate::Result<Self> {
        let Signature::Structure(signatures) = expected else {
            return Err(crate::Error::SignatureMismatch(
                expected.clone(),
                "an enum structure".to_owned(),
            ));
        };
        let Some(discriminant) = value.first() else {
            return Err(de::Error::invalid_length(0, &"an enum discriminant"));
        };
        let mut signatures = signatures.iter();
        let Some(discriminant_signature) = signatures.next() else {
            return Err(de::Error::invalid_length(0, &"an enum discriminant"));
        };
        let payload = value.get(1);
        let payload_signature = signatures.next();
        if payload.is_some() != payload_signature.is_some() {
            return Err(de::Error::invalid_length(value.len(), &"an enum payload"));
        }
        Ok(Self {
            discriminant,
            discriminant_signature: discriminant_signature.clone(),
            payload,
            payload_signature: payload_signature.cloned(),
        })
    }
}

impl<'de> EnumAccess<'de> for ValueEnumAccess<'de> {
    type Error = crate::Error;
    type Variant = ValueVariantAccess<'de>;

    fn variant_seed<V>(self, seed: V) -> crate::Result<(V::Value, Self::Variant)>
    where
        V: DeserializeSeed<'de>,
    {
        let variant = seed.deserialize(ValueDeserializer::new(
            self.discriminant,
            &self.discriminant_signature,
            false,
        ))?;
        Ok((
            variant,
            ValueVariantAccess {
                payload: self.payload,
                payload_signature: self.payload_signature,
            },
        ))
    }
}

struct ValueVariantAccess<'de> {
    payload: Option<&'de Value<'de>>,
    payload_signature: Option<Signature>,
}

impl<'de> VariantAccess<'de> for ValueVariantAccess<'de> {
    type Error = crate::Error;

    fn unit_variant(self) -> crate::Result<()> {
        if self.payload.is_none() {
            Ok(())
        } else {
            Err(de::Error::invalid_length(2, &"a unit enum"))
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> crate::Result<T::Value>
    where
        T: DeserializeSeed<'de>,
    {
        let Some((value, signature)) = self.payload.zip(self.payload_signature.as_ref()) else {
            return Err(de::Error::invalid_length(1, &"an enum payload"));
        };
        seed.deserialize(ValueDeserializer::new(value, signature, true))
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.fields_access(visitor)
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        self.fields_access(visitor)
    }
}

impl<'de> ValueVariantAccess<'de> {
    fn fields_access<V>(self, visitor: V) -> crate::Result<V::Value>
    where
        V: Visitor<'de>,
    {
        let Some((Value::Structure(structure), Signature::Structure(signatures))) =
            self.payload.zip(self.payload_signature)
        else {
            return Err(de::Error::invalid_type(
                de::Unexpected::Other("non-structure enum payload"),
                &"a structure enum payload",
            ));
        };
        visitor.visit_seq(EnumFieldsAccess::new(
            structure.fields(),
            signatures.iter().cloned().collect(),
        ))
    }
}

struct EnumFieldsAccess<'de> {
    fields: &'de [Value<'de>],
    signatures: Vec<Signature>,
    index: usize,
}

impl<'de> EnumFieldsAccess<'de> {
    fn new(fields: &'de [Value<'de>], signatures: Vec<Signature>) -> Self {
        Self {
            fields,
            signatures,
            index: 0,
        }
    }
}

impl<'de> SeqAccess<'de> for EnumFieldsAccess<'de> {
    type Error = crate::Error;

    fn next_element_seed<T>(&mut self, seed: T) -> crate::Result<Option<T::Value>>
    where
        T: DeserializeSeed<'de>,
    {
        let Some(value) = self.fields.get(self.index) else {
            return Ok(None);
        };
        let Some(signature) = self.signatures.get(self.index) else {
            return Err(de::Error::invalid_length(
                self.fields.len(),
                &"an enum payload",
            ));
        };
        self.index += 1;
        seed.deserialize(ValueDeserializer::new(value, signature, true))
            .map(Some)
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
