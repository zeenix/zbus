//! The built-in runtime: async-io for readiness and timers, async-executor for tasks, `blocking`
//! for blocking work.

use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll},
    time::Duration,
};

use async_executor::Executor;
use async_io::{Async, Timer};

use super::{Interest, IoSource, traits, unblock};

/// The runtime zbus uses by default: async-io for readiness and timers, async-executor for
/// tasks.
///
/// Every connection built without an explicit runtime on the `async-io` feature runs on an
/// instance of this type. Its tasks run on a `zbus::Connection executor` thread that starts on
/// the first spawn, exits as soon as the executor runs dry, and starts again on the next spawn;
/// a detached task that never finishes keeps the thread alive for as long as it runs. Spawning
/// panics if that thread cannot be started, as [`traits::Runtime::spawn`] has no way to report
/// the failure.
#[derive(Clone, Debug, Default)]
pub(crate) struct AsyncIo {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    executor: Executor<'static>,
    thread_running: Mutex<bool>,
}

impl AsyncIo {
    /// A runtime with an executor of its own.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Starts the executor thread unless one is already running.
    ///
    /// The thread owns a strong reference, so whatever is already queued runs to completion even
    /// once every other `AsyncIo` clone is gone, and it gives that reference up as soon as the
    /// executor is empty. Clearing the flag under the lock this takes is what makes the hand-off
    /// race-free: either the outgoing thread still sees the task the caller has just pushed, or
    /// the caller finds the flag cleared and starts a new thread.
    fn ensure_thread(&self) {
        let mut running = self
            .inner
            .thread_running
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *running {
            return;
        }

        let inner = self.inner.clone();
        std::thread::Builder::new()
            .name("zbus::Connection executor".into())
            .spawn(move || run_executor(inner))
            .expect("failed to spawn the zbus executor thread");
        *running = true;
    }
}

/// Runs `Inner::executor` until it runs dry.
///
/// `utils::block_on` picks tokio when both backends are enabled, which would make tasks on this
/// executor unexpectedly observe a tokio runtime, so the thread blocks on async-io directly.
fn run_executor(inner: Arc<Inner>) {
    async_io::block_on(async {
        loop {
            inner.executor.tick().await;

            let mut running = inner
                .thread_running
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // Cancelled tasks were dropped by the tick that observed the cancellation, so an
            // empty executor here means there is nothing left for this thread to do.
            if inner.executor.is_empty() {
                *running = false;
                break;
            }
        }
    })
}

impl traits::Runtime for AsyncIo {
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
        // async-executor has nowhere to keep a task's name, so the name is dropped here.
        let task = self.inner.executor.spawn(future);
        self.ensure_thread();
        Task(task)
    }

    fn spawn_blocking<T>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Pin<Box<dyn Future<Output = T> + Send + 'static>>
    where
        T: Send + 'static,
    {
        unblock::run(work)
    }
}

/// An async-io registration.
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
            // async-io re-arms the interest and returns `Pending` unless a newer readiness
            // event is already there, so this loop cannot spin.
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

/// An async-io timer with the completion instant dropped.
#[derive(Debug)]
pub(crate) struct Sleep(Timer);

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll(cx).map(drop)
    }
}

/// An async-task handle: dropping it cancels the task.
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

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Instant};

    use ntest::timeout;

    use super::*;
    use crate::runtime::traits::Runtime as _;

    #[test]
    #[timeout(15000)]
    fn a_spawned_task_runs_to_completion() {
        let runtime = AsyncIo::new();
        let (sender, receiver) = mpsc::channel();
        let task = runtime.spawn("a spawned task", async move {
            sender.send(42).expect("the receiver is still alive");
        });

        async_io::block_on(task).unwrap();
        assert_eq!(receiver.recv().unwrap(), 42);
    }

    #[test]
    #[timeout(15000)]
    fn a_spawned_task_hands_its_output_back() {
        let runtime = AsyncIo::new();
        let task = runtime.spawn("a task with an output", async { 42 });

        assert_eq!(async_io::block_on(task).unwrap(), 42);
    }

    #[test]
    #[timeout(15000)]
    fn the_executor_thread_starts_on_the_first_spawn() {
        let runtime = AsyncIo::new();
        let idle = runtime.spawn("an idle task", std::future::pending::<()>());
        assert!(!runtime.inner.executor.is_empty());
        assert_eq!(
            thread_name(&runtime).as_deref(),
            Some("zbus::Connection executor"),
        );

        drop(idle);
        // The task is only dropped by the tick that observes the cancellation, and the thread
        // only leaves once it has, so give it the chance to get there.
        async_io::block_on(async {
            while *runtime.inner.thread_running.lock().unwrap() {
                Timer::after(Duration::from_millis(1)).await;
            }
        });
        assert!(runtime.inner.executor.is_empty());

        // A spawn after the thread gave up starts a new one.
        assert_eq!(
            thread_name(&runtime).as_deref(),
            Some("zbus::Connection executor"),
        );
    }

    /// The name of the thread a task of `runtime` runs on.
    fn thread_name(runtime: &AsyncIo) -> Option<Box<str>> {
        let (sender, receiver) = mpsc::channel();
        async_io::block_on(runtime.spawn("thread name", async move {
            let name = std::thread::current().name().map(Box::from);
            sender.send(name).expect("the receiver is still alive");
        }))
        .unwrap();

        receiver.recv().unwrap()
    }

    #[cfg(unix)]
    #[test]
    #[timeout(15000)]
    fn a_registration_reports_readiness() {
        use std::{
            future::poll_fn,
            io::{Read, Write},
            os::unix::net::UnixStream,
        };

        let (reader, mut writer) = UnixStream::pair().unwrap();
        // The registration is given a duplicate of the descriptor, so the end this test keeps
        // refers to the same open file description and hence to the same readiness.
        reader.set_nonblocking(true).unwrap();
        let runtime = AsyncIo::new();
        let registration = runtime
            .register_io_source(IoSource::new(reader.try_clone().unwrap().into()))
            .unwrap();

        let writing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            writer.write_all(b"!").unwrap();
        });

        let mut byte = [0; 1];
        let read = async_io::block_on(poll_fn(|cx| {
            traits::PollIo::poll_io(&registration, cx, Interest::Readable, || {
                (&reader).read(&mut byte)
            })
        }))
        .unwrap();
        writing.join().unwrap();

        assert_eq!(read, 1);
        assert_eq!(&byte, b"!");
    }

    #[test]
    #[timeout(15000)]
    fn sleep_resolves_once_the_duration_has_passed() {
        let runtime = AsyncIo::new();

        async_io::block_on(async {
            runtime.sleep(Duration::ZERO).await;

            let start = Instant::now();
            runtime.sleep(Duration::from_millis(20)).await;
            assert!(start.elapsed() >= Duration::from_millis(20));
        });
    }
}
