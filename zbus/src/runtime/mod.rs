//! Integration with the async runtime that drives a connection.
//!
//! zbus does not ship a runtime of its own. A connection watches its socket, takes its timers and
//! runs its internal tasks on `async-io` (the default), on Tokio, or on any implementation of
//! [`traits::Runtime`] handed to [`Builder::runtime`]. Both built-in backends are implementations
//! of that trait, picked by the `async-io` and `tokio` features. The async locks a connection
//! holds are not part of that trait: they come from `async-lock` or from Tokio, whichever of the
//! `async-lock` and `tokio` cargo features is on.
//! [`AsyncDrop`] is the async counterpart of [`Drop`] that zbus's own types implement.
//!
//! [`Builder::runtime`]: crate::connection::Builder::runtime

pub mod traits;

pub(crate) mod io;
mod io_source;
pub use io_source::{Interest, IoSource};
#[cfg(feature = "async-io")]
mod async_io;
#[cfg(feature = "async-io")]
pub(crate) use async_io::AsyncIo;
mod async_drop;
pub use async_drop::AsyncDrop;
mod blocking_thread;
mod erased;
pub(crate) mod locks;
mod task;
pub(crate) use task::Task;
#[cfg(test)]
pub(crate) mod test_runtime;
mod timeout;
#[cfg(feature = "tokio")]
mod tokio_rt;
#[cfg(feature = "tokio")]
use tokio_rt::Tokio;
#[cfg(feature = "async-io")]
mod unblock;

// Only the `unixexec` and `ibus` transports and, on macOS, the `launchd` one run commands, and
// only a backend can run one: the transports are unsupported without one.
#[cfg(all(
    unix,
    any(feature = "async-io", feature = "tokio"),
    any(feature = "unixexec", feature = "ibus", target_os = "macos")
))]
pub(crate) mod process;

use std::{any::Any, future::Future, sync::Arc};

use erased::ErasedRuntime;

use crate::Result;

/// The runtime a connection runs on, chosen once when it is built.
#[derive(Clone, Debug)]
pub(crate) enum Runtime {
    #[cfg(feature = "async-io")]
    AsyncIo(AsyncIo),
    #[cfg(feature = "tokio")]
    Tokio(Tokio),
    /// A runtime handed to the builder, reached through the object-safe mirrors of the traits.
    External(Arc<dyn ErasedRuntime>),
}

impl Runtime {
    /// The runtime for a connection built without an explicit one.
    ///
    /// Tokio when it is compiled in and a runtime is current on this thread, otherwise async-io
    /// when that is compiled in. This keeps the features additive: enabling `tokio` elsewhere in
    /// the dependency graph doesn't force every zbus user into a tokio runtime. Two builds have
    /// no default left and report [`Error::Unsupported`] instead, so that a connection in them
    /// has to be given a runtime of its own: one with neither backend compiled in, and one with
    /// only `tokio` called from a thread where no Tokio runtime is current.
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

    /// The runtime for a connection built with an explicit one.
    ///
    /// Its every operation goes through the erased mirrors of the traits, which is what lets a
    /// connection hold any implementation without being generic over it.
    pub(crate) fn from_external<R>(runtime: R) -> Self
    where
        R: traits::Runtime,
    {
        Self::External(Arc::new(runtime))
    }

    /// Watches `source` for readiness on this runtime.
    ///
    /// Every I/O a connection performs on a socket of its own goes through the registration this
    /// returns, so a socket is only ever driven by the runtime the connection was built with.
    pub(crate) fn register_io_source(&self, source: IoSource) -> std::io::Result<io::Registration> {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncIo(runtime) => {
                traits::Runtime::register_io_source(runtime, source).map(io::Registration::AsyncIo)
            }
            #[cfg(feature = "tokio")]
            Self::Tokio(runtime) => {
                traits::Runtime::register_io_source(runtime, source).map(io::Registration::Tokio)
            }
            Self::External(runtime) => ErasedRuntime::register_io_source(&**runtime, source)
                .map(io::Registration::External),
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
            Self::AsyncIo(runtime) => Task::AsyncIo(traits::Runtime::spawn(runtime, name, future)),
            #[cfg(feature = "tokio")]
            Self::Tokio(runtime) => Task::Tokio(traits::Runtime::spawn(runtime, name, future)),
            Self::External(runtime) => {
                Task::External(erased::ExternalTask::spawn(&**runtime, name, future))
            }
        }
    }

    /// Runs `work` off the event loop, on whatever this runtime keeps for blocking work.
    pub(crate) async fn spawn_blocking<T>(&self, work: impl FnOnce() -> T + Send + 'static) -> T
    where
        T: Send + 'static,
    {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncIo(runtime) => traits::Runtime::spawn_blocking(runtime, work).await,
            #[cfg(feature = "tokio")]
            Self::Tokio(runtime) => traits::Runtime::spawn_blocking(runtime, work).await,
            Self::External(runtime) => {
                // The erased runtime only takes work that produces an opaque value, so the
                // result comes back to be downcast to what `work` returned.
                let work = Box::new(move || Box::new(work()) as Box<dyn Any + Send>);

                *runtime
                    .spawn_blocking(work)
                    .await
                    .downcast()
                    .expect("blocking work hands back the value it produced")
            }
        }
    }
}

/// Blocking work for code that does not have a connection's runtime at hand.
///
/// Transports and sockets are not tied to one connection's runtime, so they pick the backend per
/// call the way `select_runtime!` does. A build with neither backend has no pool to offer: this
/// function and every path that needs it are compiled out of it.
#[cfg(any(feature = "async-io", feature = "tokio"))]
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
#[cfg(any(feature = "async-io", feature = "tokio"))]
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
#[cfg(any(feature = "async-io", feature = "tokio"))]
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

#[cfg(test)]
mod tests;
