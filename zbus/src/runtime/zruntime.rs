//! The zruntime backend: [zruntime]'s reactor for readiness, its timer, its tasks and the
//! standard hook for blocking work.
//!
//! [zruntime]: https://docs.rs/zruntime

use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use super::{Interest, IoSource, traits};

/// The runtime zbus brings along by default: a thin wrapper around [`zruntime::SharedRuntime`].
pub(crate) struct ZRuntime(zruntime::SharedRuntime);

impl ZRuntime {
    /// A handle on the runtime for what this thread builds, brought into being here if none is
    /// alive.
    ///
    /// What can fail is the reactor: it opens the channel a wait is broken through.
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self(zruntime::SharedRuntime::current()?))
    }
}

impl traits::Runtime for ZRuntime {
    type RegisteredIoSource = Registration;
    type Sleep = zruntime::Sleep<zruntime::Shared>;
    type Task<T>
        = Task<T>
    where
        T: Send + 'static;

    fn register_io_source(&self, source: IoSource) -> io::Result<Registration> {
        self.0.register(source).map(Registration)
    }

    fn sleep(&self, duration: Duration) -> zruntime::Sleep<zruntime::Shared> {
        self.0.sleep(duration)
    }

    fn spawn<T>(&self, name: &str, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        // zruntime keeps the name for as long as the task lives, and takes it as a
        // `Cow<'static, str>`. The borrow this is handed does not live that long, so it is copied.
        Task(self.0.spawn(name.to_owned(), future))
    }
}

/// Runs `future` to completion on the calling thread, running that thread's runtime alongside it:
/// see [`zruntime::block_on`].
#[cfg(not(feature = "tokio"))]
pub(crate) fn block_on<F>(future: F) -> F::Output
where
    F: Future,
{
    zruntime::block_on(future)
}

/// A registration on [zruntime]'s reactor.
///
/// [zruntime]: https://docs.rs/zruntime
pub(crate) struct Registration(zruntime::Registration<zruntime::Shared>);

impl traits::PollIo for Registration {
    fn poll_io<T>(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        operation: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        self.0.poll_io(cx, interest.into(), operation)
    }
}

impl From<Interest> for zruntime::Interest {
    fn from(interest: Interest) -> Self {
        // `Interest` is `non_exhaustive` for zbus's own callers, not for this crate, so an
        // exhaustive match is fine here.
        match interest {
            Interest::Readable => zruntime::Interest::Readable,
            Interest::Writable => zruntime::Interest::Writable,
        }
    }
}

/// A task spawned on zruntime, which cancels that task when dropped.
pub(crate) struct Task<T>(zruntime::Task<T, zruntime::Shared>);

impl<T> Future for Task<T> {
    type Output = io::Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().0).poll(cx)
    }
}

impl<T> traits::TaskHandle<T> for Task<T>
where
    T: Send + 'static,
{
    fn detach(self) {
        self.0.detach();
    }
}

#[cfg(test)]
mod tests {
    use ntest::timeout;

    use super::*;
    use crate::runtime::traits::Runtime as _;

    #[test]
    #[timeout(15000)]
    fn a_spawned_task_hands_its_output_back() {
        let runtime = ZRuntime::new().expect("a runtime for this thread");
        let task = runtime.spawn("an answer", async { 42 });

        assert_eq!(zruntime::block_on(task).unwrap(), 42);
    }
}
