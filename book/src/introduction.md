<img src="zbus-pixels.gif" alt="zbus illustration" style="width: 100%;">

# Introduction

**[zbus]** is a **[Rust]** crate for **[D-Bus]**. If you are not familiar with D-Bus, you should
read [what is D-Bus?] first[^outdated]. In short, zbus allows you to communicate from one program
to another, using the D-Bus protocol. In other words, it's an *inter-process* communication (IPC)
solution. It is a very popular IPC solution on Linux and many Linux services (e.g systemd,
NetworkManager) and desktop environments (e.g GNOME and KDE), rely on D-Bus for their IPC needs.
There are many tools and implementations available, making it easy to interact with D-Bus programs
from different languages and environments.

zbus is a 100% Rust-native implementation of the D-Bus protocol. It provides both an API to send
and receive messages over a connection, as well as API to interact with peers through high-level
concepts like method calls, signals and properties[^high-level-api]. Thanks to the power of Rust
macros, zbus is able to make interacting with D-Bus very easy.

The crate has two halves.

## The wire format

D-Bus defines a marshalling format for its messages. The `zbus::wire` module provides a
[serde]-based [API] to serialize and deserialize Rust data types to and from that format. It does
not need the rest of zbus, so if the format is all you are after — no connection, no proxies —
depend on zbus with its default features disabled:

```toml
[dependencies]
zbus = { version = "6", default-features = false }
```

Outside of a D-Bus context, a modified form of this format, [GVariant], is commonly used for
efficient storage of arbitrary data; the separate [zgvariant] crate implements it.

## The D-Bus API

Everything else — connections, messages, proxies, the object server — is the API you will use to
interact with D-Bus from Rust. It takes care of the establishment of a connection, the creation,
sending and receiving of different kind of D-Bus messages (method calls, signals etc) for you.

[zbus]: https://github.com/z-galaxy/zbus
[Rust]: https://www.rust-lang.org/
[D-Bus]: https://dbus.freedesktop.org/
[what is D-Bus?]: https://www.freedesktop.org/wiki/Software/dbus/#index1h1
[serde]: https://serde.rs/
[API]: https://docs.rs/zbus/latest/zbus/wire/index.html
[GVariant]: https://developer.gnome.org/documentation/specifications/gvariant-specification-1.0.html
[zgvariant]: https://crates.io/crates/zgvariant

[^outdated]: D-Bus is ~15y old, unfortunately many documents out there are
    sometime aging or misleading.

[^high-level-api]: These concepts are explained in the
[following chapter](concepts.html#interfaces).

<p align="center">
  <img src="https://www.freedesktop.org/png/freedesktop-logo.png" height="32"/>
</p>
