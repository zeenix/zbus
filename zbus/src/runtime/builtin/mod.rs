//! The runtime zbus brings along, for a connection that is handed no runtime of its own.
//!
//! It is made of three parts: a scheduler, which holds the connection's tasks and hands them out
//! to be polled; a reactor, which watches its sockets and keeps its timers; and a worker thread,
//! which runs the two. Everything [`traits::Runtime`] asks of a runtime reaches one of them: a
//! spawned future is queued on the scheduler, a registered source is the reactor's and so is a
//! timer, from the first poll of it onwards, and blocking work goes to a thread of its own, as
//! the trait's default has it.
//!
//! The worker starts on the first piece of work handed to the runtime — a task spawned, a source
//! registered, a timer polled — and retires in the round it finds nothing left to run, watch or
//! time. So a connection built without a runtime of its own costs one thread, and that thread
//! only for as long as it has something to do, and one channel for breaking that thread's wait
//! from another: a pipe on unix and a socket pair on Windows, two descriptors either way, open
//! for as long as the runtime is.
//!
//! A panic outside a task — in a waker, say — ends the worker thread with its flag cleared, and
//! the wakers the reactor had already taken out of its maps to wake are dropped in the unwind
//! without ever being woken. The waiters those wakers belonged to are served once the next spawn,
//! registration or timer poll starts a worker again.
//!
//! A runtime lives as long as any clone of [`Builtin`] does, and its worker holds the three
//! parts for as long as it runs, so work already under way is finished even where the last of
//! those clones is let go of while it is running. A detached task that never finishes is
//! therefore a thread for the life of the process, along with everything that task's future
//! holds: nothing here takes a runtime down, and the scheduler lets go of a task only once the
//! worker has left.

mod poll;
mod reactor;
mod scheduler;
mod worker;

use std::{
    fmt,
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    task::{Context, Poll},
    thread,
    time::Duration,
};

use reactor::Reactor;
use scheduler::{JoinHandle, Scheduler};

use super::{IoSource, traits};

/// The runtime zbus brings along: a scheduler, a reactor, and a worker thread to run them.
#[derive(Clone)]
pub(crate) struct Builtin {
    inner: Arc<Inner>,
}

impl Builtin {
    /// A runtime with a scheduler and a reactor of its own, and no thread until it is given
    /// something to do.
    ///
    /// What can fail here is the reactor: it opens the channel a worker's wait is broken
    /// through.
    pub(crate) fn new() -> io::Result<Self> {
        let reactor = Arc::new(Reactor::new()?);
        let scheduler = {
            let reactor = reactor.clone();

            // A task that becomes ready breaks the wait its worker is in. Nothing in the reactor
            // points back at the scheduler, so this hook closes no cycle.
            Arc::new(Scheduler::new(move || reactor.notify()))
        };

        Ok(Self {
            inner: Arc::new(Inner {
                scheduler,
                reactor,
                worker: Mutex::new(false),
            }),
        })
    }

    /// Whether a worker thread is running.
    #[cfg(test)]
    pub(super) fn worker_running(&self) -> bool {
        *lock(&self.inner.worker)
    }
}

impl fmt::Debug for Builtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builtin")
            .field("worker", &*lock(&self.inner.worker))
            .finish_non_exhaustive()
    }
}

impl traits::Runtime for Builtin {
    type RegisteredIoSource = reactor::RegisteredIoSource;
    type Sleep = Sleep;
    type Task<T>
        = Task<T>
    where
        T: Send + 'static;

    fn register_io_source(&self, source: IoSource) -> io::Result<reactor::RegisteredIoSource> {
        let registered = self.inner.reactor.register(source)?;
        // Asked for once the source is in the reactor's map, so that a worker starting here
        // takes it into its very first wait.
        self.inner.ensure_worker();

        Ok(registered)
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        Sleep {
            inner: self.inner.clone(),
            sleep: self.inner.reactor.sleep(duration),
        }
    }

    fn spawn<T>(&self, name: &str, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        let task = Task(self.inner.scheduler.spawn(name, future));
        // Asked for once the task is on the scheduler's queue, so that a worker starting here
        // finds it there.
        self.inner.ensure_worker();

        task
    }
}

/// A timer on a built-in runtime, which keeps a worker for as long as it has a deadline.
///
/// The reactor takes a timer's deadline on the first poll of it rather than where it is made, and
/// the thread that is to fire it has to be there from that poll onwards, however long ago the
/// timer was asked for. So it is the poll that asks for a worker, and a timer nobody ever polls
/// costs nothing at all.
pub(crate) struct Sleep {
    inner: Arc<Inner>,
    sleep: reactor::Sleep,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if Pin::new(&mut this.sleep).poll(cx).is_ready() {
            return Poll::Ready(());
        }
        // Asked for once the deadline is in the reactor's map, so that a worker starting here
        // waits on it.
        this.inner.ensure_worker();

        Poll::Pending
    }
}

/// A task spawned on a built-in runtime, which cancels that task when dropped.
pub(crate) struct Task<T>(JoinHandle<T>);

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

/// What a runtime is made of, shared by every clone of it and by the worker that runs it.
pub(super) struct Inner {
    scheduler: Arc<Scheduler>,
    reactor: Arc<Reactor>,
    /// Whether a worker thread is running; the lock the start and exit decisions are made under.
    worker: Mutex<bool>,
}

impl Inner {
    /// Starts the worker unless one is running. Called after the work it is to see is in place
    /// (a task queued, a source registered, a deadline stored), never before.
    ///
    /// That order is what makes the hand-off safe either way round: a worker that starts here
    /// finds the work, and one that is already running either sees it in the round it is in or
    /// finds it where it decides whether to retire, which it does under this very lock.
    ///
    /// The thread holds a runtime of its own, so whatever has been handed over runs to
    /// completion even once every [`Builtin`] clone is gone.
    fn ensure_worker(self: &Arc<Self>) {
        let mut running = lock(&self.worker);
        if *running {
            return;
        }
        let inner = self.clone();
        thread::Builder::new()
            .name(worker::THREAD_NAME.into())
            .spawn(move || worker::run(inner))
            .expect("the thread a connection's runtime runs on");
        // Set once the thread is there: a spawn that fails panics with the flag clear, so that
        // the next call tries again rather than wait on a thread that was never started.
        *running = true;
    }

    /// Marks the worker as gone if it has nothing left to do. Under the `worker` lock, so that
    /// a spawn or a registration racing with the exit either is seen here or starts a worker
    /// itself once the lock is released.
    fn retire_if_idle(&self) -> bool {
        let mut running = lock(&self.worker);
        let busy = self.scheduler.has_ready()
            || self.scheduler.live_tasks() > 0
            || !self.reactor.is_idle();
        if busy {
            return false;
        }
        *running = false;

        true
    }
}

/// The value behind a lock, taken whether or not a panic poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
