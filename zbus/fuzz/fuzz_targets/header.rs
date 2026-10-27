#![no_main]

use zbus::{
    Message,
    message::{Header, Type},
    wire::{
        BE, Endian, LE,
        serialized::{Context, Data},
    },
};

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let endian = match data.first() {
        Some(b'B') => BE,
        _ => LE,
    };
    check(data, endian);

    // The length that most inputs give their fields doesn't match the bytes after it, so each is
    // checked with the length of the bytes after it too, for the fuzzer to get to the fields
    // sooner.
    if let Some(fields_len) = data.len().checked_sub(FIELDS_LEN.end) {
        let mut data = data.to_vec();
        endian.write_u32(&mut data[FIELDS_LEN], fields_len.try_into().unwrap());
        check(&data, endian);
    }
});

/// Check that a message decoded from `data` has the header that decoding a `Header` from it through
/// serde gives.
fn check(data: &[u8], endian: Endian) {
    let ctxt = Context::new(endian, 0);
    // SAFETY: The data carries no file descriptors.
    let received = unsafe { Message::from_bytes(Data::new(data.to_vec(), ctxt)) };
    let encoded = Data::new(data, ctxt);
    let decoded = encoded.deserialize::<Header<'_>>();

    // Decoding through serde skips the byte that ends a string without checking that it's there,
    // so it takes a header whose last string ends right where the bytes do, without that byte,
    // while a message is never decoded from bytes that its header runs past.
    if data.len() >= FIELDS_LEN.end {
        let fields_len = endian.read_u32(&data[FIELDS_LEN]);
        if FIELDS_LEN.end + fields_len as usize > data.len() {
            assert!(received.is_err());

            return;
        }
    }

    // The body of a message isn't looked at, as it may be shorter than its header says.
    let (received, decoded) = match (received, decoded) {
        (Ok(received), Ok((decoded, len))) => {
            assert!(whole(&decoded, len, data, endian));

            (received, decoded)
        }
        (Err(_), Err(_)) => return,
        (Err(_), Ok((decoded, len))) if !whole(&decoded, len, data, endian) => return,
        (received, decoded) => panic!(
            "decoded as a message: {:?}, as a header: {:?}",
            received.map(|_| ()),
            decoded.map(|_| ()),
        ),
    };

    let primary = received.primary_header();
    let decoded_primary = decoded.primary();
    assert_eq!(primary.endian_sig(), decoded_primary.endian_sig());
    assert_eq!(primary.msg_type(), decoded_primary.msg_type());
    assert_eq!(primary.flags(), decoded_primary.flags());
    assert_eq!(
        primary.protocol_version(),
        decoded_primary.protocol_version()
    );
    assert_eq!(primary.body_len(), decoded_primary.body_len());
    assert_eq!(primary.serial_num(), decoded_primary.serial_num());

    let header = received.header();
    assert_eq!(header.path(), decoded.path());
    assert_eq!(header.interface(), decoded.interface());
    assert_eq!(header.member(), decoded.member());
    assert_eq!(header.error_name(), decoded.error_name());
    assert_eq!(header.reply_serial(), decoded.reply_serial());
    assert_eq!(header.destination(), decoded.destination());
    assert_eq!(header.sender(), decoded.sender());
    assert_eq!(header.signature(), decoded.signature());
    assert_eq!(header.unix_fds(), decoded.unix_fds());
}

/// Whether a message whose header, `len` bytes long, decodes to `header` from the start of `data`,
/// in byte order `endian`, is whole, as a received message has to be beyond what decoding a header
/// through serde checks of its fields: of the only version of the protocol, with the fields that
/// its type requires, none of which comes twice, with a 0 that ends each string and signature in
/// them, with padding of 0s after its header, and with the file descriptors that its header says it
/// brought, of which `data` has none.
fn whole(header: &Header<'_>, len: usize, data: &[u8], endian: Endian) -> bool {
    let required = match header.message_type() {
        Type::MethodCall => header.path().is_some() && header.member().is_some(),
        Type::MethodReturn => header.reply_serial().is_some(),
        Type::Error => header.error_name().is_some() && header.reply_serial().is_some(),
        Type::Signal => {
            header.path().is_some() && header.interface().is_some() && header.member().is_some()
        }
    };
    let padded = data
        .get(len..len.next_multiple_of(8))
        .is_some_and(|padding| padding.iter().all(|byte| *byte == 0));
    let fds = !cfg!(unix) || header.unix_fds().unwrap_or(0) == 0;

    header.primary().protocol_version() == 1
        && required
        && canonical_fields(data, endian)
        && padded
        && fds
}

/// Whether the fields of the header at the start of `data`, which decodes through serde, end each
/// of their strings and signatures with a 0, none of them coming twice.
fn canonical_fields(data: &[u8], endian: Endian) -> bool {
    let end = FIELDS_LEN.end + endian.read_u32(&data[FIELDS_LEN]) as usize;
    let byte = |i: usize| data.get(i).copied();
    let mut codes = std::collections::HashSet::new();
    let mut field = FIELDS_LEN.end;
    while field < end {
        // A field starts with its code, and the signature of its value: 1 type, and a 0. Its value
        // comes next, on a 4-byte boundary, as the field starts on an 8-byte one.
        let value = field + 4;
        let value_end = match byte(field + 2) {
            Some(b'u') => value + 4,
            Some(b'g') => match byte(value) {
                Some(len) => value + 1 + len as usize + 1,
                None => return false,
            },
            _ => match data.get(value..value + 4) {
                Some(len) => value + 4 + endian.read_u32(len) as usize + 1,
                None => return false,
            },
        };
        let ends_with_0 = byte(field + 3) == Some(0)
            && (byte(field + 2) == Some(b'u') || byte(value_end - 1) == Some(0));
        if !codes.insert(byte(field)) || !ends_with_0 {
            return false;
        }
        field = value_end.next_multiple_of(8);
    }

    true
}

/// Where the length of the header fields lies in a message, which is followed by the fields.
const FIELDS_LEN: std::ops::Range<usize> = 12..16;
