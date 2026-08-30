# Establishing a connection

<!-- toc -->

The first thing you will have to do is to connect to a D-Bus bus or to a D-Bus peer. This is the
entry point of the zbus API.

## Connection to the bus

To connect to the session bus (the *per-user* bus), simply call `Connection::session()`. It
returns an instance of the connection (if all went well). Similarly, to connect to the system bus
(to communicate with services such as [NetworkManager], [BlueZ] or [PID1]), use
`Connection::system()`.

Moreover, it can be converted to a [`MessageStream`] that implements [`futures::stream::Stream`],
which can be used to conveniently receive messages, for the times when low-level API is
more appropriate for your use case.

**Note:** it is common for a D-Bus library to provide a "shared" connection to a bus for a process:
all `session()` share the same underlying connection for example. At the time of this writing,
zbus doesn't do that.

**Note:** on macOS, there is no standard implicit way to connect to a session bus. zbus provides
opt-in compatibility to the Launchd session bus discovery mechanism via the `launchctl getenv` feature.
The official dbus installation method via `Homebrew` provides a session bus installation,
utilizing macOS `LaunchAgents` feature. By default, zbus consumes an address for a bus connection that
is provided via `launchctl getenv DBUS_LAUNCHD_SESSION_BUS_SOCKET` command output.

## Using a custom bus address

You may also specify a custom bus with [`connection::Builder::address`] which takes a D-Bus address
[as specified in the specification][dspec].

## Peer to peer connection

Peer-to-peer connections are bus-less[^bus-less], and the initial handshake protocol is a bit
different. There is the notion of client & server endpoints, but that distinction doesn't matter
once the connection is established (both ends are equal, and can send any messages).

For example to create a bus-less peer-to-peer connection on Unix, you can do:

```rust,noplayground
# #[tokio::main]
# async fn main() -> zbus::Result<()> {
# #[cfg(all(unix, feature = "async-io"))]
# {
use std::os::unix::net::UnixStream;
use zbus::{connection::Builder, Guid};

let guid = Guid::generate();
let (p0, p1) = UnixStream::pair().unwrap();
# #[allow(unused)]
let (client_conn, server_conn) = futures_util::try_join!(
    // Client
    Builder::async_io_unix_stream(p0).p2p().build(),
    // Server
    Builder::async_io_unix_stream(p1).server(guid)?.p2p().build(),
)?;
# }
#
# Ok(())
# }
```

`async_io_unix_stream` takes a [`std::os::unix::net::UnixStream`]. With `tokio` enabled you can
instead pass a [`tokio::net::UnixStream`] to `tokio_unix_stream`.

[`std::os::unix::net::UnixStream`]: https://doc.rust-lang.org/std/os/unix/net/struct.UnixStream.html
[`tokio::net::UnixStream`]: https://docs.rs/tokio/latest/tokio/net/struct.UnixStream.html

**Note:** the `p2p` and `server` methods of `connection::Builder` are only available when `p2p`
cargo feature of `zbus` is enabled.

[NetworkManager]: https://developer.gnome.org/NetworkManager/stable/spec.html
[BlueZ]: https://git.kernel.org/pub/scm/bluetooth/bluez.git/tree/doc
[PID1]: https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.systemd1.html
[`futures::stream::Stream`]: https://docs.rs/futures/4/futures/stream/trait.Stream.html
[`MessageStream`]: https://docs.rs/zbus/latest/zbus/struct.MessageStream.html
[`connection::Builder::address`]: https://docs.rs/zbus/latest/zbus/connection/struct.Builder.html#method.address
[dspec]: https://dbus.freedesktop.org/doc/dbus-specification.html#addresses

[^bus-less]: Unless you implemented them, none of the bus methods will exist.
