//! Integration with the async runtime that drives a connection.
//!
//! By default, a connection runs on [zruntime] (the `default-rt` feature, on by default):
//! one runtime per thread that runs [`block_on`](crate::block_on), driven by that thread, and by
//! a helper thread only for work left with no thread inside `block_on`. A connection built
//! inside a Tokio runtime runs on it instead (the `tokio` feature). Any other runtime reaches a
//! connection through an implementation of [`traits::Runtime`], handed to [`Builder::runtime`].
//! The async locks a connection holds are not part of that trait: they come from zruntime, except
//! on a Tokio build, which uses Tokio's locks instead.
//! [`AsyncDrop`] is the async counterpart of [`Drop`] that zbus's own types implement.
//!
//! [`Builder::runtime`]: crate::connection::Builder::runtime
//! [zruntime]: https://docs.rs/zruntime

pub mod traits;

pub(crate) mod io;
mod io_source;
pub use io_source::{Interest, IoSource};
mod async_drop;
pub use async_drop::AsyncDrop;
#[cfg(feature = "default-rt")]
pub(crate) mod zruntime;
#[cfg(feature = "default-rt")]
pub(crate) use self::zruntime::ZRuntime;
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

// Only the `unixexec` and `ibus` transports and, on macOS, the `launchd` one run a program.
#[cfg(all(unix, any(feature = "unixexec", feature = "ibus", target_os = "macos")))]
pub(crate) mod process;

use std::{any::Any, future::Future, net::SocketAddr, path::Path, sync::Arc};

use erased::{BoxFuture, ErasedRuntime};

use crate::Result;

/// The runtime a connection runs on, chosen once when it is built.
///
/// Whichever runtime that is, a built-in backend or one handed to [`Builder::runtime`], it sits
/// behind the same [`ErasedRuntime`], so that every operation takes one path and a connection is
/// not generic over its runtime. The methods here put back the types that the erased layer took
/// out: see the `erased` module for how, and for what it costs.
///
/// [`Builder::runtime`]: crate::connection::Builder::runtime
#[derive(Clone, Debug)]
pub(crate) struct Runtime(Arc<dyn ErasedRuntime>);

impl Runtime {
    /// The runtime for a connection built without an explicit one.
    ///
    /// Tokio when the `tokio` feature is on and a runtime is current on this thread, otherwise
    /// [zruntime](https://docs.rs/zruntime), when the `default-rt` feature is on. This keeps the
    /// features additive: enabling `tokio` elsewhere in the dependency graph doesn't force every
    /// zbus user into a tokio runtime. Two builds have no default left and report
    /// [`Error::Unsupported`] instead, so that a connection in them has to be given an external
    /// runtime: one with neither feature on, and one with only `tokio` called from a thread where
    /// no Tokio runtime is current.
    ///
    /// [`Error::Unsupported`]: crate::Error::Unsupported
    pub(crate) fn default_for_build() -> Result<Self> {
        #[cfg(feature = "tokio")]
        if let Some(runtime) = Tokio::current() {
            return Ok(Self::new(runtime));
        }
        #[cfg(feature = "default-rt")]
        {
            Ok(Self::new(ZRuntime::new()?))
        }
        #[cfg(not(feature = "default-rt"))]
        {
            Err(crate::Error::Unsupported)
        }
    }

    /// The runtime for a connection that runs on `runtime`, a built-in backend or any other.
    ///
    /// The runtime is boxed here, once, behind the object-safe mirrors of the traits, which is
    /// what lets a connection hold any implementation without being generic over it.
    pub(crate) fn new<R>(runtime: R) -> Self
    where
        R: traits::Runtime,
    {
        Self(Arc::new(runtime))
    }

    /// Watches `source` for readiness on this runtime.
    ///
    /// Every I/O a connection performs on a socket of its own goes through the registration this
    /// returns, so a socket is only ever driven by the runtime the connection was built with.
    pub(crate) fn register_io_source(&self, source: IoSource) -> std::io::Result<io::Registration> {
        self.0.register_io_source(source)
    }

    /// Spawns a task onto the runtime, under the diagnostic name `name`.
    ///
    /// The name is lent to the runtime, which copies it where it keeps one. What the future
    /// produces is boxed as the task ends and unboxed as the handle is read, so that the erased
    /// layer need not know its type: a spawn pays for that box once, and a task whose output is
    /// zero-sized does not pay for it at all.
    pub(crate) fn spawn<T>(
        &self,
        name: &str,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T>
    where
        T: Send + 'static,
    {
        let future = Box::pin(async move { Box::new(future.await) as Box<dyn Any + Send> });

        Task::new(self.0.spawn(name, future))
    }

    /// Connects a stream socket to the TCP endpoint at `address`, the way this runtime does it.
    ///
    /// The socket comes back connected and non-blocking, and no longer watched by the runtime, so
    /// that the connection can register it for its own traffic.
    pub(crate) async fn connect_tcp(&self, address: SocketAddr) -> std::io::Result<IoSource> {
        self.0.connect_tcp(address).await
    }

    /// Connects a stream socket to the unix-domain socket at `path`, the way this runtime does it.
    ///
    /// A path whose first byte is zero names an abstract socket on Linux and Android. The socket
    /// comes back as [`Runtime::connect_tcp`]'s does.
    pub(crate) async fn connect_unix(&self, path: &Path) -> std::io::Result<IoSource> {
        self.0.connect_unix(path).await
    }

    /// Runs `work` off the event loop, on whatever this runtime keeps for blocking work.
    ///
    /// The work is handed to the runtime here and not on the first poll, and the future this
    /// hands back wraps the runtime's own and borrows nothing from here: whoever wants the
    /// outcome awaits it, and whoever does not may drop it, with the work under way either way.
    pub(crate) fn spawn_blocking<T>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> BoxFuture<'static, T>
    where
        T: Send + 'static,
    {
        // The erased runtime only takes work that produces an opaque value, so the result comes
        // back to be downcast to what `work` returned.
        let work = Box::new(move || Box::new(work()) as Box<dyn Any + Send>);
        let outcome = self.0.spawn_blocking(work);

        Box::pin(async move {
            *outcome
                .await
                .downcast()
                .expect("blocking work hands back the value it produced")
        })
    }
}

#[cfg(test)]
mod tests;
