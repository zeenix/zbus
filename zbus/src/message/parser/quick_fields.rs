//! What the parser finds of the header fields: where each string field lies in the message, and
//! the other fields.

use std::{borrow::Cow, num::NonZeroU32, str};

use winnow::stream::Accumulate;

use crate::{
    ObjectPath, Signature,
    message::Fields,
    names::{BusName, ErrorName, InterfaceName, MemberName, UniqueName, WellKnownName},
};

/// Where the string fields of a header lie in the bytes of its message, and the other fields.
///
/// Only the parser creates it, from fields that it checked, so that reading them back doesn't have
/// to check them again.
#[derive(Debug, Default)]
pub(crate) struct QuickFields {
    path: Option<FieldPos>,
    interface: Option<FieldPos>,
    member: Option<FieldPos>,
    error_name: Option<FieldPos>,
    reply_serial: Option<NonZeroU32>,
    destination: Option<FieldPos>,
    /// Whether the destination is a unique name, rather than a well-known one.
    unique_destination: bool,
    sender: Option<FieldPos>,
    signature: Signature,
    unix_fds: Option<u32>,
}

impl QuickFields {
    /// The number of file descriptors that accompany the message.
    #[cfg(unix)]
    pub(crate) fn unix_fds(&self) -> Option<u32> {
        self.unix_fds
    }

    /// The fields, borrowed from the bytes of the message.
    ///
    /// # Safety
    ///
    /// `bytes` must be the bytes that `self` was parsed from.
    pub(in crate::message) unsafe fn fields<'m>(&'m self, bytes: &'m [u8]) -> Fields<'m> {
        let read = |pos: Option<FieldPos>| {
            pos.map(|pos| {
                // SAFETY: The caller ensures that `bytes` are those that `pos` was parsed from, as
                // the position of a name or a path, which the parser checked, and those are ASCII.
                unsafe { str::from_utf8_unchecked(&bytes[pos.start as usize..pos.end as usize]) }
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

    /// The signature of the body.
    pub(in crate::message) fn signature(&self) -> &Signature {
        &self.signature
    }
}

/// The fields are gathered as they're parsed. A field that comes more than once is checked each
/// time, and the last one is kept.
impl Accumulate<Field> for QuickFields {
    fn initial(_capacity: Option<usize>) -> Self {
        Self::default()
    }

    fn accumulate(&mut self, field: Field) {
        match field {
            Field::Path(pos) => self.path = Some(pos),
            Field::Interface(pos) => self.interface = Some(pos),
            Field::Member(pos) => self.member = Some(pos),
            Field::ErrorName(pos) => self.error_name = Some(pos),
            Field::ReplySerial(serial) => self.reply_serial = Some(serial),
            Field::Destination { pos, unique } => {
                self.destination = Some(pos);
                self.unique_destination = unique;
            }
            Field::Sender(pos) => self.sender = Some(pos),
            Field::Signature(signature) => self.signature = signature,
            Field::UnixFds(unix_fds) => self.unix_fds = Some(unix_fds),
        }
    }
}

/// A header field, as the parser finds it.
pub(super) enum Field {
    Path(FieldPos),
    Interface(FieldPos),
    Member(FieldPos),
    ErrorName(FieldPos),
    ReplySerial(NonZeroU32),
    Destination {
        pos: FieldPos,
        /// Whether the destination is a unique name, rather than a well-known one.
        unique: bool,
    },
    Sender(FieldPos),
    Signature(Signature),
    UnixFds(u32),
}

/// The byte range of a string field in the bytes of its message.
#[derive(Debug, Clone, Copy)]
pub(super) struct FieldPos {
    start: u32,
    end: u32,
}

impl FieldPos {
    /// The field at `start..end` in the bytes of the message, which the length of the fields
    /// ensured fits in a `u32`.
    pub(super) fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }
}
