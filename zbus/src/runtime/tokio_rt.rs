//! The Tokio backend: Tokio's reactor for readiness, its timer, its tasks and its thread pool
//! for blocking work.
//!
//! The socket a connection registers is watched through Tokio's `AsyncFd` on unix. On Windows
//! Tokio watches only the sockets of its own types, each of which owns its socket, so the
//! registration there is a `TcpStream` made from a duplicate of the connection's socket handle.
//! The duplicate refers to the same socket as the handle the connection reads and writes through,
//! and the stream is there to tell when that socket is ready. Tokio has no unix-domain sockets on
//! Windows, and mio, which it watches sockets with, has none either, so the registration of a
//! unix-domain socket there is `Unsupported`.

use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

#[cfg(windows)]
use socket2::{Domain, SockRef};
#[cfg(unix)]
use tokio::io::unix::AsyncFd;
use tokio::runtime::Handle;

use super::{Interest, IoSource, traits};

/// The Tokio runtime a connection runs on.
///
/// The handle is taken when the connection is built, so its tasks, timers and blocking work keep
/// going to that runtime even where the connection is later polled from a thread with no Tokio
/// runtime current.
#[derive(Clone, Debug)]
pub(crate) struct Tokio {
    handle: Handle,
}

impl Tokio {
    /// The runtime current on this thread, or `None` outside one.
    pub(crate) fn current() -> Option<Self> {
        Handle::try_current().ok().map(|handle| Self { handle })
    }
}

impl traits::Runtime for Tokio {
    type RegisteredIoSource = Registration;
    type Sleep = tokio::time::Sleep;
    type Task<T>
        = TokioTask<T>
    where
        T: Send + 'static;

    #[cfg(unix)]
    fn register_io_source(&self, source: IoSource) -> io::Result<Registration> {
        // A registration is made on the runtime of the calling thread, which is not necessarily
        // the one this connection belongs to.
        let _guard = self.handle.enter();

        AsyncFd::new(source).map(Registration)
    }

