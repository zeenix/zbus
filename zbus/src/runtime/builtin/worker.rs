//! The thread a built-in runtime runs on: ready tasks first, then one wait on the reactor
//! bounded by the nearest deadline, until nothing is left to run, watch or time.

use std::{cell::Cell, sync::Arc, thread, time::Duration};

use super::Inner;
use crate::log::error;

/// How many ready tasks run between two looks at the reactor, so a busy task queue cannot
/// starve a socket or a timer.
const BATCH: usize = 32;

/// The name a worker thread carries: twelve bytes, which fits the fifteen Linux keeps for one.
pub(super) const THREAD_NAME: &str = "zbus runtime";

thread_local! {
    static ON_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Whether the calling thread is a runtime's worker.
pub(super) fn on_worker_thread() -> bool {
    ON_WORKER.with(Cell::get)
}

/// Runs `inner`'s tasks and its reactor until there is nothing left of either.
///
/// Each round polls what is ready, a batch at a time, and then asks whether anything is left to
/// run, watch or time. Where nothing is, the thread retires there and then, rather than sit in a
/// wait until something comes along to tell it what it could have worked out for itself. Where
/// something is, the round ends in one wait on the reactor: bounded by no time at all where a
/// task is ready, so that the round after it polls that task, and by nothing where none is, so
/// that the thread sleeps until a source, a deadline or a notification has something for it.
pub(super) fn run(inner: Arc<Inner>) {
    ON_WORKER.with(|flag| flag.set(true));
    let _guard = ClearOnUnwind(&inner);
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
        let at_most = inner.scheduler.has_ready().then_some(Duration::ZERO);
        match inner.reactor.wait(at_most) {
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

/// Clears the running flag if the worker unwinds, so that the next spawn, registration or timer
/// poll starts a worker instead of waiting on one that is gone.
struct ClearOnUnwind<'a>(&'a Inner);

impl Drop for ClearOnUnwind<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            *super::lock(&self.0.worker) = false;
        }
    }
}
