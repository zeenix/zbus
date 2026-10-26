use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error, SeqAccess, Visitor},
    ser::{SerializeSeq, SerializeStruct},
};
use std::{borrow::Cow, num::NonZeroU32, ops::Range, str};

use crate::{
    ObjectPath, Signature, Type, Variant,
    message::{FieldCode, Header},
    names::{BusName, ErrorName, InterfaceName, MemberName, UniqueName, WellKnownName},
};
use zbus_utils::object_path;

/// A collection of [`Field`] instances.
///
/// [`Field`]: enum.Field.html
#[derive(Debug, Default, Clone, Type)]
#[zbus(signature = "a(yv)")]
pub(crate) struct Fields<'f> {
    pub path: Option<ObjectPath<'f>>,
    pub interface: Option<InterfaceName<'f>>,
    pub member: Option<MemberName<'f>>,
    pub error_name: Option<ErrorName<'f>>,
    pub reply_serial: Option<NonZeroU32>,
    pub destination: Option<BusName<'f>>,
    pub sender: Option<UniqueName<'f>>,
    pub signature: Cow<'f, Signature>,
    pub unix_fds: Option<u32>,
}

impl Fields<'_> {
    /// Create an empty collection of fields.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Serialize for Fields<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(None)?;
        if let Some(path) = &self.path {
            seq.serialize_element(&(FieldCode::Path, Variant::from(path)))?;
        }
        if let Some(interface) = &self.interface {
            seq.serialize_element(&(FieldCode::Interface, Variant::from(interface.as_str())))?;
        }
        if let Some(member) = &self.member {
            seq.serialize_element(&(FieldCode::Member, Variant::from(member.as_str())))?;
        }
        if let Some(error_name) = &self.error_name {
            seq.serialize_element(&(FieldCode::ErrorName, Variant::from(error_name.as_str())))?;
        }
        if let Some(reply_serial) = self.reply_serial {
            seq.serialize_element(&(FieldCode::ReplySerial, Variant::from(reply_serial.get())))?;
        }
        if let Some(destination) = &self.destination {
            seq.serialize_element(&(FieldCode::Destination, Variant::from(destination.as_str())))?;
        }
        if let Some(sender) = &self.sender {
            seq.serialize_element(&(FieldCode::Sender, Variant::from(sender.as_str())))?;
        }
        if !matches!(&*self.signature, Signature::Unit) {
            seq.serialize_element(&(FieldCode::Signature, SignatureSerializer(&self.signature)))?;
        }
        if let Some(unix_fds) = self.unix_fds {
            seq.serialize_element(&(FieldCode::UnixFDs, Variant::from(unix_fds)))?;
        }
        seq.end()
    }
}

/// Our special serializer for [`Variant::Signature`].
///
/// Normally `Variant` would use the default serializer for `Signature`, which will include the `()`
/// for strucutures but for body signature, that's what what the D-Bus expects so we do the same as
/// `Variant` here, except we serialize signature value as string w/o the `()`.
#[derive(Debug, Type)]
#[zbus(signature = "v")]
struct SignatureSerializer<'a>(&'a Signature);

impl Serialize for SignatureSerializer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut structure = serializer.serialize_struct("Variant", 2)?;

        structure.serialize_field("signature", &Signature::Signature)?;

        let signature_str = self.0.to_string_no_parens();
        structure.serialize_field("value", &signature_str)?;

        structure.end()
    }
}

impl<'de: 'f, 'f> Deserialize<'de> for Fields<'f> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(FieldsVisitor)
    }
}

struct FieldsVisitor;

impl<'de> Visitor<'de> for FieldsVisitor {
    type Value = Fields<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("D-Bus message header fields")
    }

    fn visit_seq<V>(self, mut visitor: V) -> Result<Fields<'de>, V::Error>
    where
        V: SeqAccess<'de>,
    {
        let mut fields = Fields::new();
        while let Some((code, value)) = visitor.next_element::<(FieldCode, Variant<'de>)>()? {
            match code {
                FieldCode::Path => {
                    let path = ObjectPath::try_from(value).map_err(V::Error::custom)?;
                    // A variant doesn't check the object path it holds, unlike the name fields.
                    object_path::validate(path.as_bytes())
                        .map_err(|_| V::Error::custom(crate::Error::InvalidObjectPath))?;
                    fields.path = Some(path)
                }
                FieldCode::Interface => {
                    fields.interface =
                        Some(InterfaceName::try_from(value).map_err(V::Error::custom)?)
                }
                FieldCode::Member => {
                    fields.member = Some(MemberName::try_from(value).map_err(V::Error::custom)?)
                }
                FieldCode::ErrorName => {
                    fields.error_name = Some(ErrorName::try_from(value).map_err(V::Error::custom)?)
                }
                FieldCode::ReplySerial => {
                    let value = u32::try_from(value)
                        .map_err(V::Error::custom)
                        .and_then(|v| v.try_into().map_err(V::Error::custom))?;
                    fields.reply_serial = Some(value)
                }
                FieldCode::Destination => {
                    fields.destination = Some(BusName::try_from(value).map_err(V::Error::custom)?)
                }
                FieldCode::Sender => {
                    fields.sender = Some(UniqueName::try_from(value).map_err(V::Error::custom)?)
                }
                FieldCode::Signature => {
                    fields.signature =
                        Cow::Owned(Signature::try_from(value).map_err(V::Error::custom)?)
                }
                FieldCode::UnixFDs => {
                    fields.unix_fds = Some(u32::try_from(value).map_err(V::Error::custom)?)
                }
            }
        }

        Ok(fields)
    }
}

