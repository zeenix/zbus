#[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
use super::scheduler::{JoinHandle, Scheduler};
#[cfg(feature = "async-io")]
use async_executor::Executor as AsyncExecutor;
#[cfg(feature = "async-io")]
use async_task::Task as AsyncTask;
#[cfg(feature = "tokio")]
use std::io::Error;
#[cfg(any(feature = "async-io", test, not(feature = "tokio")))]
use std::sync::Arc;
use std::{
    future::Future,
    io::Result,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};
#[cfg(feature = "tokio")]
use tokio::task::JoinHandle as TokioJoinHandle;

/// A wrapper around the underlying runtime/executor.
///
/// This is used to run asynchronous tasks internally and allows integration with various runtimes.
/// See [`crate::Connection::executor`] for an example of integration with external runtimes.
///
/// **Note:** You can (and should) completely ignore this type when the `tokio` backend is in use.
#[derive(Debug, Clone)]
pub struct Executor<'a> {
    inner: Inner,
    // The lifetime is part of the public type; nothing inside needs it since every spawned
    // future is `'static`.
    lifetime: PhantomData<&'a ()>,
}

#[derive(Debug, Clone)]
enum Inner {
    #[cfg(feature = "async-io")]
    AsyncExecutor(Arc<AsyncExecutor<'static>>),
    // tokio spawns onto the ambient runtime; there is nothing to hold.
    #[cfg(feature = "tokio")]
    Tokio,
    #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
    Scheduler(Arc<Scheduler>),
}

impl Executor<'_> {
    /// Spawns a task onto the executor.
    #[doc(hidden)]
    pub fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
        #[allow(unused)] name: &str,
    ) -> Task<T> {
        match &self.inner {
            #[cfg(feature = "async-io")]
            Inner::AsyncExecutor(executor) => {
                Task(TaskInner::AsyncExecutor(executor.spawn(future)))
            }
            #[cfg(feature = "tokio")]
            Inner::Tokio => Task(TaskInner::Tokio(TokioTask::new(tokio_spawn(future, name)))),
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            Inner::Scheduler(scheduler) => Task(TaskInner::Scheduler(scheduler.spawn(future))),
        }
    }

    /// Return `true` if there are no unfinished tasks.
    ///
    /// With the `tokio` backend in use, this always returns `true`.
    pub fn is_empty(&self) -> bool {
        match &self.inner {
            #[cfg(feature = "async-io")]
            Inner::AsyncExecutor(executor) => executor.is_empty(),
            #[cfg(feature = "tokio")]
            Inner::Tokio => true,
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            Inner::Scheduler(scheduler) => scheduler.is_empty(),
        }
    }

    /// Runs a single task.
    ///
    /// With the `tokio` backend in use, it's a noop and never returns.
    pub async fn tick(&self) {
        match &self.inner {
            #[cfg(feature = "async-io")]
            Inner::AsyncExecutor(executor) => executor.tick().await,
            #[cfg(feature = "tokio")]
            Inner::Tokio => std::future::pending().await,
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            Inner::Scheduler(scheduler) => scheduler.tick().await,
        }
    }

    /// Create a new `Executor`.
    pub(crate) fn new() -> Self {
        #[cfg(all(feature = "async-io", feature = "tokio"))]
        {
            if super::use_tokio() {
                Self::from_inner(Inner::Tokio)
            } else {
                Self::with_async_executor()
            }
        }
        #[cfg(all(feature = "async-io", not(feature = "tokio")))]
        {
            Self::with_async_executor()
        }
        #[cfg(all(feature = "tokio", not(feature = "async-io")))]
        {
            Self::from_inner(Inner::Tokio)
        }
        #[cfg(not(any(feature = "async-io", feature = "tokio")))]
        {
            Self::with_scheduler()
        }
    }

    fn from_inner(inner: Inner) -> Self {
        Self {
            inner,
            lifetime: PhantomData,
        }
    }

    #[cfg(feature = "async-io")]
    fn with_async_executor() -> Self {
        Self::from_inner(Inner::AsyncExecutor(Arc::new(AsyncExecutor::new())))
    }

    /// An executor over zbus's own scheduler, for connections that no backend drives.
    #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
    pub(crate) fn with_scheduler() -> Self {
        Self::from_inner(Inner::Scheduler(Arc::new(Scheduler::new())))
    }

    /// Whether this executor needs an external driver thread (only the `async-io` backend does).
    #[cfg(feature = "async-io")]
    pub(crate) fn needs_internal_driver(&self) -> bool {
        matches!(self.inner, Inner::AsyncExecutor(_))
    }

    /// Runs the executor until the given future completes.
    ///
    /// With the `tokio` backend in use, it just awaits on the `future`.
    pub(crate) async fn run<T>(&self, future: impl Future<Output = T>) -> T {
        match &self.inner {
            #[cfg(feature = "async-io")]
            Inner::AsyncExecutor(executor) => executor.run(future).await,
            #[cfg(feature = "tokio")]
            Inner::Tokio => future.await,
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            Inner::Scheduler(scheduler) => scheduler.run(future).await,
        }
    }
}

