#[cfg(feature = "async-io")]
pub(crate) use async_lock::Mutex;
#[cfg(all(feature = "async-io", feature = "service"))]
pub(crate) use async_lock::{RwLock, RwLockReadGuard, RwLockWriteGuard};
#[cfg(all(feature = "tokio", not(feature = "async-io")))]
pub(crate) use tokio::sync::Mutex;
#[cfg(all(feature = "tokio", not(feature = "async-io"), feature = "service"))]
pub(crate) use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
