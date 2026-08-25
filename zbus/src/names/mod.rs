//! # zbus_names
//!
//! This crate provides collection of types for various [D-Bus bus names][dbn].
//!
//! This is used by [`zbus`] (and in future by [`zbus_macros`] as well) crate. Other D-Bus crates
//! are also encouraged to use this API in the spirit of cooperation. :)
//!
//! For convenience, `zbus` re-exports this crate as `names`, so you do not need to depend directly
//! on this crate if you already depend on `zbus`.
//!
//! **Status:** Stable.
//!
//! [dbn]: https://dbus.freedesktop.org/doc/dbus-specification.html#message-protocol-names
//! [`zbus`]: https://crates.io/crates/zbus
//! [`zbus_macros`]: https://crates.io/crates/zbus_macros

mod bus_name;
pub use bus_name::*;

mod unique_name;
pub use unique_name::*;

mod well_known_name;
pub use well_known_name::*;

mod interface_name;
pub use interface_name::*;

mod member_name;
pub use member_name::*;

mod property_name;
pub use property_name::*;

mod error;
pub use error::*;

mod error_name;
pub use error_name::*;

mod utils;
