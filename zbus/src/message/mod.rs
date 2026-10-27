//! D-Bus Message.
use std::{fmt, ops::Range, sync::Arc};

use crate::{
    Error, ObjectPath, Result,
    names::{ErrorName, InterfaceName, MemberName},
    utils::padding_for_8_bytes,
    wire::{Endian, serialized},
};

mod builder;
pub use builder::Builder;

mod field_code;
pub(crate) use field_code::FieldCode;

mod fields;
pub(crate) use fields::Fields;

mod body;
pub use body::Body;

pub(crate) mod header;
use header::PROTOCOL_VERSION;
pub use header::{EndianSig, Flags, Header, NATIVE_ENDIAN_SIG, PrimaryHeader, Type};

pub(crate) mod parser;
use parser::QuickFields;

/// A position in the stream of [`Message`] objects received by a single [`zbus::Connection`].
///
/// Note: the relative ordering of values obtained from distinct [`zbus::Connection`] objects is
/// not specified; only sequence numbers originating from the same connection should be compared.
#[derive(Debug, Default, Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub struct Sequence {
    recv_seq: u64,
}

impl Sequence {
    /// A sequence number that is higher than any other; used by errors that terminate a stream.
    pub(crate) const LAST: Self = Self { recv_seq: u64::MAX };
}

/// A D-Bus Message.
///
/// The contents of the message are stored in serialized format. To get the body of the message, use
/// the [`Message::body`] method, and use [`Body`] methods to deserialize it. You may also access
/// the header and other details with the various other getters.
///
/// Also provided are constructors for messages of different types. These will mainly be useful for
/// very advanced use cases as typically you will want to create a message for immediate dispatch
/// and hence use the API provided by [`crate::connection::Connection::call_method`], even when
/// using the low-level API.
///
/// **Note**: The message owns the received FDs and will close them when dropped. You can
/// deserialize the body (that you get using [`Message::body`]) to [`crate::OwnedFd`] if
/// you want to keep the FDs around after the containing message is dropped.
#[derive(Clone)]
pub struct Message {
    pub(super) inner: Arc<Inner>,
}

pub(super) struct Inner {
    pub(crate) primary_header: PrimaryHeader,
    /// The fields of the header, as found in `bytes`, once they've been looked for.
    quick_fields: std::sync::OnceLock<QuickFields>,
    pub(crate) bytes: serialized::Data<'static, 'static>,
    pub(crate) body_offset: usize,
    pub(crate) recv_seq: Sequence,
}

impl Message {
    /// Create a builder for a message of type [`Type::MethodCall`].
    ///
    /// An invalid path or method name is reported by [`Builder::build`].
    #[must_use]
    pub fn method_call<'b, 'p: 'b, 'm: 'b, P, M>(path: P, method_name: M) -> Builder<'b>
    where
        P: TryInto<ObjectPath<'p>>,
        M: TryInto<MemberName<'m>>,
        P::Error: Into<Error>,
        M::Error: Into<Error>,
    {
        Builder::new(Type::MethodCall)
            .path(path)
            .member(method_name)
    }

