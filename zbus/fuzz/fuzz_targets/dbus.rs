#![no_main]
mod utils;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    utils::fuzz_for_context(
        data,
        zbus::wire::serialized::Context::new_dbus(zbus::wire::LE, 0),
    );
    utils::fuzz_for_context(
        data,
        zbus::wire::serialized::Context::new_dbus(zbus::wire::BE, 0),
    );
});
