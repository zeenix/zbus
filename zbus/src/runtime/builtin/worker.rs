//! The thread a built-in runtime runs on: ready tasks first, then one wait on the reactor
//! bounded by the nearest deadline, until nothing is left to run, watch or time.

use std::{cell::Cell, ptr::NonNull, sync::Arc, thread, time::Duration};

use super::{Inner, reactor::Reactor};
use crate::log::error;

/// How many ready tasks run between two looks at the reactor, so a busy task queue cannot
/// starve a socket or a timer.
const BATCH: usize = 32;

/// The name a worker thread carries: twelve bytes, which fits the fifteen Linux keeps for one.
pub(super) const THREAD_NAME: &str = "zbus runtime";

thread_local! {
    /// The reactor of the runtime this thread is the worker of, and nothing on a thread that is
    /// no worker at all.
    static WORKER_REACTOR: Cell<Option<NonNull<Reactor>>> = const { Cell::new(None) };
}

/// Whether the calling thread is the worker of the runtime `reactor` belongs to.
///
/// A process holds a runtime per connection that was built without one of its own, each with a
/// worker of its own, and a wake travels between them: a task of one connection wakes a task of
/// another wherever the two talk. Such a wake has to break the other worker's wait, so what is
/// asked here is which runtime this thread works for, not whether it works for one at all.
pub(super) fn on_worker_thread(reactor: &Reactor) -> bool {
    WORKER_REACTOR.with(Cell::get) == Some(NonNull::from(reactor))
}

/// Runs `inner`'s tasks and its reactor until there is nothing left of either.
///
/// Each round polls what is ready, a batch at a time, and then asks whether anything is left to
/// run, watch or time. Where nothing is, the thread retires there and then, rather than sit in a
/// wait until something comes along to tell it what it could have worked out for itself. Where
/// something is, the round ends in one wait on the reactor: bounded by no time at all where a
/// task is ready, so that the round after it polls that task, and by nothing where none is, so
/// that the thread sleeps until a source, a deadline or a notification has something for it.
///
/// That bound is worked out inside the wait rather than here, because the queue it is read from
/// is one of the things the wait announces itself before reading.
pub(super) fn run(inner: Arc<Inner>) {
    let _marks = Marks::put_up(&inner);
    let mut failed_waits = 0u32;
    loop {
        for _ in 0..BATCH {
            if !inner.scheduler.run_one() {
                break;
            }
        }
        if inner.retire_if_idle() {
            return;
        }
        match inner
            .reactor
            .wait(|| inner.scheduler.has_ready().then_some(Duration::ZERO))
        {
            Ok(()) => failed_waits = 0,
            Err(e) => {
                failed_waits += 1;
                if failed_waits == 1 {
                    error!("The runtime's wait failed: {}", e);
                }
                // Every waiter retries its own operation and sees its own error; the pause
                // keeps a wait that fails every time from becoming a spin.
                inner.reactor.wake_everything();
                thread::sleep(Duration::from_millis(1 << failed_waits.min(10)));
            }
        }
    }
}

/// What says a thread is a worker, for as long as it is one.
struct Marks<'a>(&'a Inner);

impl<'a> Marks<'a> {
    /// Marks the calling thread as the worker of `inner`'s reactor.
    ///
    /// The reactor is named by its address, which stands for it alone while the mark is up: the
    /// thread holds the runtime, so the reactor cannot be dropped and its place taken by
    /// another until the mark comes down here.
    fn put_up(inner: &'a Arc<Inner>) -> Self {
        WORKER_REACTOR.with(|reactor| reactor.set(Some(NonNull::from(&*inner.reactor))));

        Self(inner)
    }
}

impl Drop for Marks<'_> {
    fn drop(&mut self) {
        WORKER_REACTOR.with(|reactor| reactor.set(None));
        // A worker that returns has cleared the running flag itself, under the lock that decides
        // whether another is to start; one that unwinds has not, and clearing it here is what
        // makes the next spawn, registration or timer poll start a worker rather than wait on
        // one that is gone.
        if thread::panicking() {
            *super::lock(&self.0.worker) = false;
        }
    }
}
