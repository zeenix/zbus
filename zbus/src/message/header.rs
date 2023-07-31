use std::convert::TryFrom;

use enumflags2::{bitflags, BitFlags};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use static_assertions::assert_impl_all;
use zbus_names::{BusName, ErrorName, InterfaceName, MemberName, UniqueName};
use zvariant::{EncodingContext, ObjectPath, Signature, Type as VariantType};

use crate::{
    message::{Field, FieldCode, Fields},
    Error,
};

pub(crate) const PRIMARY_HEADER_SIZE: usize = 12;
pub(crate) const MIN_MESSAGE_SIZE: usize = PRIMARY_HEADER_SIZE + 4;
pub(crate) const MAX_MESSAGE_SIZE: usize = 128 * 1024 * 1024; // 128 MiB

/// D-Bus code for endianness.
#[repr(u8)]
#[derive(Debug, Copy, Clone, Deserialize_repr, PartialEq, Eq, Serialize_repr, VariantType)]
pub enum EndianSig {
    /// The D-Bus message is in big-endian (network) byte order.
    Big = b'B',

    /// The D-Bus message is in little-endian byte order.
    Little = b'l',
}

assert_impl_all!(EndianSig: Send, Sync, Unpin);

// Such a shame I've to do this manually
impl TryFrom<u8> for EndianSig {
    type Error = Error;

    fn try_from(val: u8) -> Result<EndianSig, Error> {
        match val {
            b'B' => Ok(EndianSig::Big),
            b'l' => Ok(EndianSig::Little),
            _ => Err(Error::IncorrectEndian),
        }
    }
}

#[cfg(target_endian = "big")]
/// Signature of the target's native endian.
pub const NATIVE_ENDIAN_SIG: EndianSig = EndianSig::Big;
#[cfg(target_endian = "little")]
/// Signature of the target's native endian.
pub const NATIVE_ENDIAN_SIG: EndianSig = EndianSig::Little;

/// Message header representing the D-Bus type of the message.
#[repr(u8)]
#[derive(
    Debug, Copy, Clone, Deserialize_repr, PartialEq, Eq, Hash, Serialize_repr, VariantType,
)]
pub enum Type {
    /// Invalid message type. All unknown types on received messages are treated as invalid.
    Invalid = 0,
    /// Method call. This message type may prompt a reply (and typically does).
    MethodCall = 1,
    /// A reply to a method call.
    MethodReturn = 2,
    /// An error in response to a method call.
    Error = 3,
    /// Signal emission.
    Signal = 4,
}

assert_impl_all!(Type: Send, Sync, Unpin);

// Such a shame I've to do this manually
impl From<u8> for Type {
    fn from(val: u8) -> Type {
        match val {
            1 => Type::MethodCall,
            2 => Type::MethodReturn,
            3 => Type::Error,
            4 => Type::Signal,
            _ => Type::Invalid,
        }
    }
}

/// Pre-defined flags that can be passed in Message header.
#[bitflags]
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, VariantType)]
pub enum Flags {
    /// This message does not expect method return replies or error replies, even if it is of a
    /// type that can have a reply; the reply should be omitted.
    ///
    /// Note that `Type::MethodCall` is the only message type currently defined in the
    /// specification that can expect a reply, so the presence or absence of this flag in the other
    /// three message types that are currently documented is meaningless: replies to those message
    /// types should not be sent, whether this flag is present or not.
    NoReplyExpected = 0x1,
    /// The bus must not launch an owner for the destination name in response to this message.
    NoAutoStart = 0x2,
    /// This flag may be set on a method call message to inform the receiving side that the caller
    /// is prepared to wait for interactive authorization, which might take a considerable time to
    /// complete. For instance, if this flag is set, it would be appropriate to query the user for
    /// passwords or confirmation via Polkit or a similar framework.
    AllowInteractiveAuth = 0x4,
}

assert_impl_all!(Flags: Send, Sync, Unpin);

#[derive(Clone, Debug)]
struct SerialNum(OnceCell<u32>);

// FIXME: Can use `zvariant::Type` macro after `zvariant` provides a blanket implementation for
// `OnceCell<T>`.
impl zvariant::Type for SerialNum {
    fn signature() -> Signature<'static> {
        u32::signature()
    }
}

// Unfortunately Serde doesn't provide a blanket impl. for `Cell<T>` so we have to implement
// manually.
//
// https://github.com/serde-rs/serde/issues/1952
impl Serialize for SerialNum {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // `Message` serializes the PrimaryHeader at construct time before the user has the
        // time to tweak it and set a correct serial_num. We should probably avoid this but
        // for now, let's silently use a default serialized value.
        self.0
            .get()
            .cloned()
            .unwrap_or_default()
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SerialNum {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(SerialNum(OnceCell::from(u32::deserialize(deserializer)?)))
    }
}

