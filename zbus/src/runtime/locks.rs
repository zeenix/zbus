//! The async locks a connection holds, picked by cargo feature.
//!
//! These belong to no particular runtime: any of them can be taken from a future polled anywhere,
//! so which crate they come from is a build-time choice rather than the connection's. `async-lock`
//! wins where it is enabled, and Tokio's own locks stand in where it is not, so that a Tokio-only
//! build pulls in no extra lock crate. A `comms` build with neither feature has no locks to offer
//! and does not compile; see the error `lib.rs` raises for it. Only the object server takes
//! readers-writer locks, so those come along with the `service` feature.

#[cfg(feature = "async-lock")]
pub(crate) use async_lock::Mutex;
#[cfg(all(feature = "async-lock", feature = "service"))]
pub(crate) use async_lock::{RwLock, RwLockReadGuard, RwLockWriteGuard};
#[cfg(all(feature = "tokio", not(feature = "async-lock")))]
pub(crate) use tokio::sync::Mutex;
#[cfg(all(feature = "tokio", not(feature = "async-lock"), feature = "service"))]
pub(crate) use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
