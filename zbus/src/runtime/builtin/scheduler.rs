//! The tasks of one runtime, and the queue of those that are ready to be polled.
//!
//! A scheduler holds every task spawned on it and hands them out one at a time to be polled, on
//! whichever thread calls [`Scheduler::run_one`]. It has no thread of its own: the thread
//! running the runtime calls that method for as long as there is anything to run.
//!
//! Every task lives in a cell that holds its future, and each cell with something to do sits in
//! the ready queue. Waking a task, from any thread, puts its cell on that queue and calls the
//! `notify` hook the scheduler was built with, which is how a thread waiting with nothing to do
//! learns that it has a task to poll. A wake for a cell that is already queued, or whose future
//! has gone, does nothing, so a task woken three times is polled once; a wake that arrives while
//! the task is being polled is remembered and queues the cell again once that poll returns.
//!
//! [`Inner::spawn`] hands back a handle that resolves to what the task produced. Dropping
//! the handle cancels the task: the future is dropped there and then if nobody is polling it,
//! and by the poll under way otherwise. A task whose outcome is of no further interest is
//! detached instead, and runs until it ends.
//!
//! A task that panics takes nothing with it. The panic is caught where the task is polled, the
//! task ends there, its handle reports the failure, and the thread that polled it carries on
//! with the next task.
//!
//! A cell's lock is always taken before the scheduler's own, never the other way round. Neither
//! is held while a future is polled or dropped, nor while a waker is woken, because all three
//! run code that may spawn a task, cancel one or wake another on this very scheduler — the
//! destructor of a future is where a connection hands over the last of its work — and that code
//! takes the locks that would be held. What a cell is up to is therefore kept under its lock as
//! one state whose variants carry only the flags that apply to them, rather than in atomics, so
//! that a poll which has just returned settles what to do next from one consistent view of the
//! wake and of the cancellation that may have arrived while it ran.

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    future::Future,
    io, mem,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    pin::{Pin, pin},
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
    task::{Context, Poll, Wake, Waker},
};

use crate::log::error;

use super::Inner;

/// The tasks of one runtime: what has been spawned on it, and what is ready to be polled.
pub(super) struct Scheduler {
    state: Mutex<State>,
    /// Called, from any thread, whenever the thread running the runtime has something new to
    /// look at.
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Scheduler {
    /// An empty scheduler, which calls `notify` whenever a task becomes ready to be polled.
    pub(super) fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            state: Mutex::new(State {
                ready: VecDeque::new(),
                live: HashMap::new(),
                next_id: 0,
            }),
            notify: Box::new(notify),
        }
    }

    /// Polls one ready task. `false` when the queue was empty.
    pub(super) fn run_one(&self) -> bool {
        let Some(cell) = lock(&self.state).ready.pop_front() else {
            return false;
        };
        let mut future = {
            let mut guard = lock(&cell.cell);
            match mem::replace(
                &mut *guard,
                CellState::Running {
                    woken: false,
                    cancelled: false,
                },
            ) {
                // The queue entry popped above is spent by this replacement: `Running` carries
                // no `queued` flag, so a wake that arrives during the poll queues the cell
                // afresh rather than reusing this entry.
                CellState::Idle { future, .. } => future,
                // Nothing to poll: the future is either gone or already out of its cell. A
                // cancelled cell may still sit in the queue as `Done`, so a stale entry like
                // that is accepted here rather than treated as a bug.
                state => {
                    *guard = state;

                    return true;
                }
            }
        };

        let waker = Waker::from(cell.clone());
        // Whatever a panic left half-done is in the future, and a task that panicked is marked
        // `Done`, dropped and forgotten below, so nothing here reads that half-done state and
        // nothing polls the future again.
        let polled = catch_unwind(AssertUnwindSafe(|| {
            future.as_mut().poll(&mut Context::from_waker(&waker))
        }));
        let panicked = polled.is_err();
        let ready = matches!(polled, Ok(Poll::Ready(())));

        // A future that is finished with is taken out of its cell here and dropped below, with
        // no lock held.
        let finished = {
            let mut guard = lock(&cell.cell);
            let CellState::Running { woken, cancelled } = *guard else {
                unreachable!("only this call takes a cell out of `Running`");
            };
            if ready || panicked || cancelled {
                *guard = CellState::Done;
                lock(&self.state).live.remove(&cell.id);

                Some(future)
            } else {
                *guard = CellState::Idle {
                    future,
                    queued: woken,
                };
                if woken {
                    lock(&self.state).ready.push_back(cell.clone());
                }

                None
            }
        };

        if panicked {
            cell.joint.fail(io::Error::other("the task panicked"));
            error!("The task `{}` panicked", cell.name);
        }
        // Nothing carries the panic any further, so its payload ends here: once the task is
        // marked done, taken off the live map and failed, so that a payload whose own destructor
        // panics cannot leave the task behind with nobody to finish it. That destructor is the
        // task's code as much as the future is, and is contained the same way.
        dispose(polled);
        if let Some(future) = finished {
            // A destructor is free to panic and free to spawn, so it runs here: caught, and
            // clear of every lock a spawn of its own would take.
            dispose(future);
        }

        true
    }

    /// Whether a task is waiting to be polled.
    pub(super) fn has_ready(&self) -> bool {
        !lock(&self.state).ready.is_empty()
    }

    /// How many spawned futures have neither finished nor been cancelled.
    pub(super) fn live_tasks(&self) -> usize {
        lock(&self.state).live.len()
    }
}