/// The primary message header, which is present in all D-Bus messages.
///
/// This header contains all the essential information about a message, regardless of its type.
#[derive(Clone, Debug, Serialize, Deserialize, VariantType)]
pub struct PrimaryHeader {
    endian_sig: EndianSig,
    msg_type: Type,
    flags: BitFlags<Flags>,
    protocol_version: u8,
    body_len: u32,
    serial_num: SerialNum,
}

assert_impl_all!(PrimaryHeader: Send, Sync, Unpin);

impl PrimaryHeader {
    /// Create a new `PrimaryHeader` instance.
    pub fn new(msg_type: Type, body_len: u32) -> Self {
        Self {
            endian_sig: NATIVE_ENDIAN_SIG,
            msg_type,
            flags: BitFlags::empty(),
            protocol_version: 1,
            body_len,
            serial_num: SerialNum(OnceCell::new()),
        }
    }

    pub(crate) fn read(buf: &[u8]) -> Result<(PrimaryHeader, u32), Error> {
        let ctx = EncodingContext::<byteorder::NativeEndian>::new_dbus(0);
        let (primary_header, size) = zvariant::from_slice(buf, ctx)?;
        assert_eq!(size, PRIMARY_HEADER_SIZE);
        let (fields_len, _) = zvariant::from_slice(&buf[PRIMARY_HEADER_SIZE..], ctx)?;
        Ok((primary_header, fields_len))
    }

    /// D-Bus code for bytorder encoding of the message.
    pub fn endian_sig(&self) -> EndianSig {
        self.endian_sig
    }

    /// Set the D-Bus code for bytorder encoding of the message.
    pub fn set_endian_sig(&mut self, sig: EndianSig) {
        self.endian_sig = sig;
    }

    /// The message type.
    pub fn msg_type(&self) -> Type {
        self.msg_type
    }

    /// Set the message type.
    pub fn set_msg_type(&mut self, msg_type: Type) {
        self.msg_type = msg_type;
    }

    /// The message flags.
    pub fn flags(&self) -> BitFlags<Flags> {
        self.flags
    }

    /// Set the message flags.
    pub fn set_flags(&mut self, flags: BitFlags<Flags>) {
        self.flags = flags;
    }

    /// The major version of the protocol the message is compliant to.
    ///
    /// Currently only `1` is valid.
    pub fn protocol_version(&self) -> u8 {
        self.protocol_version
    }

    /// Set the major version of the protocol the message is compliant to.
    ///
    /// Currently only `1` is valid.
    pub fn set_protocol_version(&mut self, version: u8) {
        self.protocol_version = version;
    }

    /// The byte length of the message body.
    pub fn body_len(&self) -> u32 {
        self.body_len
    }

    /// Set the byte length of the message body.
    pub fn set_body_len(&mut self, len: u32) {
        self.body_len = len;
    }

    /// The serial number of the message (if set).
    ///
    /// This is used to match a reply to a method call.
    ///
    /// **Note:** There is no setter provided for this in the public API since this is set by the
    /// [`Connection`](struct.Connection.html) the message is sent over.
    pub fn serial_num(&self) -> Option<&u32> {
        self.serial_num.0.get()
    }

    pub(crate) fn serial_num_or_init<F>(&mut self, f: F) -> &u32
    where
        F: FnOnce() -> u32,
    {
        self.serial_num.0.get_or_init(f)
    }
}

/// The message header, containing all the metadata about the message.
///
/// This includes both the [`PrimaryHeader`] and [`Fields`].
///
/// [`PrimaryHeader`]: struct.PrimaryHeader.html
/// [`Fields`]: struct.Fields.html
#[derive(Debug, Clone, Serialize, Deserialize, VariantType)]
pub struct Header<'m> {
    primary: PrimaryHeader,
    #[serde(borrow)]
    fields: Fields<'m>,
}

