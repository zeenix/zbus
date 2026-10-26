//! Strings to check the validators of names and object paths on.

/// Call `check` with every string of up to 2 bytes, then with longer ones made of bytes that the
/// rules tell apart, including those either side of each range of bytes that they take, and with
/// long ones either side of the length that names are limited to.
///
/// The strings are made one after the other in a single buffer, as there are millions of them.
pub(crate) fn for_each<F>(mut check: F)
where
    F: FnMut(&[u8]),
{
    let mut string = Vec::with_capacity(260);
    check(&string);
    for first in 0..=u8::MAX {
        string.push(first);
        check(&string);
        for second in 0..=u8::MAX {
            string.push(second);
            check(&string);
            string.pop();
        }
        string.pop();
    }

    extend(&mut string, &mut check);

    for len in 250..=260 {
        for (start, rest) in [
            (&b"a."[..], b'b'),
            (b":a.", b'b'),
            (b"a", b'a'),
            (b"/a/", b'b'),
        ] {
            string.clear();
            string.extend_from_slice(start);
            string.resize(len, rest);
            check(&string);
        }
    }
}

/// Extend `string` with each byte that the rules tell apart, up to 6 bytes, and call `check` with
/// each string of 3 bytes or more that it makes.
fn extend<F>(string: &mut Vec<u8>, check: &mut F)
where
    F: FnMut(&[u8]),
{
    // The 5th and 6th bytes are of fewer different bytes, to keep the number of strings down.
    let bytes: &[u8] = if string.len() < 4 {
        b"aZ0_-.:/\0\x80@[`{,^"
    } else {
        b"a0-.:/"
    };
    for byte in bytes {
        string.push(*byte);
        if string.len() > 2 {
            check(string);
        }
        if string.len() < 6 {
            extend(string, check);
        }
        string.pop();
    }
}
