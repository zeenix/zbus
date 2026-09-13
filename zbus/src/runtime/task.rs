//! The handle to a task a connection spawned on its runtime.

use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};

use super::erased::ExternalTask;

/// A spawned task. Dropping the handle cancels the task; [`TaskHandle::detach`] lets it run on.
#[derive(Debug)]
pub(crate) struct Task<T>(pub(super) TaskInner<T>);

#[derive(Debug)]
pub(super) enum TaskInner<T> {
    #[cfg(feature = "async-io")]
    AsyncIo(super::async_io::Task<T>),
    #[cfg(feature = "tokio")]
    Tokio(super::tokio_rt::TokioTask<T>),
    /// A task on a runtime the builder was handed, reached through the object-safe mirror of
    /// the task trait.
    External(ExternalTask<T>),
}

impl<T> Task<T>
where
    T: Send + 'static,
{
    /// Detaches the task to let it keep running in the background.
    pub(crate) fn detach(self) {
        match self.0 {
            #[cfg(feature = "async-io")]
            TaskInner::AsyncIo(task) => super::traits::TaskHandle::detach(task),
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(task) => super::traits::TaskHandle::detach(task),
            TaskInner::External(handle) => super::traits::TaskHandle::detach(handle),
        }
    }
}

impl<T> Future for Task<T>
where
    T: Send + 'static,
{
    type Output = io::Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.get_mut().0 {
            #[cfg(feature = "async-io")]
            TaskInner::AsyncIo(task) => Pin::new(task).poll(cx),
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(task) => Pin::new(task).poll(cx),
            TaskInner::External(handle) => Pin::new(handle).poll(cx),
        }
    }
}
