//! Support for the type-definition elements of the [Telepathy D-Bus introspection extensions].
//!
//! While the D-Bus type system only knows structural types (through signatures), the Telepathy
//! extensions allow an introspection document to give them names and documentation:
//!
//! * `<tp:simple-type>` names a plain D-Bus type ([`SimpleType`]),
//! * `<tp:enum>` enumerates the values a type can take ([`Enum`]),
//! * `<tp:struct>` names a structure and its members ([`Struct`]),
//! * `<tp:mapping>` names a dictionary type ([`Mapping`]).
//!
//! Type definitions appear as children of the `<node>` or `<interface>` elements (see
//! [`Node::telepathy_types`](crate::Node::telepathy_types) and
//! [`Interface::telepathy_types`](crate::Interface::telepathy_types)) and are referenced by
//! name through the `tp:type` attribute of `<arg>`, `<property>` and `<tp:member>` elements
//! (see e. g. [`Arg::tp_type`](crate::Arg::tp_type)), where the name may carry one `[]`
//! suffix per level of array nesting (e. g. `Playlist[]`).
//!
//! A definition that cannot be parsed — a missing required attribute, an invalid signature —
//! does not fail the document: it is skipped with a [`Warning`](crate::Warning), in the spirit
//! of treating everything beyond the core introspection format as optional extras. Type
//! definitions are also parse-only: [`Node::to_writer`](crate::Node::to_writer) does not emit
//! them.
//!
//! [Telepathy D-Bus introspection extensions]: https://telepathy.freedesktop.org/spec/

use zbus::{Signature, Str};

/// A named type defined through the Telepathy introspection extensions.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeDef<'a> {
    /// A name given to a plain D-Bus type (`<tp:simple-type>`).
    SimpleType(SimpleType<'a>),
    /// An enumeration of the values of a type (`<tp:enum>`).
    Enum(Enum<'a>),
    /// A named structure (`<tp:struct>`).
    Struct(Struct<'a>),
    /// A named dictionary type (`<tp:mapping>`).
    Mapping(Mapping<'a>),
}

impl TypeDef<'_> {
    /// The name of the defined type, as referenced by `tp:type` attributes.
    pub fn name(&self) -> &str {
        match self {
            TypeDef::SimpleType(t) => t.name(),
            TypeDef::Enum(e) => e.name(),
            TypeDef::Struct(s) => s.name(),
            TypeDef::Mapping(m) => m.name(),
        }
    }

    /// The docstring of the definition, if any.
    pub fn docstring(&self) -> Option<&str> {
        match self {
            TypeDef::SimpleType(t) => t.docstring(),
            TypeDef::Enum(e) => e.docstring(),
            TypeDef::Struct(s) => s.docstring(),
            TypeDef::Mapping(m) => m.docstring(),
        }
    }

    /// The D-Bus signature of the defined type.
    pub fn signature(&self) -> Signature {
        match self {
            TypeDef::SimpleType(t) => t.ty().clone(),
            TypeDef::Enum(e) => e.ty().clone(),
            TypeDef::Struct(s) => s.signature(),
            TypeDef::Mapping(m) => m.signature(),
        }
    }

    /// Creates an owned clone of `self`.
    pub fn to_owned(&self) -> TypeDef<'static> {
        self.clone().into_owned()
    }

    /// Converts `self` into an owned tree, copying the strings it borrows.
    pub fn into_owned(self) -> TypeDef<'static> {
        match self {
            TypeDef::SimpleType(t) => TypeDef::SimpleType(t.into_owned()),
            TypeDef::Enum(e) => TypeDef::Enum(e.into_owned()),
            TypeDef::Struct(s) => TypeDef::Struct(s.into_owned()),
            TypeDef::Mapping(m) => TypeDef::Mapping(m.into_owned()),
        }
    }
}

/// A name given to a plain D-Bus type (`<tp:simple-type>`).
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleType<'a> {
    pub(crate) name: Str<'a>,
    pub(crate) ty: Signature,
    pub(crate) docstring: Option<Str<'a>>,
}

impl SimpleType<'_> {
    /// The name of the type.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The underlying D-Bus type.
    pub fn ty(&self) -> &Signature {
        &self.ty
    }

    /// The docstring of the definition, if any.
    pub fn docstring(&self) -> Option<&str> {
        self.docstring.as_deref()
    }

    /// Creates an owned clone of `self`.
    pub fn to_owned(&self) -> SimpleType<'static> {
        self.clone().into_owned()
    }

    /// Converts `self` into an owned tree, copying the strings it borrows.
    pub fn into_owned(self) -> SimpleType<'static> {
        SimpleType {
            name: self.name.into_owned(),
            ty: self.ty,
            docstring: self.docstring.map(Str::into_owned),
        }
    }
}

/// An enumeration of the values of a type (`<tp:enum>`).
#[derive(Debug, Clone, PartialEq)]
pub struct Enum<'a> {
    pub(crate) name: Str<'a>,
    pub(crate) ty: Signature,
    pub(crate) values: Vec<EnumValue<'a>>,
    pub(crate) docstring: Option<Str<'a>>,
}

impl<'a> Enum<'a> {
    /// The name of the type.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The underlying D-Bus type.
    pub fn ty(&self) -> &Signature {
        &self.ty
    }

    /// The values of the enumeration.
    pub fn values(&self) -> &[EnumValue<'a>] {
        &self.values
    }

    /// The docstring of the definition, if any.
    pub fn docstring(&self) -> Option<&str> {
        self.docstring.as_deref()
    }

