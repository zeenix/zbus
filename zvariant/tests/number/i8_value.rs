use zvariant::LE;

#[test]
fn i8_value() {
    basic_type_test!(LE, DBus, 77_i8, 2, i8, 2);
}
