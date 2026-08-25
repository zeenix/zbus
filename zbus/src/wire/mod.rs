//! # zvariant
//!
//! This crate provides API for encoding/decoding of data to/from [D-Bus wire format][dwf]. This
//! binary wire format is simple and very efficient and hence useful outside of D-Bus context as
//! well. A modified form of this format, [GVariant], is very commonly used for efficient storage of
//! arbitrary data; it is implemented by the [zgvariant] crate.
//!
//! Since version 2.0, the API is [serde]-based and hence you'll find it very intuitive if you're
//! already familiar with serde. If you're not familiar with serde, you may want to first read its
//! [tutorial] before learning further about this crate.
//!
//! **Status:** Stable.
//!
//! ## Example code
//!
//! Serialization and deserialization is achieved through the [toplevel functions]:
//!
//! ```rust
//! use std::collections::HashMap;
//! use zbus::wire::{serialized::Context, to_bytes, Type, LE};
//! use serde::{Deserialize, Serialize};
//!
//! // All serialization and deserialization API, needs a context.
//! let ctxt = Context::new_dbus(LE, 0);
//!
//! // i16
//! let encoded = to_bytes(ctxt, &42i16).unwrap();
//! let decoded: i16 = encoded.deserialize().unwrap().0;
//! assert_eq!(decoded, 42);
//!
//! // strings
//! let encoded = to_bytes(ctxt, &"hello").unwrap();
//! let decoded: &str = encoded.deserialize().unwrap().0;
//! assert_eq!(decoded, "hello");
//!
//! // tuples
//! let t = ("hello", 42i32, true);
//! let encoded = to_bytes(ctxt, &t).unwrap();
//! let decoded: (&str, i32, bool) = encoded.deserialize().unwrap().0;
//! assert_eq!(decoded, t);
//!
//! // Vec
//! let v = vec!["hello", "world!"];
//! let encoded = to_bytes(ctxt, &v).unwrap();
//! let decoded: Vec<&str> = encoded.deserialize().unwrap().0;
//! assert_eq!(decoded, v);
//!
//! // Dictionary
//! let mut map: HashMap<i64, &str> = HashMap::new();
//! map.insert(1, "123");
//! map.insert(2, "456");
//! let encoded = to_bytes(ctxt, &map).unwrap();
//! let decoded: HashMap<i64, &str> = encoded.deserialize().unwrap().0;
//! assert_eq!(decoded[&1], "123");
//! assert_eq!(decoded[&2], "456");
//!
//! // derive macros to handle custom types.
//! #[derive(Deserialize, Serialize, Type, PartialEq, Debug)]
//! struct Struct<'s> {
//!     field1: u16,
//!     field2: i64,
//!     field3: &'s str,
//! }
//!
//! assert_eq!(Struct::SIGNATURE, "(qxs)");
//! let s = Struct {
//!     field1: 42,
//!     field2: i64::max_value(),
//!     field3: "hello",
//! };
//! let ctxt = Context::new_dbus(LE, 0);
//! let encoded = to_bytes(ctxt, &s).unwrap();
//! let decoded: Struct = encoded.deserialize().unwrap().0;
//! assert_eq!(decoded, s);
//!
//! // It can handle enums too, just that all variants must have the same number and types of
//! // fields. Names of fields don't matter though. You can make use of `Value` or `OwnedValue`
//! // if you want to encode different data in different fields.
//! #[derive(Deserialize, Serialize, Type, PartialEq, Debug)]
//! enum Enum<'s> {
//!     Variant1 { field1: u16, field2: i64, field3: &'s str },
//!     Variant2(u16, i64, &'s str),
//!     Variant3 { f1: u16, f2: i64, f3: &'s str },
//! }
//!
//! // Enum encoding uses a `u32` to denote the variant index. For unit-type enums that's all that's
//! // needed so the signature is just `u` but complex enums are encoded as a structure whose first
//! // field is the variant index and the second one is the field(s).
//! assert_eq!(Enum::SIGNATURE, "(u(qxs))");
//! let e = Enum::Variant3 {
//!     f1: 42,
//!     f2: i64::max_value(),
//!     f3: "hello",
//! };
//! let encoded = to_bytes(ctxt, &e).unwrap();
//! let decoded: Enum = encoded.deserialize().unwrap().0;
//! assert_eq!(decoded, e);
//!
//! // Enum encoding can be adjusted by using the `serde_repr` crate
//! // and by annotating the representation of the enum with `repr`.
//! use serde_repr::{Serialize_repr, Deserialize_repr};
//!
//! #[derive(Deserialize_repr, Serialize_repr, Type, PartialEq, Debug)]
//! #[repr(u8)]
//! enum UnitEnum {
//!     Variant1,
//!     Variant2,
//!     Variant3,
//! }
//!
//! assert_eq!(UnitEnum::SIGNATURE, "y");
//! let encoded = to_bytes(ctxt, &UnitEnum::Variant2).unwrap();
//! let e: UnitEnum = encoded.deserialize().unwrap().0;
//! assert_eq!(e, UnitEnum::Variant2);
//!
//! // Unit enums can also be (de)serialized as strings.
//! #[derive(Deserialize, Serialize, Type, PartialEq, Debug)]
//! #[zvariant(signature = "s")]
//! enum StrEnum {
//!     Variant1,
//!     Variant2,
//!     Variant3,
//!     // Catch-all that preserves the raw value for any variant we don't know about.
//!     #[serde(untagged)]
//!     Other(String),
//! }
//!
//! assert_eq!(StrEnum::SIGNATURE, "s");
//! ```
//!
//! Apart from the obvious requirement of [`serialized::Context`] instance by the main serialization
//! and deserialization API, the type being serialized or deserialized must also implement `Type`
//! trait in addition to [`Serialize`] or [`Deserialize`], respectively. Please refer to [`Type`
//! module documentation] for more details.
//!
//! Most of the [basic types] of D-Bus match 1-1 with all the primitive Rust types. The only two
//! exceptions being, [`Signature`] and [`ObjectPath`], which are really just strings. These types
//! are covered by the [`Basic`] trait.
//!
//! Similarly, most of the [container types] also map nicely to the usual Rust types and
//! collections (as can be seen in the example code above). The only note worthy exception being
//! ARRAY type. As arrays in Rust are fixed-sized, serde treats them as tuples and so does this
//! crate. This means they are encoded as STRUCT type of D-Bus. If you need to serialize to, or
//! deserialize from a D-Bus array, you'll need to use a [slice] (array can easily be converted to a
//! slice), a [`Vec`] or an [`arrayvec::ArrayVec`].
//!
//! D-Bus string types, including [`Signature`] and [`ObjectPath`], require one additional
//! restriction that strings in Rust do not. They must not contain any interior null bytes (`'\0'`).
//! Encoding/Decoding strings that contain this character will return an error.
//!
//! The generic D-Bus type, `VARIANT` is represented by `Value`, an enum that holds exactly one
//! value of any of the other types. Please refer to [`Value` module documentation] for examples.
//!
//! ## no-std
//!
//! While `std` is currently a hard requirement, optional `no-std` support is planned in the future.
//! On the other hand, `noalloc` support is not planned as it will be extremely difficult to
//! accomplish. However, community contribution can change that. 😊
//!
//! ## Optional features
//!
//! | Feature | Description |
//! | ---     | ----------- |
//! | arrayvec | Implement `Type` for [`arrayvec::ArrayVec`] and [`arrayvec::ArrayString`] |
//! | enumflags2 | Implement `Type` for [`enumflags2::BitFlags`]`<F>` |
//! | option-as-array | Enable `Option<T>` (de)serialization using array encoding |
//!
//! [dwf]: https://dbus.freedesktop.org/doc/dbus-specification.html#message-protocol-marshaling
//! [GVariant]: https://developer.gnome.org/documentation/specifications/gvariant-specification-1.0.html
//! [zgvariant]: https://crates.io/crates/zgvariant
//! [serde]: https://crates.io/crates/serde
//! [tutorial]: https://serde.rs/
//! [toplevel functions]: https://docs.rs/zvariant/latest/zvariant/#functions
//! [`serialized::Context`]: https://docs.rs/zvariant/latest/serialized/struct.Context.html
//! [`Serialize`]: https://docs.serde.rs/serde/trait.Serialize.html
//! [`Deserialize`]: https://docs.serde.rs/serde/de/trait.Deserialize.html
//! [`Type` module documentation]: https://docs.rs/zvariant/latest/zvariant/trait.Type.html
//! [basic types]: https://dbus.freedesktop.org/doc/dbus-specification.html#basic-types
//! [`Signature`]: https://docs.rs/zvariant/latest/zvariant/struct.Signature.html
//! [`ObjectPath`]: https://docs.rs/zvariant/latest/zvariant/struct.ObjectPath.html
//! [`Basic`]: https://docs.rs/zvariant/latest/zvariant/trait.Basic.html
//! [container types]: https://dbus.freedesktop.org/doc/dbus-specification.html#container-types
//! [slice]: https://doc.rust-lang.org/std/primitive.slice.html
//! [`Vec`]: https://doc.rust-lang.org/std/vec/struct.Vec.html
//! [`arrayvec::ArrayVec`]: https://docs.rs/arrayvec/0.7.1/arrayvec/struct.ArrayVec.html
//! [`arrayvec::ArrayString`]: https://docs.rs/arrayvec/0.7.1/arrayvec/struct.ArrayString.html
//! [`enumflags2::Bitflags`]: https://docs.rs/enumflags2/latest/enumflags2/struct.BitFlags.html
//! [`Value` module documentation]: https://docs.rs/zvariant/latest/zvariant/enum.Value.html

