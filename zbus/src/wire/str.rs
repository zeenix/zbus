/// This type is used for keeping strings in a [`Value`], among other things.
///
/// [`Value`]: crate::wire::Value::Str
pub use zcheapstr::CheapStr as Str;

use crate::wire::{Basic, Type};

impl Basic for Str<'_> {
    const SIGNATURE_CHAR: char = <&str>::SIGNATURE_CHAR;
    const SIGNATURE_STR: &'static str = <&str>::SIGNATURE_STR;
}

impl Type for Str<'_> {
    const SIGNATURE: &'static crate::wire::Signature = &crate::wire::Signature::Str;
}
