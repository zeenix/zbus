//! Integration with the async runtime that drives a connection.
//!
//! zbus does not ship a runtime of its own. A connection takes its timers and runs its internal
//! tasks on `async-io` (the default), on Tokio, or on any implementation of [`traits::Runtime`]
//! handed to `Builder::runtime`; the socket it is given supplies its own readiness. Both built-in
//! backends are implementations of that trait, picked by the `async-io` and `tokio` features. The
//! async locks a connection holds are not part of that trait: they come from `async-lock` or from
//! Tokio, whichever cargo feature is on. [`AsyncDrop`] is the async counterpart of [`Drop`] that
//! zbus's own types implement.

pub mod traits;

mod io_source;
pub use io_source::{Interest, IoSource};
#[cfg(feature = "async-io")]
mod async_io;
#[cfg(feature = "async-io")]
pub(crate) use async_io::AsyncIo;
mod async_drop;
pub use async_drop::AsyncDrop;
mod blocking_thread;
pub(crate) mod locks;
mod task;
pub(crate) use task::Task;
mod timeout;
#[cfg(feature = "tokio")]
mod tokio_rt;
#[cfg(feature = "tokio")]
use tokio_rt::Tokio;
#[cfg(feature = "async-io")]
mod unblock;

// Only the `unixexec` and `ibus` transports and, on macOS, the `launchd` one run commands.
#[cfg(all(unix, any(feature = "unixexec", feature = "ibus", target_os = "macos")))]
pub(crate) mod process;

use std::future::Future;

use crate::Result;

/// The runtime a connection runs on, chosen once when it is built.
#[derive(Clone, Debug)]
pub(crate) enum Runtime {
    #[cfg(feature = "async-io")]
    AsyncIo(AsyncIo),
    #[cfg(feature = "tokio")]
    Tokio(Tokio),
}

impl Runtime {
    /// The runtime for a connection built without an explicit one.
    ///
    /// Tokio when it is compiled in and a runtime is current on this thread, otherwise async-io
    /// when that is compiled in. This keeps the features additive: enabling `tokio` elsewhere in
    /// the dependency graph doesn't force every zbus user into a tokio runtime. A `tokio` build
    /// without `async-io` has no default left on a thread where no Tokio runtime is current, and
    /// reports [`Error::Unsupported`] instead.
    ///
    /// [`Error::Unsupported`]: crate::Error::Unsupported
    pub(crate) fn default_for_build() -> Result<Self> {
        #[cfg(feature = "tokio")]
        if let Some(runtime) = Tokio::current() {
            return Ok(Self::Tokio(runtime));
        }
        #[cfg(feature = "async-io")]
        {
            Ok(Self::AsyncIo(AsyncIo::new()))
        }
        #[cfg(not(feature = "async-io"))]
        {
            Err(crate::Error::Unsupported)
        }
    }

    /// Spawns a task onto the runtime, under the diagnostic name `name`.
    pub(crate) fn spawn<T>(
        &self,
        name: &str,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T>
    where
        T: Send + 'static,
    {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncIo(runtime) => Task(task::TaskInner::AsyncIo(traits::Runtime::spawn(
                runtime, name, future,
            ))),
            #[cfg(feature = "tokio")]
            Self::Tokio(runtime) => Task(task::TaskInner::Tokio(traits::Runtime::spawn(
                runtime, name, future,
            ))),
        }
    }
}

/// Blocking work for code that does not have a connection's runtime at hand.
///
/// Transports and sockets are not tied to one connection's runtime, so they pick the backend per
/// call the way `select_runtime!` does.
pub(crate) fn spawn_blocking<F, T>(
    f: F,
    #[allow(unused)] name: &str,
) -> std::pin::Pin<Box<dyn Future<Output = std::io::Result<T>> + Send + 'static>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    select_runtime! {
        tokio: {
            let task = task::tokio_spawn_blocking(f, name);

            Box::pin(async move { task.await.map_err(std::io::Error::other) })
        },
        async_io: Box::pin(async move { Ok(blocking::unblock(f).await) }),
    }
}

/// Evaluates the `tokio` or `async-io` expression for the active backend.
///
/// With a single backend it resolves to that backend's expression at compile time; with both it
/// picks at runtime via [`use_tokio`]. The inactive arm is `cfg`-stripped, so each arm only needs
/// to be valid in the configurations where its backend is compiled in.
///
/// The choice is re-evaluated on every call, so this is only safe where it doesn't need to match a
/// particular connection's backend. A connection latches its backend once at build time (see
/// [`Runtime::default_for_build`]); use that instead of this macro for anything tied to the
/// socket's reactor. The current call sites (timers, the blocking pool) are independent of the
/// socket, so a per-call decision is fine.
macro_rules! select_runtime {
    (tokio: $tokio:expr, async_io: $async_io:expr $(,)?) => {{
        #[cfg(all(feature = "tokio", feature = "async-io"))]
        {
            if $crate::runtime::use_tokio() {
                $tokio
            } else {
                $async_io
            }
        }
        #[cfg(all(feature = "tokio", not(feature = "async-io")))]
        {
            $tokio
        }
        #[cfg(all(feature = "async-io", not(feature = "tokio")))]
        {
            $async_io
        }
    }};
}
pub(crate) use select_runtime;

/// Whether zbus should use tokio (rather than `async-io`) for its I/O.
///
/// Only consulted when both backends are compiled in, since that's the only time there's a
/// choice: we then use tokio when a tokio runtime is active on the current thread. This keeps the
/// features additive: enabling `tokio` elsewhere in the dependency graph doesn't force every zbus
/// user into a tokio runtime.
#[cfg(all(feature = "async-io", feature = "tokio"))]
pub(crate) fn use_tokio() -> bool {
    tokio::runtime::Handle::try_current().is_ok()
}

#[cfg(all(test, feature = "tokio", feature = "async-io"))]
mod tests {
    #[test]
    fn use_tokio_reflects_active_runtime() {
        assert!(!super::use_tokio(), "no runtime is active here");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert!(
            runtime.block_on(async { super::use_tokio() }),
            "a tokio runtime is active",
        );
    }
}