    #[cfg(windows)]
    fn register_io_source(&self, source: IoSource) -> io::Result<Registration> {
        let socket = SockRef::from(&source);
        // A socket's address says which kind of socket it is, and every socket zbus registers is
        // connected, so it has one. Tokio cannot watch a unix-domain socket here: see the module
        // documentation.
        if socket.local_addr()?.domain() == Domain::UNIX {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        // Tokio's stream owns the socket it watches, so it is given a duplicate of the handle.
        // The socket is in non-blocking mode already, and the mode belongs to the socket, which
        // both handles refer to.
        let stream = std::net::TcpStream::from(socket.try_clone()?);
        // A registration is made on the runtime of the calling thread, which is not necessarily
        // the one this connection belongs to.
        let _guard = self.handle.enter();

        tokio::net::TcpStream::from_std(stream).map(Registration)
    }

    fn sleep(&self, duration: Duration) -> tokio::time::Sleep {
        // A timer is armed on the runtime of the calling thread, as a registration is.
        let _guard = self.handle.enter();

        // Tokio times on a clock of its own, which a test can pause and advance by hand, so the
        // wait is handed over as a length for that clock to measure rather than as a deadline
        // read off the standard one.
        tokio::time::sleep(duration)
    }

    fn spawn<T>(&self, name: &str, future: impl Future<Output = T> + Send + 'static) -> TokioTask<T>
    where
        T: Send + 'static,
    {
        TokioTask::new(spawn_named(&self.handle, name, future))
    }

    fn spawn_blocking<T>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Pin<Box<dyn Future<Output = T> + Send + 'static>>
    where
        T: Send + 'static,
    {
        let task = self.handle.spawn_blocking(work);

        Box::pin(async move {
            match task.await {
                Ok(value) => value,
                Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                Err(_) => {
                    panic!("the Tokio runtime shut down while zbus was waiting on blocking work")
                }
            }
        })
    }
}

/// A registration on Tokio's reactor.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct Registration(AsyncFd<IoSource>);

#[cfg(unix)]
impl traits::PollIo for Registration {
    fn poll_io<T>(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        mut operation: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        loop {
            // Tokio hands out a guard for the readiness it has observed and only re-arms the
            // interest once that guard is cleared, so this loop cannot spin.
            let mut guard = match interest {
                Interest::Readable => std::task::ready!(self.0.poll_read_ready(cx))?,
                Interest::Writable => std::task::ready!(self.0.poll_write_ready(cx))?,
            };
            match operation() {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => guard.clear_ready(),
                result => return Poll::Ready(result),
            }
        }
    }
}

/// A registration on Tokio's reactor.
///
/// It holds the `TcpStream` made from a duplicate of the connection's socket handle, which is what
/// Tokio watches.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct Registration(tokio::net::TcpStream);

#[cfg(windows)]
impl traits::PollIo for Registration {
    /// Waits for Tokio's readiness of the stream, then runs `operation` under its `try_io`.
    ///
    /// `try_io` runs the operation only while Tokio holds readiness for the interest, and clears
    /// that readiness when the operation reports `WouldBlock`. The stream is mio's underneath, and
    /// mio's poll of a Windows socket reports an event once: it is armed again by a `WouldBlock`
    /// that goes through mio's own `try_io`, which Tokio's calls. Neither looks at which handle the
    /// operation used. Here it reads or writes the connection's own handle, which refers to the
    /// same socket as the stream's, so the readiness waited for is that socket's, and so is the
    /// `WouldBlock` that clears it and arms mio again.
    ///
    /// A `WouldBlock` sends the loop back to wait for readiness, which is `Pending` until mio
    /// reports some, so the loop does not spin.
    fn poll_io<T>(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        mut operation: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        let tokio_interest = match interest {
            Interest::Readable => tokio::io::Interest::READABLE,
            Interest::Writable => tokio::io::Interest::WRITABLE,
        };

        loop {
            std::task::ready!(match interest {
                Interest::Readable => self.0.poll_read_ready(cx),
                Interest::Writable => self.0.poll_write_ready(cx),
            })?;
            match self.0.try_io(tokio_interest, &mut operation) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                result => return Poll::Ready(result),
            }
        }
    }
}

/// Spawns `future` on `handle`, named where Tokio has somewhere to keep a name.
///
/// A task's name is what tokio-console and a task dump show it by, and the builder that takes
/// one only exists with `--cfg tokio_unstable` set *and* Tokio's own `tracing` feature on, which
/// zbus's `tracing` feature is what turns on. Anywhere else the name has nowhere to go.
fn spawn_named<F>(handle: &Handle, name: &str, future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    #[cfg(all(tokio_unstable, feature = "tracing"))]
    {
        tokio::task::Builder::new()
            .name(name)
            .spawn_on(future, handle)
            .expect("`Builder::spawn_on` never returns `Err`")
    }
    #[cfg(not(all(tokio_unstable, feature = "tracing")))]
    {
        // Tokio's plain spawn takes no name, so this one goes no further than here.
        let _ = name;

        handle.spawn(future)
    }
}

/// A tokio task handle that aborts the task when dropped, matching `async_task::Task`.
#[derive(Debug)]
pub(crate) struct TokioTask<T>(Option<tokio::task::JoinHandle<T>>);

impl<T> TokioTask<T> {
    pub(super) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self(Some(handle))
    }
}

impl<T> traits::TaskHandle<T> for TokioTask<T>
where
    T: Send + 'static,
{
    fn detach(mut self) {
        // Dropping a tokio `JoinHandle` detaches it.
        drop(self.0.take());
    }
}

impl<T> Drop for TokioTask<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

impl<T> Future for TokioTask<T> {
    type Output = io::Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let handle = self
            .get_mut()
            .0
            .as_mut()
            .expect("a `TokioTask` is only polled before it is detached or dropped");
        Pin::new(handle).poll(cx).map(|r| match r {
            Ok(v) => Ok(v),
            Err(e) => {
                if e.is_cancelled() {
                    Err(io::Error::other("tokio::task cancelled"))
                } else {
                    panic!("tokio::task::JoinHandle error: {e}")
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use ntest::timeout;

    use super::*;
    use crate::runtime::traits::Runtime as _;

    #[tokio::test]
    #[timeout(15000)]
    async fn a_spawned_task_hands_its_output_back() {
        let runtime = Tokio::current().expect("a Tokio runtime is current");
        let task = runtime.spawn("a task with an output", async { 42 });

        assert_eq!(task.await.unwrap(), 42);
    }
}