impl Inner {
    /// Queues `future` and hands back the handle that joins or cancels it.
    ///
    /// `name` says what the task is there for. It is for diagnostics alone: the message a
    /// panicking task logs, and the handle's [`Debug`](fmt::Debug).
    pub(super) fn spawn<T>(
        self: &Arc<Self>,
        name: &str,
        future: impl Future<Output = T> + Send + 'static,
    ) -> JoinHandle<T>
    where
        T: Send + 'static,
    {
        let joint = Arc::new(Mutex::new(Joint {
            output: None,
            waker: None,
        }));
        let wrapped = {
            let joint = joint.clone();
            async move {
                // Pinned in place and awaited through that pin, rather than awaited by value:
                // the output is then stored before the future it came from is dropped, at the
                // end of this block, so a future whose destructor panics hands its value back
                // all the same.
                let mut future = pin!(future);
                Joint::finish(&joint, Ok(future.as_mut().await));
            }
        };
        let cell = {
            let mut state = lock(&self.scheduler.state);
            let id = state.next_id;
            state.next_id += 1;
            let cell = Arc::new(TaskCell {
                id,
                name: name.into(),
                runtime: Arc::downgrade(self),
                joint: joint.clone(),
                cell: Mutex::new(CellState::Idle {
                    future: Box::pin(wrapped),
                    queued: false,
                }),
            });
            state.live.insert(id, cell.clone());

            cell
        };
        // Queued through a wake, so that a task reaches the queue by one path only.
        cell.wake_by_ref();

        JoinHandle {
            cell,
            joint,
            detached: false,
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // A cell outlives the scheduler wherever something holds a waker of it, so every future
        // is taken out here rather than left to go with the last of those wakers.
        let cells: Vec<Arc<TaskCell>> = {
            let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
            state
                .ready
                .drain(..)
                .chain(state.live.drain().map(|(_, cell)| cell))
                .collect()
        };
        let mut futures = Vec::new();
        for cell in cells {
            let state = mem::replace(&mut *lock(&cell.cell), CellState::Done);
            if let CellState::Idle { future, .. } = state {
                futures.push(future);
            }
            // The task will never produce anything, and whoever waits on its handle is told so
            // rather than left waiting for a task no thread will ever poll again.
            cell.joint
                .fail(io::Error::other("the task's runtime is gone"));
        }
        for future in futures {
            dispose(future);
        }
    }
}

/// Drops `value` with every panic its destructor raises contained.
///
/// A task's panic is contained per task: the destructor of its payload, and of that payload's
/// own payload should it have one, is part of that same task rather than of whoever polls it.
fn dispose<T>(value: T) {
    let Err(payload) = catch_unwind(AssertUnwindSafe(move || drop(value))) else {
        return;
    };
    // A payload whose own destructor panics is leaked rather than dropped a third time: the
    // panic is contained either way, and a chain of destructors that each panic with the next
    // is not one the scheduler should follow to its end.
    if let Err(payload) = catch_unwind(AssertUnwindSafe(move || drop(payload))) {
        mem::forget(payload);
    }
}

/// Joins a spawned task, and cancels it when dropped unless [`JoinHandle::detach`] was called.
pub(super) struct JoinHandle<T> {
    cell: Arc<TaskCell>,
    joint: Arc<Mutex<Joint<T>>>,
    detached: bool,
}

impl<T> JoinHandle<T> {
    /// Lets the task run to completion on its own.
    pub(super) fn detach(mut self) {
        self.detached = true;
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = io::Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut joint = lock(&self.joint);
        let Some(output) = joint.output.take() else {
            joint.waker = Some(cx.waker().clone());

            return Poll::Pending;
        };

        Poll::Ready(output)
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if self.detached {
            return;
        }
        let runtime = self.cell.runtime.upgrade();
        let scheduler = runtime.as_ref().map(|runtime| &runtime.scheduler);
        let future = {
            let mut guard = lock(&self.cell.cell);
            match mem::replace(&mut *guard, CellState::Done) {
                CellState::Idle { future, .. } => {
                    if let Some(scheduler) = &scheduler {
                        lock(&scheduler.state).live.remove(&self.cell.id);
                    }

                    Some(future)
                }
                // Whoever is polling the task drops the future once that poll returns.
                CellState::Running { woken, .. } => {
                    *guard = CellState::Running {
                        woken,
                        cancelled: true,
                    };

                    None
                }
                // Finished, or cancelled by an earlier drop: nothing to take back, nobody to
                // tell.
                CellState::Done => return,
            }
        };
        // On the thread that let the handle go, with no lock held: a destructor that panics
        // here panics where the handle was dropped, and one that spawns is served as any other
        // caller is.
        let dropped = catch_unwind(AssertUnwindSafe(move || drop(future)));
        self.cell
            .joint
            .fail(io::Error::other("the task was cancelled"));
        if let Some(scheduler) = scheduler {
            // Even where nothing was queued: a thread waiting with this as its last task learns
            // from it that it can retire.
            (scheduler.notify)();
        }
        // Carried on from here, so that the panic is still the caller's to see while the task it
        // belonged to is closed off either way: the notification above is what lets an idle
        // thread retire, and somebody's destructor panicking is no reason to leave one up.
        if let Err(payload) = dropped {
            resume_unwind(payload);
        }
    }
}

impl<T> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinHandle")
            .field("task", &self.cell.name)
            .field("detached", &self.detached)
            .finish_non_exhaustive()
    }
}