#[macro_use]
mod utils;
pub use utils::*;

mod array;
pub use array::*;

mod basic;
pub use basic::*;

mod dict;
pub use dict::*;

pub mod serialized;

#[cfg(unix)]
mod fd;
#[cfg(unix)]
pub use fd::*;

mod object_path;
pub use crate::wire::object_path::*;

mod file_path;
pub use crate::wire::file_path::*;

mod ser;
pub use ser::*;

mod de;

pub mod dbus;

pub mod signature;
pub use signature::Signature;

mod str;
pub use crate::wire::str::*;

mod structure;
pub use crate::wire::structure::*;

mod optional;
pub use crate::wire::optional::*;

mod value;
pub use value::*;

mod error;
pub use error::*;

#[macro_use]
mod r#type;
pub use r#type::*;

mod tuple;
pub use tuple::*;

mod from_value;

mod into_value;

mod owned_value;
pub use owned_value::*;

mod container_depths;

pub mod as_value;
#[deprecated(since = "5.5.0", note = "Use `as_value::Deserialize` instead.")]
pub use as_value::Deserialize as DeserializeValue;
#[deprecated(since = "5.5.0", note = "Use `as_value::Serialize` instead.")]
pub use as_value::Serialize as SerializeValue;

pub use zvariant_derive::{DeserializeDict, OwnedValue, SerializeDict, Type, Value, signature};

// Macro support module, not part of the public API.
#[doc(hidden)]
pub mod export {
    pub use serde;
}

// Re-export all of the `endi` API for ease of use.
pub use endi::*;

// `#[macro_export]` puts these at the crate root whatever module they are written in; this is
// the path they are documented under.
#[doc(inline)]
pub use crate::{impl_type_with_repr, static_str_type};
