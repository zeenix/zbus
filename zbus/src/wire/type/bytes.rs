use crate::wire::Signature;

impl crate::wire::Type for serde_bytes::Bytes {
    const SIGNATURE: &'static Signature = &Signature::static_array(&Signature::U8);
}

impl crate::wire::Type for serde_bytes::ByteBuf {
    const SIGNATURE: &'static Signature = &Signature::static_array(&Signature::U8);
}