    /// Create a builder for a message of type [`Type::Signal`].
    ///
    /// An invalid path, interface name or signal name is reported by [`Builder::build`].
    #[must_use]
    pub fn signal<'b, 'p: 'b, 'i: 'b, 'm: 'b, P, I, M>(
        path: P,
        iface: I,
        signal_name: M,
    ) -> Builder<'b>
    where
        P: TryInto<ObjectPath<'p>>,
        I: TryInto<InterfaceName<'i>>,
        M: TryInto<MemberName<'m>>,
        P::Error: Into<Error>,
        I::Error: Into<Error>,
        M::Error: Into<Error>,
    {
        Builder::new(Type::Signal)
            .path(path)
            .interface(iface)
            .member(signal_name)
    }

    /// Create a builder for a message of type [`Type::MethodReturn`].
    #[must_use]
    pub fn method_return(reply_to: &Header<'_>) -> Builder<'static> {
        Builder::new(Type::MethodReturn).reply_to(reply_to)
    }

    /// Create a builder for a message of type [`Type::Error`].
    ///
    /// An invalid error name is reported by [`Builder::build`].
    #[must_use]
    pub fn error<'b, 'e: 'b, E>(reply_to: &Header<'_>, name: E) -> Builder<'b>
    where
        E: TryInto<ErrorName<'e>>,
        E::Error: Into<Error>,
    {
        Builder::new(Type::Error)
            .reply_to(reply_to)
            .error_name(name)
    }

    /// Create a message from bytes.
    ///
    /// **Note:** Since the message is not constructed by zbus, the receive sequence,
    /// which can be acquired from [`Message::recv_position`], is not applicable and hence set
    /// to `0`.
    ///
    /// # Safety
    ///
    /// This method is unsafe as bytes may have an invalid encoding.
    pub unsafe fn from_bytes(bytes: serialized::Data<'static, 'static>) -> Result<Self> {
        Self::from_raw_parts(bytes, 0)
    }

    /// Create a message from its full contents.
    pub(crate) fn from_raw_parts(
        bytes: serialized::Data<'static, 'static>,
        recv_seq: u64,
    ) -> Result<Self> {
        let (primary_header, header_len, quick_fields) = parser::parse(&bytes)?;
        if Endian::from(primary_header.endian_sig()) != bytes.context().endian() {
            return Err(Error::IncorrectEndian);
        }

        let body_offset = header_len + padding_for_8_bytes(header_len);
        check_whole(
            &primary_header,
            &quick_fields,
            &bytes,
            header_len..body_offset,
        )?;
        let quick_fields = quick_fields.into();

        Ok(Self {
            inner: Arc::new(Inner {
                primary_header,
                quick_fields,
                bytes,
                body_offset,
                recv_seq: Sequence { recv_seq },
            }),
        })
    }

    /// The primary header of the message.
    ///
    /// This is the part of the message header that every message has, whatever its type.
    /// [`Message::header`] returns the message header as a whole.
    pub fn primary_header(&self) -> &PrimaryHeader {
        &self.inner.primary_header
    }

    /// The message header.
    pub fn header(&self) -> Header<'_> {
        // SAFETY: The quick fields of a message are only ever created from its own bytes.
        let fields = unsafe { self.quick_fields().fields(&self.inner.bytes) };

        Header::new(self.inner.primary_header.clone(), fields)
    }

    /// The message type.
    pub fn message_type(&self) -> Type {
        self.inner.primary_header.msg_type()
    }

    /// The body that you can deserialize using [`Body::deserialize`].
    ///
    /// # Example
    ///
    /// ```
    /// # use zbus::message::Message;
    /// # (|| -> zbus::Result<()> {
    /// let send_body = (7i32, (2i32, "foo"), vec!["bar"]);
    /// let message = Message::method_call("/", "ping")
    ///     .destination("zbus.test")
    ///     .interface("zbus.test")
    ///     .build(&send_body)?;
    /// let header = message.header();
    /// let body = message.body();
    /// let body: zbus::Structure<'_> = body.deserialize()?;
    /// let fields = body.fields();
    /// assert!(matches!(fields[0], zbus::Variant::I32(7)));
    /// assert!(matches!(fields[1], zbus::Variant::Structure(_)));
    /// assert!(matches!(fields[2], zbus::Variant::Array(_)));
    ///
    /// let reply_body = Message::method_return(&header).build(&body)?.body();
    /// let reply_value : (i32, (i32, &str), Vec<String>) = reply_body.deserialize()?;
    ///
    /// assert_eq!(reply_value.0, 7);
    /// assert_eq!(reply_value.2.len(), 1);
    /// # Ok(()) })().unwrap()
    /// ```
    pub fn body(&self) -> Body {
        Body::new(
            self.inner.bytes.slice(self.inner.body_offset..),
            self.clone(),
        )
    }

    /// Get a reference to the underlying byte encoding of the message.
    pub fn data(&self) -> &serialized::Data<'static, 'static> {
        &self.inner.bytes
    }

    /// Get the receive ordering of a message.
    ///
    /// This may be used to identify how two events were ordered on the bus. It only produces a
    /// useful ordering for messages that were produced by the same [`zbus::Connection`].
    ///
    /// This is completely unrelated to the serial number on the message, which is set by the peer
    /// and might not be ordered at all.
    pub fn recv_position(&self) -> Sequence {
        self.inner.recv_seq
    }

    /// Check the body against its signature, as [`Body::validate`] does, without making a [`Body`]
    /// of it, which takes more than a check of a small body does.
    #[cfg(feature = "bus-impl")]
    pub(crate) fn validate_body(&self) -> Result<()> {
        let bytes = self.inner.bytes.bytes();
        let quick_fields = self.quick_fields();
        #[cfg(unix)]
        let fds = quick_fields.unix_fds().unwrap_or(0);
        // Without file descriptors, a signature can't have them.
        #[cfg(not(unix))]
        let fds = 0;

        parser::validate_body(
            &bytes[self.inner.body_offset..],
            quick_fields.signature_bytes(bytes),
            self.inner.primary_header.endian_sig(),
            fds,
        )
    }

    fn quick_fields(&self) -> &QuickFields {
        // Only a message that zbus built has its header parsed this late, and its header is valid,
        // unless it was built with a name or path from an unchecked constructor, such as
        // `ObjectPath::from_str_unchecked`.
        self.inner.quick_fields.get_or_init(|| {
            let (_, _, quick_fields) =
                parser::parse(&self.inner.bytes).expect("Invalid field in a built message");

            quick_fields
        })
    }
}

