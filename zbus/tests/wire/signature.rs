use zbus::wire::{LE, Signature, Value, signature};

#[test]
fn signature() {
    let sig: Signature = signature!("yys");

    // Structure will always add () around the signature if it's a struct.
    basic_type_test!(LE, sig, 7, Signature, 1);

    // As Value
    let v: Value<'_> = sig.into();
    assert_eq!(v.value_signature(), "g");
    let encoded = value_test!(LE, v, 10);
    let v = encoded.deserialize::<Value<'_>>().unwrap().0;
    assert_eq!(v, Value::Signature(signature!("yys")));
}
