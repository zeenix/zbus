//! Every runtime, in two layers.
//!
//! [`Builder::runtime`] takes any implementation of the runtime traits and a connection must not
//! be generic over it, so each runtime is boxed once behind the object-safe mirrors of those
//! traits that make up the lower layer here: [`ErasedRuntime`] and one trait per thing a runtime
//! hands out. Everything generic is erased on the way through: what a task produces becomes a
//! `Box<dyn Any + Send>` and a future becomes a trait object of its own.
//!
//! The upper layer puts the public traits back on top of the boxed mirrors: the methods of
//! [`Runtime`] box what they hand down and downcast what comes back, and [`Task`] carries the type
//! of the value a task produces again. Every runtime takes this path, one of the built-in
//! backends and one handed to the builder alike, so a connection reaches all of them the same way
//! and nothing in the crate depends on which one it runs on.
//!
//! What the layers cost, for every runtime. A registration costs one allocation and so does a
//! sleep. A spawn costs two, the future and the handle, and a third as the task ends for a value
//! that is not zero-sized. A call to the blocking hook costs three — the work, the value it hands
//! back and the future that downcasts that value — on top of the boxed future the hook itself
//! returns. A connect costs the one allocation of the boxed future its method returns, and a
//! spawned process costs none, its method returning that future boxed already.
//!
//! Readiness is mirrored without erasing anything: the operation a registration runs hands its
//! result back through the caller's own captures, so an I/O call costs no allocation here.
//!
//! [`Builder::runtime`]: crate::connection::Builder::runtime
//! [`Runtime`]: super::Runtime
//! [`Task`]: super::Task

#[cfg(all(unix, any(feature = "unixexec", feature = "ibus", target_os = "macos")))]
use std::process::{Command, ExitStatus, Stdio};
use std::{
    any::Any,
    fmt,
    future::Future,
    io,
    net::SocketAddr,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use super::{Interest, IoSource, traits};

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait ErasedRuntime: Send + Sync {
    fn register_io_source(&self, source: IoSource) -> io::Result<Box<dyn ErasedRegistration>>;
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
    fn spawn(
        &self,
        name: &str,
        future: BoxFuture<'static, Box<dyn Any + Send>>,
    ) -> Box<dyn ErasedTask>;
    fn connect_tcp(&self, address: SocketAddr) -> BoxFuture<'_, io::Result<IoSource>>;
    fn connect_unix<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<IoSource>>;
    #[cfg(all(unix, any(feature = "unixexec", feature = "ibus", target_os = "macos")))]
    fn spawn_process(
        &self,
        command: Command,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
    ) -> io::Result<BoxFuture<'static, io::Result<ExitStatus>>>;
    fn spawn_blocking(
        &self,
        work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
    ) -> BoxFuture<'static, Box<dyn Any + Send>>;
}

impl fmt::Debug for dyn ErasedRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<runtime>")
    }
}

impl<R> ErasedRuntime for R
where
    R: traits::Runtime,
{
    fn register_io_source(&self, source: IoSource) -> io::Result<Box<dyn ErasedRegistration>> {
        traits::Runtime::register_io_source(self, source)
            .map(|registration| Box::new(registration) as Box<dyn ErasedRegistration>)
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(traits::Runtime::sleep(self, duration))
    }

    fn spawn(
        &self,
        name: &str,
        future: BoxFuture<'static, Box<dyn Any + Send>>,
    ) -> Box<dyn ErasedTask> {
        Box::new(traits::Runtime::spawn(self, name, future))
    }

    fn connect_tcp(&self, address: SocketAddr) -> BoxFuture<'_, io::Result<IoSource>> {
        Box::pin(traits::Runtime::connect_tcp(self, address))
    }

    fn connect_unix<'a>(&'a self, path: &'a Path) -> BoxFuture<'a, io::Result<IoSource>> {
        Box::pin(traits::Runtime::connect_unix(self, path))
    }

    #[cfg(all(unix, any(feature = "unixexec", feature = "ibus", target_os = "macos")))]
    fn spawn_process(
        &self,
        command: Command,
        stdin: Stdio,
        stdout: Stdio,
        stderr: Stdio,
    ) -> io::Result<BoxFuture<'static, io::Result<ExitStatus>>> {
        traits::Runtime::spawn_process(self, command, stdin, stdout, stderr)
    }

    fn spawn_blocking(
        &self,
        work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
    ) -> BoxFuture<'static, Box<dyn Any + Send>> {
        traits::Runtime::spawn_blocking(self, work)
    }
}

/// The object-safe mirror of [`traits::PollIo`].
///
/// The operation returns nothing, so that nothing has to be boxed for it: the typed caller wraps
/// its own operation in a closure that keeps the value it produced.
pub(crate) trait ErasedRegistration: Send + Sync {
    fn poll_io(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        operation: &mut dyn FnMut() -> io::Result<()>,
    ) -> Poll<io::Result<()>>;
}

impl fmt::Debug for dyn ErasedRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<registration>")
    }
}

impl<R> ErasedRegistration for R
where
    R: traits::PollIo,
{
    fn poll_io(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        operation: &mut dyn FnMut() -> io::Result<()>,
    ) -> Poll<io::Result<()>> {
        traits::PollIo::poll_io(self, cx, interest, operation)
    }
}

impl traits::PollIo for Box<dyn ErasedRegistration> {
    fn poll_io<T>(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        mut operation: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        // The mirror hands nothing back, so what the operation produced travels in a local the
        // wrapping closure fills in.
        let mut value = None;
        let ready = ErasedRegistration::poll_io(&**self, cx, interest, &mut || {
            value = Some(operation()?);

            Ok(())
        });

        match ready {
            Poll::Ready(result) => Poll::Ready(result.map(|()| {
                value
                    .take()
                    .expect("a successful operation produced a value")
            })),
            Poll::Pending => {
                // A value taken off the socket and then dropped here is bytes gone from the
                // stream, which surfaces much later as a message that will not parse, so this
                // one holds wherever it runs.
                assert!(
                    value.is_none(),
                    "a registration must not report `Pending` for an operation that produced a \
                     value",
                );

                Poll::Pending
            }
        }
    }
}

pub(crate) trait ErasedTask:
    Future<Output = io::Result<Box<dyn Any + Send>>> + Send + Sync + Unpin
{
    fn detach(self: Box<Self>);
}

impl fmt::Debug for dyn ErasedTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<task>")
    }
}

impl<H> ErasedTask for H
where
    H: traits::TaskHandle<Box<dyn Any + Send>>,
{
    fn detach(self: Box<Self>) {
        traits::TaskHandle::detach(*self)
    }
}
