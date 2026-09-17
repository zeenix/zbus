//! Blocking work on the `blocking` crate's thread pool, where the async-io backend puts it.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

/// Hands `work` to the `blocking` crate's thread pool.
///
/// The work runs whether or not the future this returns is polled, and whether or not it is
/// dropped; [`DetachOnDrop`] is what sees to the last of those.
pub(super) fn run<T>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Pin<Box<dyn Future<Output = T> + Send + 'static>>
where
    T: Send + 'static,
{
    Box::pin(DetachOnDrop(Some(blocking::unblock(work))))
}

/// The outcome of a task that is left to run when this future is let go of.
///
/// Dropping an `async_task::Task` cancels the task behind it, and work that has not started by
/// then is thrown away rather than run. Blocking work has to outlive the interest in its outcome:
/// the wait for a helper process owns that process, so a wait thrown away along with the future
/// for it — by a connection attempt that was cancelled, say — is a process nothing will ever
/// reap. Detaching the task on the way out is what leaves the work to the pool.
struct DetachOnDrop<T>(Option<async_task::Task<T>>);

impl<T> Future for DetachOnDrop<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let task = self
            .0
            .as_mut()
            .expect("the task is only let go of as this future is dropped");

        Pin::new(task).poll(cx)
    }
}

impl<T> Drop for DetachOnDrop<T> {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use async_task::{Runnable, Task};
    use ntest::timeout;

    use super::*;

    /// Work that is still waiting its turn when the future for it is let go of runs all the same.
    #[test]
    #[timeout(15000)]
    fn dropping_the_future_leaves_queued_work_to_run() {
        let (runnable, task, ran) = queued();

        drop(DetachOnDrop(Some(task)));
        runnable.run();

        assert!(ran.load(Ordering::SeqCst));
    }

    /// Letting go of an `async_task::Task` throws away work that is still waiting its turn.
    #[test]
    #[timeout(15000)]
    fn dropping_a_bare_task_handle_throws_queued_work_away() {
        let (runnable, task, ran) = queued();

        drop(task);
        runnable.run();

        assert!(!ran.load(Ordering::SeqCst));
    }

    /// A task nobody has run yet: what would run it, its handle, and the flag it sets.
    ///
    /// Its schedule leaves the runnable in a queue nothing is draining, which is the pool with
    /// every worker busy — the case where cancelling the task costs the work rather than merely
    /// the interest in its outcome.
    fn queued() -> (Runnable, Task<()>, Arc<AtomicBool>) {
        let ran = Arc::new(AtomicBool::new(false));
        let setting = ran.clone();
        let queue: Arc<Mutex<Vec<Runnable>>> = Arc::default();

        let scheduling = queue.clone();
        let (runnable, task) = async_task::spawn(
            async move { setting.store(true, Ordering::SeqCst) },
            move |runnable| scheduling.lock().unwrap().push(runnable),
        );
        runnable.schedule();

        let runnable = queue.lock().unwrap().pop().expect("the task was scheduled");

        (runnable, task, ran)
    }
}