    /// Creates an owned clone of `self`.
    pub fn to_owned(&self) -> Enum<'static> {
        self.clone().into_owned()
    }

    /// Converts `self` into an owned tree, copying the strings it borrows.
    pub fn into_owned(self) -> Enum<'static> {
        Enum {
            name: self.name.into_owned(),
            ty: self.ty,
            values: self.values.into_iter().map(EnumValue::into_owned).collect(),
            docstring: self.docstring.map(Str::into_owned),
        }
    }
}

/// A single value of an [`Enum`] (`<tp:enumvalue>`).
#[derive(Debug, Clone, PartialEq)]
pub struct EnumValue<'a> {
    pub(crate) suffix: Str<'a>,
    pub(crate) value: Str<'a>,
    pub(crate) docstring: Option<Str<'a>>,
}

impl EnumValue<'_> {
    /// The name of the value.
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// The value itself — a number for numeric enumerations, or e. g. a string.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The docstring of the value, if any.
    pub fn docstring(&self) -> Option<&str> {
        self.docstring.as_deref()
    }

    /// Creates an owned clone of `self`.
    pub fn to_owned(&self) -> EnumValue<'static> {
        self.clone().into_owned()
    }

    /// Converts `self` into an owned tree, copying the strings it borrows.
    pub fn into_owned(self) -> EnumValue<'static> {
        EnumValue {
            suffix: self.suffix.into_owned(),
            value: self.value.into_owned(),
            docstring: self.docstring.map(Str::into_owned),
        }
    }
}

/// A named structure (`<tp:struct>`).
#[derive(Debug, Clone, PartialEq)]
pub struct Struct<'a> {
    pub(crate) name: Str<'a>,
    pub(crate) members: Vec<Member<'a>>,
    pub(crate) docstring: Option<Str<'a>>,
}

impl<'a> Struct<'a> {
    /// The name of the type.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The members of the structure.
    pub fn members(&self) -> &[Member<'a>] {
        &self.members
    }

    /// The docstring of the definition, if any.
    pub fn docstring(&self) -> Option<&str> {
        self.docstring.as_deref()
    }

    /// The D-Bus signature of the structure.
    pub fn signature(&self) -> Signature {
        Signature::structure(
            self.members
                .iter()
                .map(|m| m.ty().clone())
                .collect::<Vec<_>>(),
        )
    }

    /// Creates an owned clone of `self`.
    pub fn to_owned(&self) -> Struct<'static> {
        self.clone().into_owned()
    }

    /// Converts `self` into an owned tree, copying the strings it borrows.
    pub fn into_owned(self) -> Struct<'static> {
        Struct {
            name: self.name.into_owned(),
            members: self.members.into_iter().map(Member::into_owned).collect(),
            docstring: self.docstring.map(Str::into_owned),
        }
    }
}

/// A member of a [`Struct`] or [`Mapping`] (`<tp:member>`).
#[derive(Debug, Clone, PartialEq)]
pub struct Member<'a> {
    pub(crate) name: Str<'a>,
    pub(crate) ty: Signature,
    pub(crate) tp_type: Option<Str<'a>>,
    pub(crate) docstring: Option<Str<'a>>,
}

impl Member<'_> {
    /// The name of the member.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The D-Bus type of the member.
    pub fn ty(&self) -> &Signature {
        &self.ty
    }

    /// The named Telepathy type of the member (its `tp:type` attribute), if any.
    pub fn tp_type(&self) -> Option<&str> {
        self.tp_type.as_deref()
    }

    /// The docstring of the member, if any.
    pub fn docstring(&self) -> Option<&str> {
        self.docstring.as_deref()
    }

    /// Creates an owned clone of `self`.
    pub fn to_owned(&self) -> Member<'static> {
        self.clone().into_owned()
    }

    /// Converts `self` into an owned tree, copying the strings it borrows.
    pub fn into_owned(self) -> Member<'static> {
        Member {
            name: self.name.into_owned(),
            ty: self.ty,
            tp_type: self.tp_type.map(Str::into_owned),
            docstring: self.docstring.map(Str::into_owned),
        }
    }
}

/// A named dictionary type (`<tp:mapping>`).
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping<'a> {
    pub(crate) name: Str<'a>,
    pub(crate) key: Member<'a>,
    pub(crate) value: Member<'a>,
    pub(crate) docstring: Option<Str<'a>>,
}

impl<'a> Mapping<'a> {
    /// The name of the type.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The key member of the dictionary.
    pub fn key(&self) -> &Member<'a> {
        &self.key
    }

    /// The value member of the dictionary.
    pub fn value(&self) -> &Member<'a> {
        &self.value
    }

    /// The docstring of the definition, if any.
    pub fn docstring(&self) -> Option<&str> {
        self.docstring.as_deref()
    }

    /// The D-Bus signature of the dictionary.
    pub fn signature(&self) -> Signature {
        Signature::dict(self.key.ty().clone(), self.value.ty().clone())
    }

    /// Creates an owned clone of `self`.
    pub fn to_owned(&self) -> Mapping<'static> {
        self.clone().into_owned()
    }

    /// Converts `self` into an owned tree, copying the strings it borrows.
    pub fn into_owned(self) -> Mapping<'static> {
        Mapping {
            name: self.name.into_owned(),
            key: self.key.into_owned(),
            value: self.value.into_owned(),
            docstring: self.docstring.map(Str::into_owned),
        }
    }
}

/// Whether `ty` is a basic (i. e. non-container) D-Bus type, as required for dictionary keys.
pub(crate) fn is_basic(ty: &Signature) -> bool {
    use Signature as S;

    match ty {
        S::U8
        | S::Bool
        | S::I16
        | S::U16
        | S::I32
        | S::U32
        | S::I64
        | S::U64
        | S::F64
        | S::Str
        | S::Signature
        | S::ObjectPath => true,
        #[cfg(unix)]
        S::Fd => true,
        _ => false,
    }
}
