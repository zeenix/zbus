#![deny(missing_docs)]
//! Regression test for issue #1542: everything `#[interface]` generates has to be documented,
//! otherwise downstream crates that opt into `missing_docs` get warnings they cannot silence.

use zbus::object_server::SignalEmitter;
use zbus_macros::interface;

/// Interface with a documented and an undocumented signal.
pub struct Test;

#[interface(name = "org.freedesktop.zbus_macros.MissingDocs")]
impl Test {
    /// A method.
    async fn method(&self) {}

    /// A documented signal.
    #[zbus(signal)]
    async fn documented(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn undocumented(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}
