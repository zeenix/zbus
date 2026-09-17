//! The handle to a task a connection spawned on its runtime.

use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};

/// A spawned task. Dropping the handle cancels the task;
/// [`TaskHandle::detach`](super::traits::TaskHandle::detach) lets it run on.
#[derive(Debug)]
pub(crate) enum Task<T> {
    #[cfg(feature = "async-io")]
    AsyncIo(super::async_io::Task<T>),
    #[cfg(feature = "tokio")]
    Tokio(super::tokio_rt::TokioTask<T>),
}

impl<T> Task<T>
where
    T: Send + 'static,
{
    /// Detaches the task to let it keep running in the background.
    pub(crate) fn detach(self) {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncIo(task) => super::traits::TaskHandle::detach(task),
            #[cfg(feature = "tokio")]
            Self::Tokio(task) => super::traits::TaskHandle::detach(task),
        }
    }
}

impl<T> Future for Task<T>
where
    T: Send + 'static,
{
    type Output = io::Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.get_mut() {
            #[cfg(feature = "async-io")]
            Self::AsyncIo(task) => Pin::new(task).poll(cx),
            #[cfg(feature = "tokio")]
            Self::Tokio(task) => Pin::new(task).poll(cx),
        }
    }
}

/// Spawns blocking `f` on tokio, naming the task when `tokio_unstable` is on.
#[cfg(feature = "tokio")]
pub(super) fn tokio_spawn_blocking<F, T>(
    f: F,
    #[allow(unused)] name: &str,
) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    #[cfg(tokio_unstable)]
    {
        tokio::task::Builder::new()
            .name(name)
            .spawn_blocking(f)
            .expect("`Builder::spawn_blocking` never returns `Err`")
    }
    #[cfg(not(tokio_unstable))]
    {
        tokio::task::spawn_blocking(f)
    }
}

impl<T> super::traits::TaskHandle<T> for Task<T>
where
    T: Send + 'static,
{
    fn detach(self) {
        Task::detach(self)
    }
}
