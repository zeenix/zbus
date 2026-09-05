#![deny(rust_2018_idioms)]
#![cfg_attr(test, recursion_limit = "256")]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/z-galaxy/zbus/9f7a90d2b594ddc48b7a5f39fda5e00cd56a7dfb/logo.png"
)]
// The README's examples need a D-Bus connection and a proxy, so it can only be the crate
// documentation when the client-side D-Bus API is compiled in.
#![cfg_attr(feature = "proxy", doc = include_str!("../README.md"))]
#![cfg_attr(
    all(feature = "comms", not(feature = "proxy")),
    doc = "# zbus",
    doc = "",
    doc = "This build has the client-side D-Bus API (the `proxy` feature) disabled. See the",
    doc = "[README](https://github.com/z-galaxy/zbus#readme) for an overview of the crate."
)]
#![cfg_attr(
    not(feature = "comms"),
    doc = "# zbus\n\nThis build has the D-Bus API (the `comms` feature) disabled: only the \
           D-Bus [wire format](crate::wire) and the [bus name types](crate::names) are \
           compiled in.\n"
)]
#![doc(test(attr(
    warn(unused),
    deny(warnings),
    allow(dead_code),
    // W/o this, we seem to get some bogus warning about `extern crate zbus`.
    allow(unused_extern_crates),
)))]

// The book and README examples all need a connection, so they are only compiled with the D-Bus
// API enabled.
#[cfg(all(doctest, feature = "comms"))]
mod doctests {
    // Repo README.
    #[cfg(feature = "proxy")]
    doc_comment::doctest!("../../README.md");
    // Book markdown checks
    #[cfg(feature = "proxy")]
    doc_comment::doctest!("../../book/src/client.md");
    doc_comment::doctest!("../../book/src/concepts.md");
    // The connection chapter contains a p2p example.
    #[cfg(feature = "p2p")]
    doc_comment::doctest!("../../book/src/connection.md");
    doc_comment::doctest!("../../book/src/contributors.md");
    doc_comment::doctest!("../../book/src/introduction.md");
    #[cfg(feature = "proxy")]
    doc_comment::doctest!("../../book/src/service.md");
    #[cfg(all(feature = "blocking-api", feature = "proxy"))]
    doc_comment::doctest!("../../book/src/blocking.md");
    #[cfg(feature = "proxy")]
    doc_comment::doctest!("../../book/src/upgrading-to-6.md");
    #[cfg(feature = "proxy")]
    doc_comment::doctest!("../../book/src/faq.md");
}

#[cfg(all(feature = "comms", not(feature = "async-io"), not(feature = "tokio")))]
mod error_message {
    #[cfg(windows)]
    compile_error!(
        "Either \"async-io\" (default) or \"tokio\" must be enabled. On Windows \"async-io\" is (currently) required for UNIX socket support"
    );

    #[cfg(not(windows))]
    compile_error!("Either \"async-io\" (default) or \"tokio\" must be enabled.");
}

#[cfg(all(
    any(feature = "vsock", feature = "tokio-vsock"),
    not(target_os = "linux")
))]
compile_error!("The \"vsock\" and \"tokio-vsock\" features are only supported on Linux.");

mod error;
pub use error::*;

pub mod wire;

pub mod names;

#[deprecated(
    since = "6.0.0",
    note = "zvariant was merged into zbus; use `zbus::wire`"
)]
pub mod zvariant;

#[cfg(all(feature = "comms", windows))]
mod win32;

#[cfg(feature = "comms")]
mod dbus_error;
#[cfg(feature = "comms")]
pub use dbus_error::*;

#[cfg(feature = "comms")]
pub mod address;
#[cfg(feature = "comms")]
pub use address::Address;

#[cfg(feature = "comms")]
mod guid;
#[cfg(feature = "comms")]
pub use guid::*;

#[cfg(feature = "comms")]
pub mod message;
#[cfg(feature = "comms")]
pub use message::Message;

#[cfg(feature = "comms")]
pub mod connection;
/// Alias for `connection` module, for convenience.
#[cfg(feature = "comms")]
pub use connection as conn;
#[cfg(feature = "comms")]
pub use connection::Connection;
#[cfg(feature = "comms")]
mod message_stream;
#[cfg(feature = "comms")]
pub use message_stream::*;
#[cfg(feature = "comms")]
mod abstractions;
#[cfg(feature = "comms")]
pub use abstractions::*;

#[cfg(feature = "comms")]
pub mod match_rule;
#[cfg(feature = "comms")]
pub use match_rule::{MatchRule, OwnedMatchRule};

#[cfg(feature = "proxy")]
pub mod proxy;
#[cfg(feature = "proxy")]
pub use proxy::Proxy;

#[cfg(feature = "comms")]
pub mod object_server;
#[cfg(feature = "comms")]
pub use object_server::ObjectServer;

#[cfg(feature = "comms")]
mod utils;
#[cfg(feature = "comms")]
pub use utils::*;

#[cfg(feature = "comms")]
#[macro_use]
pub mod fdo;

#[cfg(feature = "blocking-api")]
pub mod blocking;

#[cfg(feature = "proxy")]
pub use zbus_macros::proxy;
#[cfg(feature = "comms")]
pub use zbus_macros::{DBusError, interface};

// The macros emit feature-dependent code through these macros, so that the decision follows the
// features of the `zbus` the generated code is compiled against. The features of `zbus_macros`
// can differ from them: Cargo unifies the features of host dependencies, such as proc-macros and
// build scripts, separately from the target ones.
#[cfg(feature = "blocking-api")]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_blocking_api_feature {
    ($($item:tt)*) => { $($item)* };
}
#[cfg(not(feature = "blocking-api"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_blocking_api_feature {
    ($($item:tt)*) => {};
}
#[cfg(feature = "proxy")]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_proxy_feature {
    ($($item:tt)*) => { $($item)* };
}
#[cfg(not(feature = "proxy"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_proxy_feature {
    ($($item:tt)*) => {};
}

// Required for the macros to function within this crate.
extern crate self as zbus;

// Macro support module, not part of the public API.
#[cfg(feature = "comms")]
#[doc(hidden)]
pub mod export {
    pub use async_trait;
    pub use futures_core;
    pub use ordered_stream;
    pub use serde;
}
