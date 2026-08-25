# Fuzz targets for zbus

[Fuzzing](https://en.wikipedia.org/wiki/Fuzzing) is a way to test software by feeding it random
inputs to make sure it doesn't crash. This directory contains targets to test zbus using
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz).

Run `cargo install cargo-fuzz` to install the fuzzer, then run `cargo +nightly fuzz run dbus` from
the `zbus` directory to fuzz the D-Bus deserializer.
