use zbus::wire::{LE, Value, serialized::Context, to_bytes};

#[test]
fn str_value() {
    let string = String::from("hello world");
    basic_type_test!(LE, DBus, string, 16, String, 4);
    basic_type_test!(LE, DBus, string, 16, &str, 4);

    let string = "hello world";
    basic_type_test!(LE, DBus, string, 16, &str, 4);
    basic_type_test!(LE, DBus, string, 16, String, 4);

    // As Value
    let v: Value<'_> = string.into();
    assert_eq!(v.value_signature(), "s");
    assert_eq!(v, Value::new("hello world"));
    value_test!(LE, DBus, v, 20);

    let v: String = v.try_into().unwrap();
    assert_eq!(v, "hello world");

    // Characters are treated as strings
    basic_type_test!(LE, DBus, 'c', 6, char, 4);

    // As Value
    let v: Value<'_> = "c".into();
    assert_eq!(v.value_signature(), "s");
    let ctxt = Context::new_dbus(LE, 0);
    let encoded = to_bytes(ctxt, &v).unwrap();
    assert_eq!(encoded.len(), 10);
    let (v, _) = encoded.deserialize::<Value<'_>>().unwrap();
    assert_eq!(v, Value::new("c"));
}