struct State {
    ready: VecDeque<Arc<TaskCell>>,
    /// Every cell that holds a future: spawned, and neither finished, cancelled nor panicked.
    ///
    /// Keyed, rather than a list to scan: an object server hands each method call a task of its
    /// own, and a burst of calls would make a scan per completion quadratic.
    live: HashMap<u64, Arc<TaskCell>>,
    next_id: u64,
}

/// One task: its future, and what the scheduler and its handle need to know about it.
struct TaskCell {
    id: u64,
    name: Box<str>,
    runtime: Weak<Inner>,
    /// The handle's side of the task, reached from here without knowing what the task produces.
    joint: Arc<dyn Fail>,
    cell: Mutex<CellState>,
}

impl Wake for TaskCell {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let Some(runtime) = self.runtime.upgrade() else {
            // The runtime is gone, and with it the queue this cell would go on.
            return;
        };
        let scheduler = &runtime.scheduler;
        let mut guard = lock(&self.cell);
        match &mut *guard {
            // The future is out of its cell; the poll it is out for queues the cell again.
            CellState::Running { woken, .. } => {
                *woken = true;

                return;
            }
            // There is nothing left to poll.
            CellState::Done => return,
            CellState::Idle { queued: true, .. } => return,
            CellState::Idle { queued, .. } => *queued = true,
        }
        lock(&scheduler.state).ready.push_back(self.clone());
        drop(guard);

