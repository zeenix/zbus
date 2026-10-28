#![no_main]

use serde::de::IgnoredAny;
use zbus::{
    Message, Structure,
    wire::{Endian, serialized::Data},
};

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
    let data = body.data();
    // A structure of the types of the signature has the same bytes as those types one after the
    // other, which the signature of more than one type is that of already.
    let structure = match signature {
        zbus::Signature::Structure(_) => Ok(signature.clone()),
        _ => zbus::Signature::try_from(format!("({signature})").as_str()),
    };
    let deserialized = match structure {
        Ok(structure) => data
            .deserialize_for_dynamic_signature::<_, Structure<'_>>(&structure)
            .map(|(_, len)| len),
        // A signature of a single type nested as deep as a signature can be has no room for one
        // more structure around it.
        Err(_) => values(data, signature),
    };
    match (body.validate(), deserialized) {
        (Ok(()), Ok(len)) if len == data.len() => (),
        (Ok(()), Ok(_)) => panic!("a valid body of `{signature}` has bytes left once deserialized"),
        // The structure counts toward the containers that each value is in, which a value may be
        // in the most of without it, so the values have to deserialize each on its own then.
        (Ok(()), Err(zbus::Error::MaxDepthExceeded(_))) => match values(data, signature) {
            Ok(len) if len == data.len() => (),
            Ok(_) => panic!("a valid body of `{signature}` has bytes left once deserialized"),
            Err(e) => panic!("a valid body of `{signature}` doesn't deserialize: {e}"),
        },
        (Ok(()), Err(e)) => panic!("a valid body of `{signature}` doesn't deserialize: {e}"),
        (Err(e), Ok(_)) if lax(&e) => (),
        (Err(e), Ok(len)) if len == data.len() => {
            panic!("a body of `{signature}` that deserializes is invalid: {e}")
        }
        (Err(_), _) => (),
    }
}

/// Deserialize the values of `signature` in `data` each on its own, as the body has them, rather
/// than as fields of a structure. The number of bytes that they take.
fn values(data: &Data<'_, '_>, signature: &zbus::Signature) -> zbus::Result<usize> {
    // The signature of more than one type is that of a structure of them, which the header of the
    // message has without its parentheses.
    let types: Vec<_> = match signature {
        zbus::Signature::Structure(fields) => fields.iter().collect(),
        signature => vec![signature],
    };
    types.into_iter().try_fold(0, |len, signature| {
        data.slice(len..)
            .deserialize_for_signature::<_, IgnoredAny>(signature)
            .map(|(_, value_len)| len + value_len)
    })
}

/// Whether `e` is of what the deserializer doesn't check: an object path in a `Variant`, the byte
/// that ends a string, and whether the signature of a variant is that of a single complete type,
/// or, for values that it deserializes each on its own, whether a signature is a valid one at all.
fn lax(e: &zbus::Error) -> bool {
    matches!(
        e,
        zbus::Error::InvalidObjectPath | zbus::Error::SignatureParse(_)
    ) || e.to_string().ends_with("expected the 0 that ends a string")
}
