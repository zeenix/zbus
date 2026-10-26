//! Validator for D-Bus object paths.

use winnow::{Parser, combinator::separated, stream::AsChar, token::take_while};

/// Validate a D-Bus object path.
pub fn validate(bytes: &[u8]) -> Result<(), &'static str> {
    // Rules
    //
    // * At least 1 character.
    // * First character must be `/`
    // * No trailing `/`
    // * No `//`
    // * Only ASCII alphanumeric, `_` or '/'
    let mut full_path = (b'/', separated(0.., element, b'/')).map(|_: (u8, ())| ());

    full_path.parse(bytes).map_err(|_| OBJECT_PATH_ERROR)
}

/// An element of an object path.
///
/// It's parsed by a function of its own, which builds its parser where it runs, from a set of bytes
/// that is a closure of its own. That makes its `take_while` a parser of a type of its own, which
/// the compiler inlines with its range of occurrences known, so that a whole path is checked in a
/// single loop that keeps its state in registers.
#[inline(always)]
fn element(input: &mut &[u8]) -> Result<(), ()> {
    let element_char = |byte: u8| byte.is_alphanum() || byte == b'_';

    take_while(1.., element_char).void().parse_next(input)
}

const OBJECT_PATH_ERROR: &str = "Invalid object path";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_strings;

    #[test]
    fn same_as_reference() {
        test_strings::for_each(|path| {
            assert_eq!(validate(path), reference(path), "{path:?}");
        });
    }

    #[test]
    fn valid_paths() {
        let valid: [&[u8]; 3] = [
            b"/",
            b"/Path/t0/0bject",
            b"/a/very/looooooooooooooooooooooooo0000o0ng/path",
        ];
        for path in valid {
            validate(path).unwrap();
        }
    }

    #[test]
    fn invalid_paths() {
        let invalid: [&[u8]; 5] = [
            b"",
            b"/double//slashes/",
            b".",
            b"/end/with/slash/",
            b"/ha.d",
        ];
        for path in invalid {
            assert_eq!(validate(path), Err(OBJECT_PATH_ERROR));
        }
    }

    // This message is part of the observable behaviour of the object-path type's error variant,
    // so pin it here rather than only comparing the constant against itself.
    #[test]
    fn error_message() {
        assert_eq!(OBJECT_PATH_ERROR, "Invalid object path");
    }

    /// The rules of object paths, written with winnow's combinators as they are in the
    /// specification, for the validator, which is written for speed, to be checked against.
    fn reference(bytes: &[u8]) -> Result<(), &'static str> {
        use winnow::{Parser, combinator::separated, stream::AsChar, token::take_while};

        let allowed_chars = (AsChar::is_alphanum, b'_');
        let name = take_while::<_, _, ()>(1.., allowed_chars);
        let mut full_path = (b'/', separated(0.., name, b'/')).map(|_: (u8, ())| ());

        full_path.parse(bytes).map_err(|_| OBJECT_PATH_ERROR)
    }
}
