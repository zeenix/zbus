use byteorder::{BigEndian, LittleEndian};

use crate::EndianSig;

/// Same as [`byteorder::ByteOrder`], adding a method to retrieve the D-Bus endian signature.
pub trait ByteOrder: byteorder::ByteOrder {
    /// The D-Bus endian signature for this [`byteorder::ByteOrder`] implementation.
    fn endian_signature() -> EndianSig;
}

impl ByteOrder for LittleEndian {
    fn endian_signature() -> EndianSig {
        EndianSig::Little
    }
}

impl ByteOrder for BigEndian {
    fn endian_signature() -> EndianSig {
        EndianSig::Big
    }
}