/// Check what makes the message in `bytes` whole, beyond each field of its header, which parsing
/// the header checks: its protocol version, the fields that its type requires, the padding between
/// its header and its body, at `padding`, which must be 0, and, on Unix, that it brought as many
/// file descriptors as its header says.
///
/// Only a received message is checked so: one that zbus builds may lack a field that the
/// specification requires, if it was built so.
fn check_whole(
    primary_header: &PrimaryHeader,
    quick_fields: &QuickFields,
    bytes: &serialized::Data<'static, 'static>,
    padding: Range<usize>,
) -> Result<()> {
    if primary_header.protocol_version() != PROTOCOL_VERSION {
        return Err(Error::InvalidField);
    }
    quick_fields.check_required(primary_header.msg_type())?;
    let Some(padding) = bytes.get(padding) else {
        return Err(Error::OutOfBounds);
    };
    if let Some(byte) = padding.iter().find(|byte| **byte != 0) {
        return Err(Error::PaddingNot0(*byte));
    }
    #[cfg(unix)]
    if bytes.fds().len() < quick_fields.unix_fds().unwrap_or(0) as usize {
        return Err(Error::MissingParameter("Missing file descriptors"));
    }

    Ok(())
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut msg = f.debug_struct("Msg");
        let h = self.header();
        msg.field("type", &h.message_type());
        msg.field("serial", &self.primary_header().serial_num());
        if let Some(sender) = h.sender() {
            msg.field("sender", &sender);
        }
        if let Some(serial) = h.reply_serial() {
            msg.field("reply-serial", &serial);
        }
        if let Some(path) = h.path() {
            msg.field("path", &path);
        }
        if let Some(iface) = h.interface() {
            msg.field("iface", &iface);
        }
        if let Some(member) = h.member() {
            msg.field("member", &member);
        }
        match self.body().signature() {
            crate::Signature::Unit => (),
            s => {
                msg.field("body", &s);
            }
        }
        #[cfg(unix)]
        {
            msg.field("fds", &self.data().fds());
        }
        msg.finish()
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header = self.header();
        let (ty, error_name, sender, member) = (
            header.message_type(),
            header.error_name(),
            header.sender(),
            header.member(),
        );

        match ty {
            Type::MethodCall => {
                write!(f, "Method call")?;
                if let Some(m) = member {
                    write!(f, " {m}")?;
                }
            }
            Type::MethodReturn => {
                write!(f, "Method return")?;
            }
            Type::Error => {
                write!(f, "Error")?;
                if let Some(e) = error_name {
                    write!(f, " {e}")?;
                }

                let body = self.body();
                let msg = body.deserialize_ignore_signature::<&str>();
                if let Ok(msg) = msg {
                    write!(f, ": {msg}")?;
                }
            }
            Type::Signal => {
                write!(f, "Signal")?;
                if let Some(m) = member {
                    write!(f, " {m}")?;
                }
            }
        }

        if let Some(s) = sender {
            write!(f, " from {s}")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use crate::Fd;
    use crate::Signature;
    #[cfg(unix)]
    use std::os::fd::{AsFd, AsRawFd};
    use test_log::test;

    use super::{Message, Type, parser};
    use crate::{Error, wire::serialized::Data};

    #[test]
    fn test() {
        #[cfg(unix)]
        let stdout = std::io::stdout();
        let m = Message::method_call("/", "do")
            .sender(":1.72")
            .build(&(
                #[cfg(unix)]
                Fd::from(&stdout),
                "foo",
            ))
            .unwrap();
        #[cfg(unix)]
        assert_eq!(
            m.body().signature(),
            &Signature::static_structure(&[&Signature::Fd, &Signature::Str]),
        );
        #[cfg(not(unix))]
        assert_eq!(m.body().signature(), &Signature::Str,);
        #[cfg(unix)]
        {
            let fds = m.data().fds();
            assert_eq!(fds.len(), 1);
            // FDs get dup'ed so it has to be a different FD now.
            assert_ne!(fds[0].as_fd().as_raw_fd(), stdout.as_raw_fd());
        }

        let body: Result<u32, Error> = m.body().deserialize();
        assert!(matches!(body.unwrap_err(), Error::SignatureMismatch(..)));

        assert_eq!(m.to_string(), "Method call do from :1.72");
        let r = Message::method_return(&m.header())
            .build(&("all fine!"))
            .unwrap();
        assert_eq!(r.to_string(), "Method return");
        let e = Message::error(&m.header(), "org.freedesktop.zbus.Error")
            .build(&("kaboom!", 32))
            .unwrap();
        assert_eq!(e.to_string(), "Error org.freedesktop.zbus.Error: kaboom!");
    }

    #[test]
    fn invalid_path_field() {
        // `/org.zbus/Test` is not a valid object path, so a message with it as its path is
        // rejected as it's received, rather than reading its header failing later.
        let m = Message::method_call("/org/zbus/Test", "Ping")
            .build(&())
            .unwrap();
        let mut bytes = m.data().bytes().to_vec();
        let path = bytes
            .windows(14)
            .position(|w| w == b"/org/zbus/Test")
            .unwrap();
        bytes[path + 4] = b'.';
        let data = Data::new(bytes, m.data().context());

        let error = Message::from_raw_parts(data, 0).unwrap_err();
        assert_eq!(error, Error::InvalidObjectPath);
    }

    #[test]
    fn protocol_version() {
        let m = Message::method_call("/org/zbus/Test", "Ping")
            .build(&())
            .unwrap();
        let mut bytes = m.data().bytes().to_vec();
        bytes[3] = 2;
        let data = Data::new(bytes, m.data().context());

        let error = Message::from_raw_parts(data, 0).unwrap_err();
        assert_eq!(error, Error::InvalidField);
    }

    #[test]
    fn required_fields() {
        // A method call with only the fields that it requires, which aren't those that another
        // type of message requires.
        let m = Message::method_call("/org/zbus/Test", "Ping")
            .build(&())
            .unwrap();
        Message::from_raw_parts(m.data().clone(), 0).unwrap();
        for msg_type in [Type::MethodReturn, Type::Error, Type::Signal] {
            let mut bytes = m.data().bytes().to_vec();
            bytes[1] = msg_type as u8;
            let data = Data::new(bytes, m.data().context());

            let error = Message::from_raw_parts(data, 0).unwrap_err();
            assert_eq!(error, Error::MissingField, "{msg_type:?}");
        }

        // The builder makes a reply without a reply serial, if asked to.
        let reply = Message::method_return(&m.header())
            .reply_serial(None)
            .build(&())
            .unwrap();
        let error = Message::from_raw_parts(reply.data().clone(), 0).unwrap_err();
        assert_eq!(error, Error::MissingField);
    }

    #[test]
    fn body_padding() {
        let m = Message::method_call("/org/zbus/Test", "Ping")
            .build(&"Pong")
            .unwrap();
        let (_, header_len, _) = parser::parse(m.data()).unwrap();
        assert_ne!(
            header_len, m.inner.body_offset,
            "the header needs no padding"
        );

        let mut bytes = m.data().bytes().to_vec();
        bytes[header_len] = 7;
        let data = Data::new(bytes, m.data().context());
        let error = Message::from_raw_parts(data, 0).unwrap_err();
        assert_eq!(error, Error::PaddingNot0(7));

        // Bytes that end with the header, without the padding after it.
        let bytes = m.data().bytes()[..header_len].to_vec();
        let data = Data::new(bytes, m.data().context());
        let error = Message::from_raw_parts(data, 0).unwrap_err();
        assert_eq!(error, Error::OutOfBounds);
    }

    #[cfg(unix)]
    #[test]
    fn missing_fds() {
        let stdout = std::io::stdout();
        let m = Message::method_call("/org/zbus/Test", "Ping")
            .build(&(Fd::from(&stdout), Fd::from(&stdout)))
            .unwrap();
        let fds = m
            .data()
            .fds()
            .iter()
            .map(|fd| fd.try_clone().unwrap())
            .collect::<Vec<_>>();
        let bytes = m.data().bytes().to_vec();
        let data = Data::new_fds(bytes.clone(), m.data().context(), fds);
        Message::from_raw_parts(data, 0).unwrap();

        let fds = vec![m.data().fds()[0].try_clone().unwrap()];
        let data = Data::new_fds(bytes, m.data().context(), fds);
        let error = Message::from_raw_parts(data, 0).unwrap_err();
        assert!(matches!(error, Error::MissingParameter(_)), "{error:?}");
    }
}
