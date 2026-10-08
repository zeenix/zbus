//! The handle to a task a connection spawned on its runtime.

use std::{
    fmt,
    future::Future,
    io,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

use super::{erased::ErasedTask, traits};

/// A spawned task, carrying the type of what that task produces.
///
/// Dropping the handle cancels the task; [`TaskHandle::detach`](traits::TaskHandle::detach) lets
/// it run on. The erased layer hands every output back as an opaque value, and this is where it
/// becomes the type the caller spawned again.
pub(crate) struct Task<T> {
    inner: Box<dyn ErasedTask>,
    // `fn() -> T` carries none of `T`'s auto traits, leaving them to `inner`; what makes that
    // sound is `Runtime::spawn`'s `T: Send`.
    value: PhantomData<fn() -> T>,
}

impl<T> Task<T>
where
    T: Send + 'static,
{
    /// Detaches the task to let it keep running in the background.
    pub(crate) fn detach(self) {
        ErasedTask::detach(self.inner)
    }

    /// The handle to `inner`, a task whose future boxes the `T` it produces.
    pub(super) fn new(inner: Box<dyn ErasedTask>) -> Self {
        Self {
            inner,
            value: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Task<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

impl<T> Future for Task<T>
where
    T: 'static,
{
    type Output = io::Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().inner)
            .poll(cx)
            .map_ok(|value| *value.downcast().expect(PRODUCED))
    }
}

impl<T> traits::TaskHandle<T> for Task<T>
where
    T: Send + 'static,
{
    fn detach(self) {
        Task::detach(self)
    }
}

const PRODUCED: &str = "a task hands back the value its future produced";