        // Clear of both locks: whoever is running the runtime may pick the task up on another
        // thread the moment it hears of it, and what it does there takes them.
        (scheduler.notify)();
    }
}

/// A spawned future, boxed so that every task has a cell of the same type.
type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// What a cell holding a task is up to.
enum CellState {
    /// Waiting to be polled, with its future in hand.
    Idle {
        /// The task's future.
        future: BoxFuture,
        /// Whether the cell sits in the ready queue.
        queued: bool,
    },
    /// Out of its cell, being polled.
    Running {
        /// Whether a wake arrived while the task was being polled, so it is to be polled again.
        woken: bool,
        /// Whether the handle was dropped while the task was being polled, so the future is to
        /// be dropped once that poll returns.
        cancelled: bool,
    },
    /// Gone: the task finished, was cancelled or panicked.
    Done,
}

/// Where a task's output waits for the handle that joins it.
struct Joint<T> {
    output: Option<io::Result<T>>,
    waker: Option<Waker>,
}

impl<T> Joint<T> {
    /// Stores `output` and wakes whoever waits for it.
    fn finish(joint: &Mutex<Self>, output: io::Result<T>) {
        let waker = {
            let mut joint = lock(joint);
            joint.output = Some(output);
            joint.waker.take()
        };
        // Outside the lock: the handle may be polled to completion on this very thread.
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// Failing a task without knowing what that task produces.
///
/// A cell has the one type whatever its task is, so the way it reaches the output side of that
/// task is through this.
trait Fail: Send + Sync {
    /// Hands `error` to the handle, unless an output is already waiting for it.
    fn fail(&self, error: io::Error);
}

impl<T> Fail for Mutex<Joint<T>>
where
    T: Send,
{
    fn fail(&self, error: io::Error) {
        let waker = {
            let mut joint = lock(self);
            if joint.output.is_some() {
                return;
            }
            joint.output = Some(Err(error));
            joint.waker.take()
        };
        // Outside the lock, as a handle woken here may be polled on this very thread.
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// The value behind a lock, taken whether or not a panic poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        thread,
    };

    use event_listener::Event;
    use futures_lite::future::{block_on, yield_now};
    use ntest::timeout;

    use super::*;

    #[test]
    #[timeout(15000)]
    fn a_spawned_task_runs_and_hands_its_output_back() {
        let (runtime, _) = runtime();
        let handle = runtime.spawn("an answer", async { 42 });

        drive(&runtime.scheduler);

        assert_eq!(block_on(handle).unwrap(), 42);
        assert_eq!(runtime.scheduler.live_tasks(), 0);
    }

    #[test]
    #[timeout(15000)]
    fn a_wake_from_another_thread_notifies_the_worker() {
        let (runtime, notifications) = runtime();
        let event = Arc::new(Event::new());
        let handle = {
            let event = event.clone();
            runtime.spawn("a waiting task", async move {
                event.listen().await;
                "woken"
            })
        };

        // Polled here, so that it is waiting for the event rather than for its first poll.
        drive(&runtime.scheduler);
        assert!(!runtime.scheduler.has_ready());
        let before = *lock(&notifications);

        thread::spawn(move || {
            event.notify(1);
        })
        .join()
        .unwrap();

        assert_eq!(*lock(&notifications), before + 1);
        assert!(runtime.scheduler.has_ready());
        drive(&runtime.scheduler);
        assert_eq!(block_on(handle).unwrap(), "woken");
    }

    #[test]
    #[timeout(15000)]
    fn a_task_woken_during_its_own_poll_runs_again() {
        struct WakeOnce(bool);

        impl Future for WakeOnce {
            type Output = ();

            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                if self.0 {
                    return Poll::Ready(());
                }
                self.0 = true;
                cx.waker().wake_by_ref();

                Poll::Pending
            }
        }

        let (runtime, _) = runtime();
        let handle = runtime.spawn("a self-waking task", WakeOnce(false));

        assert!(runtime.scheduler.run_one());
        assert!(runtime.scheduler.has_ready());
        assert!(runtime.scheduler.run_one());
        assert!(!runtime.scheduler.run_one());
        assert!(block_on(handle).is_ok());
    }

    #[test]
    #[timeout(15000)]
    fn dropping_the_handle_cancels_and_drops_the_future() {
        let (runtime, _) = runtime();
        let dropped = Arc::new(AtomicBool::new(false));
        let handle = {
            let marker = SetOnDrop(dropped.clone());
            runtime.spawn("a task that never finishes", async move {
                let _marker = marker;
                std::future::pending::<()>().await;
            })
        };

        // Polled here, so that the cancellation finds it idle rather than merely queued.
        drive(&runtime.scheduler);
        assert!(!dropped.load(Ordering::Acquire));

        drop(handle);

        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(runtime.scheduler.live_tasks(), 0);
    }

    #[test]
    #[timeout(15000)]
    fn cancelling_notifies_the_worker() {
        let (runtime, notifications) = runtime();
        let handle = runtime.spawn("a task that never finishes", std::future::pending::<()>());

        drive(&runtime.scheduler);
        let before = *lock(&notifications);

        drop(handle);

        assert_eq!(*lock(&notifications), before + 1);
    }

    #[test]
    #[timeout(15000)]
    fn a_detached_task_runs_to_completion() {
        let (runtime, _) = runtime();
        let ran = Arc::new(AtomicBool::new(false));
        {
            let ran = ran.clone();
            runtime
                .spawn("a detached task", async move {
                    yield_now().await;
                    ran.store(true, Ordering::Release);
                })
                .detach();
        }

        drive(&runtime.scheduler);

        assert!(ran.load(Ordering::Acquire));
        assert_eq!(runtime.scheduler.live_tasks(), 0);
    }

    #[test]
    #[timeout(15000)]
    fn a_panicking_task_fails_its_handle_and_is_forgotten() {
        let (runtime, _) = runtime();
        let handle = runtime.spawn("a task that panics", async {
            panic!("the task panicked on purpose");
        });

        drive(&runtime.scheduler);

        let error = block_on(handle).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(runtime.scheduler.live_tasks(), 0);
    }

    /// A panic whose payload panics as it goes takes its task down with it all the same.
    ///
    /// The payload is the task's to dispose of, and a destructor that panics is one more panic
    /// of that task rather than one of the thread polling it. The task is finished with either
    /// way: nobody will poll it again, and whoever waits on its handle is told so.
    #[test]
    #[timeout(15000)]
    fn a_panic_payload_that_panics_as_it_goes_still_ends_its_task() {
        struct PanickingPayload;

        impl Drop for PanickingPayload {
            fn drop(&mut self) {
                panic!("the panic payload's drop panicked on purpose");
            }
        }

        let (runtime, _) = runtime();
        let handle = runtime.spawn("a task that panics with a payload of its own", async {
            std::panic::panic_any(PanickingPayload);
        });

        assert!(runtime.scheduler.run_one());

        assert_eq!(runtime.scheduler.live_tasks(), 0);
        assert_eq!(block_on(handle).unwrap_err().kind(), io::ErrorKind::Other);
        assert!(!runtime.scheduler.run_one());
    }

    /// Cancelling a task from inside its own poll, whose future's drop panics with a payload
    /// whose own drop panics too, is contained at every level: the scheduler runs the next task
    /// instead of unwinding past the one that was cancelled.
    #[test]
    #[timeout(15000)]
    fn cancelling_a_task_whose_drop_panics_with_a_panicking_payload_is_contained() {
        struct PanickingPayload;

        impl Drop for PanickingPayload {
            fn drop(&mut self) {
                panic!("the payload's drop panicked on purpose");
            }
        }

        struct PanicsWithAPayloadWhenDropped;

        impl Drop for PanicsWithAPayloadWhenDropped {
            fn drop(&mut self) {
                std::panic::panic_any(PanickingPayload);
            }
        }

        let (runtime, _) = runtime();
        let own_handle: Arc<Mutex<Option<JoinHandle<()>>>> = Arc::new(Mutex::new(None));
        let dropped_from_inside = own_handle.clone();
        let handle = runtime.spawn(
            "a task that cancels itself from within its own poll",
            async move {
                let _marker = PanicsWithAPayloadWhenDropped;
                lock(&dropped_from_inside).take();
                std::future::pending::<()>().await;
            },
        );
        *lock(&own_handle) = Some(handle);

        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let other = runtime.spawn("an unrelated task", async move {
            flag.store(true, Ordering::Release);
        });

        assert!(runtime.scheduler.run_one());
        assert!(runtime.scheduler.run_one());

        assert!(ran.load(Ordering::Acquire));
        assert!(block_on(other).is_ok());
        assert_eq!(runtime.scheduler.live_tasks(), 0);
    }

    /// A panic payload whose own drop panics with a further payload, whose drop panics too, is
    /// contained at every level: the task is failed and the next one still runs.
    #[test]
    #[timeout(15000)]
    fn a_panic_payload_whose_drop_panics_with_another_payload_is_contained() {
        struct Inner;

        impl Drop for Inner {
            fn drop(&mut self) {
                panic!("the inner payload's drop panicked on purpose");
            }
        }

        struct Outer;

        impl Drop for Outer {
            fn drop(&mut self) {
                std::panic::panic_any(Inner);
            }
        }

        let (runtime, _) = runtime();
        let handle = runtime.spawn("a task whose panic payload panics twice over", async {
            std::panic::panic_any(Outer);
        });

        assert!(runtime.scheduler.run_one());

        assert_eq!(runtime.scheduler.live_tasks(), 0);
        assert_eq!(block_on(handle).unwrap_err().kind(), io::ErrorKind::Other);

        let next = runtime.spawn("the task after it", async { 7 });
        drive(&runtime.scheduler);
        assert_eq!(block_on(next).unwrap(), 7);
    }

    #[test]
    #[timeout(15000)]
    fn a_panic_in_a_futures_drop_is_contained() {
        struct PanicOnDrop;

        impl Future for PanicOnDrop {
            type Output = ();

            fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
                Poll::Ready(())
            }
        }

        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("the future's drop panicked on purpose");
            }
        }

        let (runtime, _) = runtime();
        let handle = runtime.spawn("a task that panics as it goes", PanicOnDrop);

        assert!(runtime.scheduler.run_one());
        assert!(block_on(handle).is_ok());

        let next = runtime.spawn("the task after it", async { 7 });
        drive(&runtime.scheduler);
        assert_eq!(block_on(next).unwrap(), 7);
    }

    /// A cancellation whose future panics as it goes notifies all the same.
    ///
    /// The notification is what lets a thread waiting with this as its last task work out that it
    /// can retire, and a thread left up for the life of the process is too high a price for a
    /// destructor of somebody else's that panicked.
    #[test]
    #[timeout(15000)]
    fn a_panic_in_a_cancelled_futures_drop_still_notifies() {
        struct PanicOnDrop;

        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("the future's drop panicked on purpose");
            }
        }

        let (runtime, notifications) = runtime();
        let handle = {
            let marker = PanicOnDrop;
            runtime.spawn("a task that never finishes", async move {
                let _marker = marker;
                std::future::pending::<()>().await;
            })
        };
        // Polled here, so that the cancellation finds the future idle and the drop of it is this
        // thread's to make.
        drive(&runtime.scheduler);
        let before = *lock(&notifications);

        let cancelled = catch_unwind(AssertUnwindSafe(|| drop(handle)));

        // The panic is the caller's to see, where the handle was let go of.
        assert!(cancelled.is_err());
        assert_eq!(*lock(&notifications), before + 1);
        assert_eq!(runtime.scheduler.live_tasks(), 0);
    }

    #[test]
    #[timeout(15000)]
    fn a_ready_task_is_queued_once_however_often_it_is_woken() {
        let (runtime, notifications) = runtime();
        let handle = runtime.spawn("a task that never finishes", std::future::pending::<()>());
        let waker = Waker::from(handle.cell.clone());

        for _ in 0..3 {
            waker.wake_by_ref();
        }

        assert_eq!(*lock(&notifications), 1);
        assert!(runtime.scheduler.has_ready());
        assert!(runtime.scheduler.run_one());
        assert!(!runtime.scheduler.run_one());
    }

    #[test]
    #[timeout(15000)]
    fn a_cancelled_futures_drop_may_spawn_on_the_scheduler() {
        struct SpawnOnDrop {
            runtime: Arc<Inner>,
            ran: Arc<AtomicBool>,
        }

        impl Drop for SpawnOnDrop {
            fn drop(&mut self) {
                let ran = self.ran.clone();
                self.runtime
                    .spawn("a task spawned from a drop", async move {
                        ran.store(true, Ordering::Release);
                    })
                    .detach();
            }
        }

        let (runtime, _) = runtime();
        let ran = Arc::new(AtomicBool::new(false));
        let handle = {
            let spawner = SpawnOnDrop {
                runtime: runtime.clone(),
                ran: ran.clone(),
            };
            runtime.spawn("a task that never finishes", async move {
                let _spawner = spawner;
                std::future::pending::<()>().await;
            })
        };

        drive(&runtime.scheduler);
        drop(handle);

        assert!(!ran.load(Ordering::Acquire));
        drive(&runtime.scheduler);
        assert!(ran.load(Ordering::Acquire));
    }

    #[test]
    #[timeout(15000)]
    fn live_tasks_counts_unfinished_futures_only() {
        let (runtime, _) = runtime();
        let done = runtime.spawn("a task that finishes at once", async {});
        let pending = runtime.spawn("a task that never finishes", std::future::pending::<()>());
        assert_eq!(runtime.scheduler.live_tasks(), 2);

        drive(&runtime.scheduler);

        assert_eq!(runtime.scheduler.live_tasks(), 1);
        assert!(block_on(done).is_ok());

        drop(pending);

        assert_eq!(runtime.scheduler.live_tasks(), 0);
    }

    #[test]
    #[timeout(15000)]
    fn dropping_the_scheduler_fails_pending_joins() {
        let (runtime, _) = runtime();
        let handle = runtime.spawn("a task that never finishes", std::future::pending::<()>());

        drive(&runtime.scheduler);
        drop(runtime);

        assert!(block_on(handle).is_err());
    }

    /// Polls ready tasks until none is left, the way the thread in the runtime's seat does.
    fn drive(scheduler: &Scheduler) {
        while scheduler.run_one() {}
    }

    /// A scheduler, and the count of the notifications it has sent.
    fn runtime() -> (Arc<Inner>, Arc<Mutex<usize>>) {
        let notifications = Arc::new(Mutex::new(0));
        let counter = notifications.clone();

        (
            Inner::with_notify(move || *lock(&counter) += 1),
            notifications,
        )
    }

    /// Sets the flag it holds when it is dropped.
    struct SetOnDrop(Arc<AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
}
