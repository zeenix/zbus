#![no_main]

use zbus::{Message, Structure, wire::Endian};

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // The byte order, then the length of the signature, the signature, and the body.
    let [order, len, rest @ ..] = data else {
        return;
    };
    let Some((signature, body)) = rest.split_at_checked((*len).into()) else {
        return;
    };
    let Ok(signature) = zbus::Signature::try_from(signature) else {
        return;
    };
    let endian = match order {
        b'B' => Endian::Big,
        _ => Endian::Little,
    };
    // SAFETY: The body carries no file descriptors, and is checked before it's deserialized.
    let Ok(msg) = (unsafe {
        Message::signal("/org/zbus/Test", "org.zbus.Test", "Test")
            .endian(endian)
            .build_raw_body(body, &signature, vec![])
    }) else {
        return;
    };
    check(&msg, &signature);
});

/// Check that the body of `msg`, of `signature`, is valid if and only if it deserializes, all of
/// it.
fn check(msg: &Message, signature: &zbus::Signature) {
    let body = msg.body();
    // A structure of the types of the signature has the same bytes as those types one after the
    // other.
    let structure = zbus::Signature::try_from(format!("({signature})").as_str());
    let Ok(structure) = structure else {
        // A signature of a single type nested as deep as a signature can be has no room for one
        // more structure around it.
        return;
    };
    let data = body.data();
    let deserialized = data.deserialize_for_dynamic_signature::<_, Structure<'_>>(&structure);
    match (body.validate(), deserialized) {
        (Ok(()), Ok((_, len))) if len == data.len() => (),
        (Ok(()), Ok(_)) => panic!("a valid body of `{signature}` has bytes left once deserialized"),
        // The deserializer has the limits of nesting that a signature has, of 32 arrays and 32
        // structures, hold across variants too, and the structure counts toward them, which the
        // types may be at without it.
        (Ok(()), Err(zbus::Error::MaxDepthExceeded(_))) => (),
        (Ok(()), Err(e)) => panic!("a valid body of `{signature}` doesn't deserialize: {e}"),
        (Err(e), Ok(_)) if lax(&e) => (),
        (Err(e), Ok((_, len))) if len == data.len() => {
            panic!("a body of `{signature}` that deserializes is invalid: {e}")
        }
        (Err(_), _) => (),
    }
}

/// Whether `e` is of what the deserializer doesn't check: an object path in a `Variant`, the byte
/// that ends a string, whether the signature of a variant is that of a single complete type, and
/// how deep values are nested, which it counts otherwise: not the dict entries that a value is in,
/// nor the values that an empty array or the type of a variant could hold.
fn lax(e: &zbus::Error) -> bool {
    matches!(
        e,
        zbus::Error::InvalidObjectPath
            | zbus::Error::SignatureParse(_)
            | zbus::Error::MaxDepthExceeded(_)
    ) || e.to_string().ends_with("expected the 0 that ends a string")
}
