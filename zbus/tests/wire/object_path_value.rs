use zbus::wire::{LE, ObjectPath, Value};

#[test]
fn object_path_value() {
    let o = ObjectPath::try_from("/hello/world").unwrap();
    basic_type_test!(LE, o, 17, ObjectPath<'_>, 4);

    // As Value
    let v: Value<'_> = o.into();
    assert_eq!(v.value_signature(), "o");
    let encoded = value_test!(LE, v, 21);
    let v = encoded.deserialize::<Value<'_>>().unwrap().0;
    assert_eq!(
        v,
        Value::ObjectPath(ObjectPath::try_from("/hello/world").unwrap())
    );
}
