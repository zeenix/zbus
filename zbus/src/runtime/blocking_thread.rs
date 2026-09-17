//! Blocking work for a runtime that keeps no threads for it.

use std::{
    any::Any,
    future::Future,
    panic::{self, AssertUnwindSafe},
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
};

use event_listener::Event;

/// Runs `work` on a thread of its own.
///
/// The thread starts right away and ends with the work; the future resolves as soon as the
/// outcome has been handed over. A panic in `work` travels with that outcome and is raised again
/// in the awaiting task, so it reaches the caller wherever the work ran. Starting the thread is
/// the one failure with nowhere to go: it panics.
pub(super) fn run<T>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Pin<Box<dyn Future<Output = T> + Send + 'static>>
where
    T: Send + 'static,
{
    let handover = Arc::new(Handover {
        outcome: Mutex::new(None),
        stored: Event::new(),
    });

    let thread_handover = handover.clone();
    std::thread::Builder::new()
        .name(THREAD_NAME.into())
        .spawn(move || {
            // Waking from a guard, which the thread drops last, covers every way out of the
            // work, storing the outcome of it included.
            let _wake = Wake(&thread_handover.stored);

            let outcome = panic::catch_unwind(AssertUnwindSafe(work));
            *thread_handover
                .outcome
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(outcome);
        })
        .expect("failed to spawn a thread for blocking work");

    Box::pin(async move {
        // Listening before the first look is what makes the hand-over race-free: an outcome
        // stored between the two is still announced to this listener.
        let stored = handover.stored.listen();
        let outcome = match take(&handover) {
            Some(outcome) => outcome,
            None => {
                stored.await;
                take(&handover).expect("the thread stores its outcome before it wakes this task")
            }
        };

        match outcome {
            Ok(value) => value,
            Err(panic) => panic::resume_unwind(panic),
        }
    })
}

/// The name of every thread started here.
const THREAD_NAME: &str = "zbus blocking work";

/// Where the thread leaves the outcome of the work for the future to pick up.
struct Handover<T> {
    outcome: Mutex<Option<Outcome<T>>>,
    stored: Event,
}

/// What the work returned, or what it panicked with.
type Outcome<T> = Result<T, Box<dyn Any + Send>>;

/// The outcome, once the thread has stored it.
fn take<T>(handover: &Handover<T>) -> Option<Outcome<T>> {
    handover
        .outcome
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

/// Wakes the awaiting task as the thread leaves.
struct Wake<'a>(&'a Event);

impl Drop for Wake<'_> {
    fn drop(&mut self) {
        self.0.notify(1);
    }
}

#[cfg(test)]
mod tests {
    use ntest::timeout;

    use super::*;

    #[test]
    #[timeout(15000)]
    fn the_work_hands_its_value_back() {
        assert_eq!(futures_lite::future::block_on(run(|| 42)), 42);
    }

    #[test]
    #[timeout(15000)]
    fn a_panic_in_the_default_blocking_hook_reaches_the_caller() {
        let panicked = panic::catch_unwind(|| {
            futures_lite::future::block_on(run(|| panic!("the blocking work panicked")))
        })
        .expect_err("awaiting work that panicked panics in turn");

        assert_eq!(
            panicked.downcast_ref::<&str>(),
            Some(&"the blocking work panicked"),
        );
    }
}