/// A byte range of a field in a Message, used in [`QuickFields`].
///
/// Some invalid encodings (end = 0) are used to indicate "not cached" and "not present".
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct FieldPos {
    start: u32,
    end: u32,
}

impl FieldPos {
    pub fn new_not_present() -> Self {
        Self { start: 1, end: 0 }
    }

    pub fn build(msg_buf: &[u8], field_buf: &str) -> Option<Self> {
        let buf_start = msg_buf.as_ptr() as usize;
        let field_start = field_buf.as_ptr() as usize;
        let offset = field_start.checked_sub(buf_start)?;
        if offset <= msg_buf.len() && offset + field_buf.len() <= msg_buf.len() {
            Some(Self {
                start: offset.try_into().ok()?,
                end: (offset + field_buf.len()).try_into().ok()?,
            })
        } else {
            None
        }
    }

    pub fn new<T>(msg_buf: &[u8], field: Option<&T>) -> Self
    where
        T: std::ops::Deref<Target = str>,
    {
        field
            .and_then(|f| Self::build(msg_buf, f.deref()))
            .unwrap_or_else(Self::new_not_present)
    }

    /// The byte range of the field in the message, if it's present.
    fn range(self) -> Option<Range<usize>> {
        match self {
            Self {
                start: 0..=1,
                end: 0,
            } => None,
            Self { start, end } => Some(start as usize..end as usize),
        }
    }
}

/// A cache of the Message header fields.
#[derive(Debug, Default, Clone)]
pub(crate) struct QuickFields {
    path: FieldPos,
    interface: FieldPos,
    member: FieldPos,
    error_name: FieldPos,
    reply_serial: Option<NonZeroU32>,
    destination: FieldPos,
    /// Whether the destination is a unique name, rather than a well-known one.
    unique_destination: bool,
    sender: FieldPos,
    signature: Signature,
    unix_fds: Option<u32>,
}

impl QuickFields {
    pub fn new(buf: &[u8], header: &Header<'_>) -> Self {
        Self {
            path: FieldPos::new(buf, header.path()),
            interface: FieldPos::new(buf, header.interface()),
            member: FieldPos::new(buf, header.member()),
            error_name: FieldPos::new(buf, header.error_name()),
            reply_serial: header.reply_serial(),
            destination: FieldPos::new(buf, header.destination()),
            unique_destination: matches!(header.destination(), Some(BusName::Unique(_))),
            sender: FieldPos::new(buf, header.sender()),
            signature: header.signature().clone(),
            unix_fds: header.unix_fds(),
        }
    }

    /// The fields, borrowed from the bytes of the message.
    ///
    /// The fields were checked when they were decoded, so they're not checked again.
    ///
    /// # Safety
    ///
    /// `bytes` must be the bytes of the message that `self` was created for.
    pub unsafe fn fields<'m>(&'m self, bytes: &'m [u8]) -> Fields<'m> {
        let read = |pos: FieldPos| {
            pos.range().map(|range| {
                // SAFETY: The caller ensures that `bytes` are those that `pos` was created
                // for, from a string of them.
                unsafe { str::from_utf8_unchecked(&bytes[range]) }
            })
        };

        Fields {
            path: read(self.path).map(ObjectPath::from_str_unchecked),
            interface: read(self.interface).map(InterfaceName::from_str_unchecked),
            member: read(self.member).map(MemberName::from_str_unchecked),
            error_name: read(self.error_name).map(ErrorName::from_str_unchecked),
            reply_serial: self.reply_serial,
            destination: read(self.destination).map(|name| {
                if self.unique_destination {
                    BusName::Unique(UniqueName::from_str_unchecked(name))
                } else {
                    BusName::WellKnown(WellKnownName::from_str_unchecked(name))
                }
            }),
            sender: read(self.sender).map(UniqueName::from_str_unchecked),
            signature: Cow::Borrowed(&self.signature),
            unix_fds: self.unix_fds,
        }
    }

    pub fn signature(&self) -> &Signature {
        &self.signature
    }
}