assert_impl_all!(Header<'_>: Send, Sync, Unpin);

macro_rules! get_field {
    ($self:ident, $kind:ident) => {
        get_field!($self, $kind, (|v| v))
    };
    ($self:ident, $kind:ident, $closure:tt) => {
        #[allow(clippy::redundant_closure_call)]
        match $self.fields().get_field(FieldCode::$kind) {
            Some(Field::$kind(value)) => Ok(Some($closure(value))),
            Some(_) => Err(Error::InvalidField),
            None => Ok(None),
        }
    };
}

macro_rules! get_field_u32 {
    ($self:ident, $kind:ident) => {
        get_field!($self, $kind, (|v: &u32| *v))
    };
}

impl<'m> Header<'m> {
    /// Create a new `Header` instance.
    pub fn new(primary: PrimaryHeader, fields: Fields<'m>) -> Self {
        Self { primary, fields }
    }

    /// Get a reference to the primary header.
    pub fn primary(&self) -> &PrimaryHeader {
        &self.primary
    }

    /// Get a mutable reference to the primary header.
    pub fn primary_mut(&mut self) -> &mut PrimaryHeader {
        &mut self.primary
    }

    /// Get the primary header, consuming `self`.
    pub fn into_primary(self) -> PrimaryHeader {
        self.primary
    }

    /// Get a reference to the message fields.
    pub fn fields(&self) -> &Fields<'m> {
        &self.fields
    }

    /// Get a mutable reference to the message fields.
    pub fn fields_mut(&mut self) -> &mut Fields<'m> {
        &mut self.fields
    }

    /// Get the message fields, consuming `self`.
    pub fn into_fields(self) -> Fields<'m> {
        self.fields
    }

    /// The message type
    pub fn message_type(&self) -> Result<Type, Error> {
        Ok(self.primary().msg_type())
    }

    /// The object to send a call to, or the object a signal is emitted from.
    pub fn path(&self) -> Result<Option<&ObjectPath<'m>>, Error> {
        get_field!(self, Path)
    }

    /// The interface to invoke a method call on, or that a signal is emitted from.
    pub fn interface(&self) -> Result<Option<&InterfaceName<'m>>, Error> {
        get_field!(self, Interface)
    }

    /// The member, either the method name or signal name.
    pub fn member(&self) -> Result<Option<&MemberName<'m>>, Error> {
        get_field!(self, Member)
    }

    /// The name of the error that occurred, for errors.
    pub fn error_name(&self) -> Result<Option<&ErrorName<'m>>, Error> {
        get_field!(self, ErrorName)
    }

    /// The serial number of the message this message is a reply to.
    pub fn reply_serial(&self) -> Result<Option<u32>, Error> {
        get_field_u32!(self, ReplySerial)
    }

    /// The name of the connection this message is intended for.
    pub fn destination(&self) -> Result<Option<&BusName<'m>>, Error> {
        get_field!(self, Destination)
    }

    /// Unique name of the sending connection.
    pub fn sender(&self) -> Result<Option<&UniqueName<'m>>, Error> {
        get_field!(self, Sender)
    }

    /// The signature of the message body.
    pub fn signature(&self) -> Result<Option<&Signature<'m>>, Error> {
        get_field!(self, Signature)
    }

    /// The number of Unix file descriptors that accompany the message.
    pub fn unix_fds(&self) -> Result<Option<u32>, Error> {
        get_field_u32!(self, UnixFDs)
    }
}

#[cfg(test)]
mod tests {
    use crate::message::{Field, Fields, Header, PrimaryHeader, Type};

    use std::{
        convert::{TryFrom, TryInto},
        error::Error,
        result::Result,
    };
    use test_log::test;
    use zbus_names::{InterfaceName, MemberName};
    use zvariant::{ObjectPath, Signature};

    #[test]
    fn header() -> Result<(), Box<dyn Error>> {
        let path = ObjectPath::try_from("/some/path")?;
        let iface = InterfaceName::try_from("some.interface")?;
        let member = MemberName::try_from("Member")?;
        let mut f = Fields::new();
        f.add(Field::Path(path.clone()));
        f.add(Field::Interface(iface.clone()));
        f.add(Field::Member(member.clone()));
        f.add(Field::Sender(":1.84".try_into()?));
        let h = Header::new(PrimaryHeader::new(Type::Signal, 77), f);

        assert_eq!(h.message_type()?, Type::Signal);
        assert_eq!(h.path()?, Some(&path));
        assert_eq!(h.interface()?, Some(&iface));
        assert_eq!(h.member()?, Some(&member));
        assert_eq!(h.error_name()?, None);
        assert_eq!(h.destination()?, None);
        assert_eq!(h.reply_serial()?, None);
        assert_eq!(h.sender()?.unwrap(), ":1.84");
        assert_eq!(h.signature()?, None);
        assert_eq!(h.unix_fds()?, None);

        let mut f = Fields::new();
        f.add(Field::ErrorName("org.zbus.Error".try_into()?));
        f.add(Field::Destination(":1.11".try_into()?));
        f.add(Field::ReplySerial(88));
        f.add(Field::Signature(Signature::from_str_unchecked("say")));
        f.add(Field::UnixFDs(12));
        let h = Header::new(PrimaryHeader::new(Type::MethodReturn, 77), f);

        assert_eq!(h.message_type()?, Type::MethodReturn);
        assert_eq!(h.path()?, None);
        assert_eq!(h.interface()?, None);
        assert_eq!(h.member()?, None);
        assert_eq!(h.error_name()?.unwrap(), "org.zbus.Error");
        assert_eq!(h.destination()?.unwrap(), ":1.11");
        assert_eq!(h.reply_serial()?, Some(88));
        assert_eq!(h.sender()?, None);
        assert_eq!(h.signature()?, Some(&Signature::from_str_unchecked("say")));
        assert_eq!(h.unix_fds()?, Some(12));

        Ok(())
    }
}
