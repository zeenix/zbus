//! A runtime implemented over the dev-dependencies, for the tests of the external path.
//!
//! It is a [`traits::Runtime`] over the same smol crates the `async-io` backend is built on,
//! reached as dev-dependencies: those never enter a user's graph, so this runtime exists in
//! every test build, including the one with neither backend compiled in. Its tasks run on one
//! thread per instance, which lives as long as the process, and its registrations and timers are
//! async-io's: enough for a test, not a model for a real host.

#[cfg(feature = "p2p")]
use std::sync::{Mutex, PoisonError};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use async_executor::Executor;
use async_io::{Async, Timer};

use super::{Interest, IoSource, traits};

#[derive(Clone, Debug)]
pub(crate) struct TestRuntime {
    executor: Arc<Executor<'static>>,
}

impl TestRuntime {
    /// A runtime with an executor and a thread of its own.
    pub(crate) fn new() -> Self {
        let executor = Arc::new(Executor::new());

        let runner = executor.clone();
        std::thread::Builder::new()
            .name("zbus test runtime".into())
            .spawn(move || futures_lite::future::block_on(runner.run(std::future::pending::<()>())))
            .expect("failed to spawn the test runtime thread");

        Self { executor }
    }

    /// Whether every task spawned on this runtime is finished or cancelled.
    pub(crate) fn is_empty(&self) -> bool {
        self.executor.is_empty()
    }
}

impl traits::Runtime for TestRuntime {
    type RegisteredIoSource = Registration;
    type Sleep = Sleep;
    type Task<T>
        = Task<T>
    where
        T: Send + 'static;

    fn register_io_source(&self, source: IoSource) -> io::Result<Registration> {
        Async::new(source).map(Registration)
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        Sleep(Timer::after(duration))
    }

    fn spawn<T>(&self, _name: &str, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        Task(self.executor.spawn(future))
    }

    fn spawn_blocking<T>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Pin<Box<dyn Future<Output = T> + Send + 'static>>
    where
        T: Send + 'static,
    {
        Box::pin(blocking::unblock(work))
    }
}

/// A [`TestRuntime`] that keeps [`traits::Runtime::spawn_blocking`]'s default.
///
/// Everything else it hands out is the test runtime's, so a test of the default hook has a
/// runtime to reach it through.
#[derive(Clone, Debug)]
pub(crate) struct DefaultBlocking(TestRuntime);

impl DefaultBlocking {
    pub(crate) fn new() -> Self {
        Self(TestRuntime::new())
    }
}

impl traits::Runtime for DefaultBlocking {
    type RegisteredIoSource = Registration;
    type Sleep = Sleep;
    type Task<T>
        = Task<T>
    where
        T: Send + 'static;

    fn register_io_source(&self, source: IoSource) -> io::Result<Registration> {
        traits::Runtime::register_io_source(&self.0, source)
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        traits::Runtime::sleep(&self.0, duration)
    }

    fn spawn<T>(&self, name: &str, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        traits::Runtime::spawn(&self.0, name, future)
    }
}

/// A [`TestRuntime`] whose tasks a test can end from the outside.
///
/// Only the p2p tests take a connection apart this way, so it is only built where they are.
///
/// The task trait lets a runtime drop or abort what it was handed, and this stands in for one
/// that does: [`AbortingRuntime::abort_tasks`] ends every task spawned on it so far while the
/// connection that asked for them carries on, which is the situation a connection has to survive.
///
/// The handles pile up: an `AbortHandle` says nothing about whether its task has finished, so
/// there is no pruning one that has. A test aborts once and then drops the runtime, so the list
/// never outgrows the tasks of a single connection.
#[cfg(feature = "p2p")]
#[derive(Clone, Debug)]
pub(crate) struct AbortingRuntime {
    inner: TestRuntime,
    // Shared with every clone, so a test that hands one to a connection can still reach the
    // tasks that connection spawned.
    handles: Arc<Mutex<Vec<futures_util::future::AbortHandle>>>,
}

#[cfg(feature = "p2p")]
impl AbortingRuntime {
    pub(crate) fn new() -> Self {
        Self {
            inner: TestRuntime::new(),
            handles: Arc::default(),
        }
    }

    /// Ends every task spawned on this runtime so far.
    pub(crate) fn abort_tasks(&self) {
        let handles =
            std::mem::take(&mut *self.handles.lock().unwrap_or_else(PoisonError::into_inner));

        for handle in handles {
            handle.abort();
        }
    }
}

#[cfg(feature = "p2p")]
impl traits::Runtime for AbortingRuntime {
    type RegisteredIoSource = Registration;
    type Sleep = Sleep;
    type Task<T>
        = AbortableTask<T>
    where
        T: Send + 'static;

    fn register_io_source(&self, source: IoSource) -> io::Result<Registration> {
        traits::Runtime::register_io_source(&self.inner, source)
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        traits::Runtime::sleep(&self.inner, duration)
    }

    fn spawn<T>(
        &self,
        name: &str,
        future: impl Future<Output = T> + Send + 'static,
    ) -> AbortableTask<T>
    where
        T: Send + 'static,
    {
        let (future, handle) = futures_util::future::abortable(future);
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handle);

        // An aborted task leaves its future unfinished, which is exactly what is under test.
        AbortableTask(traits::Runtime::spawn(&self.inner, name, future))
    }
}

/// A registration on async-io's reactor.
#[derive(Debug)]
pub(crate) struct Registration(Async<IoSource>);

impl traits::PollIo for Registration {
    fn poll_io<T>(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        mut operation: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        loop {
            match operation() {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                result => return Poll::Ready(result),
            }
            let ready = match interest {
                Interest::Readable => self.0.poll_readable(cx),
                Interest::Writable => self.0.poll_writable(cx),
            };
            if let Err(e) = std::task::ready!(ready) {
                return Poll::Ready(Err(e));
            }
        }
    }
}

/// A timer with the completion instant dropped.
#[derive(Debug)]
pub(crate) struct Sleep(Timer);

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll(cx).map(drop)
    }
}

/// An executor task: dropping it cancels the task.
#[derive(Debug)]
pub(crate) struct Task<T>(async_task::Task<T>);

impl<T> traits::TaskHandle<T> for Task<T>
where
    T: Send + 'static,
{
    fn detach(self) {
        self.0.detach();
    }
}

impl<T> Future for Task<T> {
    type Output = io::Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(Ok)
    }
}

/// A task of an [`AbortingRuntime`], whose abort reaches the caller as a lost task.
#[cfg(feature = "p2p")]
pub(crate) struct AbortableTask<T>(Task<Result<T, futures_util::future::Aborted>>);

#[cfg(feature = "p2p")]
impl<T> Future for AbortableTask<T> {
    type Output = io::Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(|outcome| match outcome {
            Ok(Ok(value)) => Ok(value),
            // An aborted task produced nothing, which is what the trait's `Err` is for: a
            // runtime that lost the task.
            Ok(Err(_)) => Err(io::Error::other("the runtime aborted the task")),
            Err(e) => Err(e),
        })
    }
}

#[cfg(feature = "p2p")]
impl<T> traits::TaskHandle<T> for AbortableTask<T>
where
    T: Send + 'static,
{
    fn detach(self) {
        traits::TaskHandle::detach(self.0)
    }
}
