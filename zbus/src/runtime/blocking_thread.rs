//! Blocking work for a runtime that keeps no threads for it.

use std::{
    any::Any,
    future::Future,
    panic::{self, AssertUnwindSafe},
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError},
    task::{Context, Poll, Waker},
};

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
    let state = Arc::new(Mutex::new(State {
        outcome: None,
        waker: None,
    }));

    let thread_state = state.clone();
    std::thread::Builder::new()
        .name(THREAD_NAME.into())
        .spawn(move || {
            // Constructed before `work` runs and dropped only once this closure returns, so it
            // covers every way out of the work, storing the outcome of it included.
            let _finish = Finish(&thread_state);

            let outcome = panic::catch_unwind(AssertUnwindSafe(work));
            lock(&thread_state).outcome = Some(outcome);
        })
        .expect("failed to spawn a thread for blocking work");

    Box::pin(Blocking(state))
}

/// The name of every thread started here.
const THREAD_NAME: &str = "zbus blocking work";

/// The future [`run`] hands back, which resolves to the outcome of the work.
///
/// Dropping it before then takes back the waker it left for the thread, so that giving up on the
/// work, as a cancelled connection attempt does, leaves nothing of the task that waited, or of its
/// runtime, with the thread for as long as the work goes on. The work itself runs to its end
/// either way.
struct Blocking<T>(Arc<Mutex<State<T>>>);

impl<T> Future for Blocking<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut guard = lock(&self.0);
        let Some(outcome) = guard.outcome.take() else {
            let waker = cx.waker();
            let replaced = match guard.waker.as_ref() {
                Some(stored) if stored.will_wake(waker) => None,
                _ => guard.waker.replace(waker.clone()),
            };
            drop(guard);
            // Past the lock, which belongs to this hand-over alone and is none of a dropped
            // waker's business.
            drop(replaced);
            return Poll::Pending;
        };
        // Taking the waker here too, whether or not this poll needed it, keeps a future that
        // resolves without ever being woken from leaving one behind for `Finish` to wake later,
        // once whoever it belongs to may already be gone.
        let leftover = guard.waker.take();
        drop(guard);
        drop(leftover);

        match outcome {
            Ok(value) => Poll::Ready(value),
            Err(panic) => panic::resume_unwind(panic),
        }
    }
}

impl<T> Drop for Blocking<T> {
    fn drop(&mut self) {
        // Besides this future, only the thread takes the lock, and only once the work is over: to
        // store the outcome, and then in `Finish`, which takes the waker out and wakes it. So a
        // lock this drop finds taken leaves the waker with the thread for no longer than the
        // thread has left to run, and waiting for the lock instead could wait for good: a `wake`
        // from inside `Finish` may drop this very future on that thread, as an executor does with
        // a task it can no longer run.
        let mut state = match self.0.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let waker = state.waker.take();
        drop(state);
        // Past the lock, as in `poll`.
        drop(waker);
    }
}

/// Where the thread leaves the outcome of the work, and the waker it hands back to.
///
/// Both live behind the one lock, so that seeing the outcome and taking the waker to wake it are
/// never two separate steps from the other side's point of view: see [`Finish`] for what that
/// buys.
struct State<T> {
    /// The outcome of the work, once the thread has produced it.
    outcome: Option<Outcome<T>>,
    /// The waker of the task that polled for that outcome last.
    waker: Option<Waker>,
}

/// What the work returned, or what it panicked with.
type Outcome<T> = Result<T, Box<dyn Any + Send>>;

/// Wakes whoever is waiting for the outcome as the thread leaves, before letting go of the lock
/// the waiting side has to take to see that outcome.
///
/// While this drop holds [`State`]'s lock, [`Blocking`] cannot take it and so cannot see the
/// outcome stored under it; by the time it can, the waker this drop took out has been woken and,
/// with nothing else left holding it, dropped. A poll that reaches the lock first, in the gap
/// between the outcome being stored and this drop running, takes the waker itself instead,
/// leaving this drop nothing to wake. Either way, the awaiting task can only complete once this
/// thread holds nothing that came from the runtime that polled it, and that runtime is then free
/// to be torn down with nothing of it left on this thread.
///
/// Waking while still holding the lock cannot deadlock here: the lock is private to this
/// hand-over, reachable from nowhere but the closure above and the future it wakes. A `wake` that
/// drops that future, as an executor may with a task it can no longer run, finds the lock taken
/// and leaves it be, the waker having been taken out already: see [`Blocking`]'s drop.
struct Finish<'a, T>(&'a Mutex<State<T>>);

impl<T> Drop for Finish<'_, T> {
    fn drop(&mut self) {
        let mut state = lock(self.0);
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
    }
}

/// The state behind its lock, taken whether or not a panic poisoned it.
fn lock<T>(state: &Mutex<State<T>>) -> MutexGuard<'_, State<T>> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, task::Wake, time::Duration};

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

    /// A waker whose `wake` takes long enough that a poll able to take the lock as soon as this
    /// thread lets go of the waker, rather than only once it is done with it, would resolve the
    /// future well before this returns.
    struct SlowWake;

    impl Wake for SlowWake {
        fn wake(self: Arc<Self>) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A waker that unparks the thread it was made on, as a `block_on` does.
    struct Unpark(std::thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    #[test]
    #[timeout(15000)]
    fn a_dropped_future_takes_its_waker_back_while_the_work_runs_on() {
        let probe = Arc::new(Unpark(std::thread::current()));
        let (release, released) = mpsc::channel::<()>();
        let (report, reported) = mpsc::channel();

        // `future`, `cx` and `waker` drop at the end of this block while the work still waits on
        // the channel: past it, only the thread could still hold the waker the future left.
        {
            let waker = Waker::from(probe.clone());
            let mut cx = Context::from_waker(&waker);
            let mut future = run(move || {
                released.recv().expect("the test lets the work go on");
                report.send(()).expect("the test waits for the work");
            });

            assert!(future.as_mut().poll(&mut cx).is_pending());
        }

        assert_eq!(
            Arc::strong_count(&probe),
            1,
            "the thread kept the waker of a future that was dropped",
        );
        release.send(()).expect("the work waits for the test");
        reported.recv().expect("the work runs to its end");
    }

    #[test]
    #[timeout(15000)]
    fn the_thread_drops_the_waker_before_the_future_it_wakes_can_resolve() {
        let probe = Arc::new(SlowWake);

        // `future`, `cx` and `waker` drop, in that order, at the end of this block: past it,
        // nothing but `probe` itself still refers to the waker the future was polled with.
        let value = {
            let waker = Waker::from(probe.clone());
            let mut cx = Context::from_waker(&waker);
            let mut future = run(|| {
                std::thread::sleep(Duration::from_millis(20));
                42
            });

            loop {
                match future.as_mut().poll(&mut cx) {
                    Poll::Ready(value) => break value,
                    Poll::Pending => std::thread::sleep(Duration::from_millis(1)),
                }
            }
        };

        assert_eq!(value, 42);
        assert_eq!(
            Arc::strong_count(&probe),
            1,
            "the thread's waker outlived the future it woke",
        );
    }
}
