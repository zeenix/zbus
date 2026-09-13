//! Object-safe mirrors of the runtime traits.
//!
//! `Builder::runtime` takes any implementation and the connection must not be generic over it,
//! so the implementation is boxed behind these traits once. Operations that are generic over a
//! value type erase the value too: what a task produces becomes a `Box<dyn Any + Send>` that
//! the typed handle downcasts, which is a `TypeId` comparison per access.
//!
//! Only what a connection asks its runtime for today is mirrored here: readiness and blocking
//! work are still taken from the backend that owns the socket, not from the connection's runtime.

use std::{
    any::Any,
    fmt,
    future::Future,
    io,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use super::traits;

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait ErasedRuntime: Send + Sync {
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
    fn spawn(
        &self,
        name: &str,
        future: BoxFuture<'static, Box<dyn Any + Send>>,
    ) -> Box<dyn ErasedTask>;
    #[cfg(test)]
    fn spawn_blocking(
        &self,
        work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
    ) -> BoxFuture<'static, Box<dyn Any + Send>>;
}

impl fmt::Debug for dyn ErasedRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<external runtime>")
    }
}

impl<R> ErasedRuntime for R
where
    R: traits::Runtime,
{
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

    #[cfg(test)]
    fn spawn_blocking(
        &self,
        work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
    ) -> BoxFuture<'static, Box<dyn Any + Send>> {
        traits::Runtime::spawn_blocking(self, work)
    }
}

pub(crate) trait ErasedTask:
    Future<Output = io::Result<Box<dyn Any + Send>>> + Send + Sync + Unpin
{
    fn detach(self: Box<Self>);
}

impl fmt::Debug for dyn ErasedTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<external task>")
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

/// A task on an external runtime, carrying the type of what that task produces.
///
/// The mirror hands every output back as an opaque value, and this is where it becomes the type
/// the caller spawned again.
pub(crate) struct ExternalTask<T> {
    inner: Box<dyn ErasedTask>,
    // `fn() -> T` carries none of `T`'s auto traits, leaving them to `inner`; what makes that
    // sound is `Runtime::spawn`'s `T: Send`.
    value: PhantomData<fn() -> T>,
}

impl<T> ExternalTask<T>
where
    T: Send + 'static,
{
    /// Spawns `future` on `runtime`, boxing what it produces on the way out.
    ///
    /// That box is made as the task ends and undone as the output is read, so a spawn pays for
    /// it once and a task that produces nothing does not pay for it at all.
    pub(crate) fn spawn(
        runtime: &dyn ErasedRuntime,
        name: &str,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Self {
        let future = Box::pin(async move { Box::new(future.await) as Box<dyn Any + Send> });

        Self {
            inner: runtime.spawn(name, future),
            value: PhantomData,
        }
    }
}

impl<T> fmt::Debug for ExternalTask<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

impl<T> Future for ExternalTask<T>
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

impl<T> traits::TaskHandle<T> for ExternalTask<T>
where
    T: Send + 'static,
{
    fn detach(self) {
        ErasedTask::detach(self.inner)
    }
}

const PRODUCED: &str = "a task hands back the value its future produced";