#[cfg(feature = "tokio")]
fn tokio_spawn<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
    #[allow(unused)] name: &str,
) -> TokioJoinHandle<T> {
    #[cfg(tokio_unstable)]
    {
        tokio::task::Builder::new()
            .name(name)
            .spawn(future)
            // SAFETY: Looking at the code, this call always returns an `Ok`.
            .unwrap()
    }
    #[cfg(not(tokio_unstable))]
    {
        tokio::task::spawn(future)
    }
}

/// A wrapper around the task API of the underlying runtime/executor.
///
/// This follows the semantics of `async_task::Task` on drop:
///
/// * it will be cancelled, rather than detached. For detaching, use the `detach` method.
/// * errors from the task cancellation will will be ignored. If you need to know about task errors,
///   convert the task to a `FallibleTask` using the `fallible` method.
#[doc(hidden)]
#[derive(Debug)]
pub struct Task<T>(TaskInner<T>);

#[derive(Debug)]
enum TaskInner<T> {
    #[cfg(feature = "async-io")]
    AsyncExecutor(AsyncTask<T>),
    #[cfg(feature = "tokio")]
    Tokio(TokioTask<T>),
    #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
    Scheduler(JoinHandle<T>),
}

impl<T> Task<T> {
    /// Detaches the task to let it keep running in the background.
    pub fn detach(self) {
        match self.0 {
            #[cfg(feature = "async-io")]
            TaskInner::AsyncExecutor(task) => task.detach(),
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(task) => task.detach(),
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            TaskInner::Scheduler(handle) => handle.detach(),
        }
    }
}

impl<T> Task<T>
where
    T: Send + 'static,
{
    /// Launch the given blocking function in a task.
    ///
    /// `blocking::unblock` needs no runtime, so async-io's pool is used unless a tokio runtime is
    /// active.
    #[allow(unused)]
    pub(crate) fn spawn_blocking<F>(f: F, #[allow(unused)] name: &str) -> Self
    where
        F: FnOnce() -> T + Send + 'static,
    {
        super::select_runtime! {
            tokio: Self(TaskInner::Tokio(TokioTask::new(tokio_spawn_blocking(f, name)))),
            async_io: Self(TaskInner::AsyncExecutor(blocking::unblock(f))),
        }
    }
}

#[cfg(feature = "tokio")]
fn tokio_spawn_blocking<F, T>(f: F, #[allow(unused)] name: &str) -> TokioJoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    #[cfg(tokio_unstable)]
    {
        tokio::task::Builder::new()
            .name(name)
            .spawn_blocking(f)
            // SAFETY: Looking at the code, this call always returns an `Ok`.
            .unwrap()
    }
    #[cfg(not(tokio_unstable))]
    {
        tokio::task::spawn_blocking(f)
    }
}

impl<T> Future for Task<T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.get_mut().0 {
            #[cfg(feature = "async-io")]
            TaskInner::AsyncExecutor(task) => Pin::new(task).poll(cx).map(Ok),
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(task) => Pin::new(task).poll(cx),
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            TaskInner::Scheduler(handle) => Pin::new(handle).poll(cx),
        }
    }
}

/// A tokio task handle that aborts the task when dropped, matching `async_task::Task`.
#[cfg(feature = "tokio")]
#[derive(Debug)]
struct TokioTask<T>(Option<TokioJoinHandle<T>>);

#[cfg(feature = "tokio")]
impl<T> TokioTask<T> {
    fn new(handle: TokioJoinHandle<T>) -> Self {
        Self(Some(handle))
    }

    fn detach(mut self) {
        // Dropping a tokio `JoinHandle` detaches it.
        drop(self.0.take());
    }
}

#[cfg(feature = "tokio")]
impl<T> Drop for TokioTask<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

#[cfg(feature = "tokio")]
impl<T> Future for TokioTask<T> {
    type Output = Result<T>;

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
                    Err(Error::other("tokio::task cancelled"))
                } else {
                    panic!("tokio::task::JoinHandle error: {e}")
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Executor;
    use futures_lite::future::block_on;

    #[test]
    fn scheduler_backed_executor_runs_its_tasks() {
        let executor = Executor::with_scheduler();
        let task = executor.spawn(async { 7 }, "seven");
        assert!(!executor.is_empty());
        assert_eq!(block_on(executor.run(task)).unwrap(), 7);
        assert!(executor.is_empty());

        executor.spawn(async {}, "detached").detach();
        block_on(executor.run(executor.tick()));
        assert!(executor.is_empty());
    }
}
