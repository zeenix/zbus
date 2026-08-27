use crate::wire::{Basic, Type};

pub use zvariant_utils::signature::*;

impl Type for Signature {
    const SIGNATURE: &'static Signature = &Signature::Signature;
}

impl Basic for Signature {
    const SIGNATURE_CHAR: char = 'g';
    const SIGNATURE_STR: &'static str = "g";
}

impl From<Signature> for crate::wire::Value<'static> {
    fn from(value: Signature) -> Self {
        crate::wire::Value::Signature(value)
    }
}
