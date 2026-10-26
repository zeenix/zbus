#![no_main]

use zbus::{
    Message,
    message::Header,
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
        (Ok(received), Ok((decoded, _))) => (received, decoded),
        (Err(_), Err(_)) => return,
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

/// Where the length of the header fields lies in a message, which is followed by the fields.
const FIELDS_LEN: std::ops::Range<usize> = 12..16;
