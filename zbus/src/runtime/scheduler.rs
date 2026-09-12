//! The task scheduler for connections that no runtime backend drives.
//!
//! A connection built with an explicit reactor cannot spawn onto async-executor or Tokio: the
//! first is a dependency an external-runtime user must not need, the second may not be compiled
//! in at all. This is the smallest thing that runs such a connection's tasks: a ready queue that
//! whoever polls [`Scheduler::run`] or [`Scheduler::tick`] drains in bounded batches. It has no
//! threads of its own; the host's event loop provides every wakeup.

use std::{
    collections::VecDeque,
    future::{Future, poll_fn},
    io,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// How many ready tasks `run` polls before it lets the inner future and the host's other futures
/// have a turn.
const BATCH: usize = 16;

#[derive(Default)]
pub(crate) struct Scheduler {
    ready: Mutex<VecDeque<Arc<TaskCell>>>,
    /// Every spawned task that has neither finished nor been cancelled. Owning the cells here is
    /// what lets dropping the scheduler drop every pending future.
    ///
    /// `forget` scans this linearly, which is fine for the handful of tasks a single connection
    /// runs; a keyed map would be needed if that count ever grows large.
    tasks: Mutex<Vec<Arc<TaskCell>>>,
    /// Wakers of the callers currently inside `run` or `tick`.
    ///
    /// Bounded in practice even though wakers here are not necessarily `will_wake`-stable:
    /// every `wake_drivers` call empties the vector, and only `run`'s self re-arm after a full
    /// batch re-registers without an intervening wake, so it stays small.
    drivers: Mutex<Vec<Waker>>,
}

impl Scheduler {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Queues `future` and returns the handle that joins or cancels it.
    pub(crate) fn spawn<T>(
        self: &Arc<Self>,
        future: impl Future<Output = T> + Send + 'static,
    ) -> JoinHandle<T>
    where
        T: Send + 'static,
    {
        let output = Arc::new(Mutex::new(Output::default()));
        // Created outside the `async move` block so that a task dropped before its first poll
        // (an async block only runs its body once polled) still drops this and wakes the joiner.
        let finish = FinishOnDrop(output.clone());
        let wrapped = {
            let output = output.clone();
            async move {
                // Dropping this marker, on completion or cancellation alike, wakes the joiner.
                let _finish = finish;
                let value = future.await;
                output.lock().expect("task output poisoned").value = Some(value);
            }
        };
        let cell = Arc::new(TaskCell {
            slot: Mutex::new(Slot::Idle(Box::pin(wrapped))),
            scheduled: AtomicBool::new(false),
            rerun: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            scheduler: Arc::downgrade(self),
        });
        self.tasks
            .lock()
            .expect("scheduler tasks poisoned")
            .push(cell.clone());
        cell.wake_by_ref();
        JoinHandle {
            cell,
            output,
            detached: false,
        }
    }

    /// Whether no spawned task is left.
    ///
    /// This has no wakeup source of its own, so a caller must not simply wait on it without a
    /// notification of some kind; the `drained` test helper re-polls itself for that reason.
    pub(crate) fn is_empty(&self) -> bool {
        self.tasks
            .lock()
            .expect("scheduler tasks poisoned")
            .is_empty()
    }

    /// Polls the next queued task, waiting for one to be queued if none is.
    pub(crate) async fn tick(&self) {
        poll_fn(|cx| {
            self.register_driver(cx.waker());
            if self.run_one() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }

    /// Drives the queued tasks until `future` completes.
    ///
    /// Tasks are polled in batches of [`BATCH`]; between batches the caller's waker is invoked
    /// and `Pending` returned, so whoever polls this (a host event loop, another executor) gets
    /// to run its own work in between.
    ///
    /// A panicking task propagates its panic out of `run`/`tick` and is forgotten, exactly as
    /// if it had been cancelled.
    pub(crate) async fn run<T>(&self, future: impl Future<Output = T>) -> T {
        let mut future = pin!(future);
        poll_fn(|cx| {
            self.register_driver(cx.waker());
            if let Poll::Ready(value) = future.as_mut().poll(cx) {
                return Poll::Ready(value);
            }
            for _ in 0..BATCH {
                if !self.run_one() {
                    return Poll::Pending;
                }
            }
            cx.waker().wake_by_ref();
            Poll::Pending
        })
        .await
    }

    /// Polls the next ready task once. Returns `false` when nothing was ready.
    fn run_one(&self) -> bool {
        let Some(cell) = self
            .ready
            .lock()
            .expect("scheduler ready queue poisoned")
            .pop_front()
        else {
            return false;
        };
        // Cleared before polling so that a wake arriving during the poll re-queues the task.
        cell.scheduled.store(false, Ordering::Release);

        let mut future = {
            let mut slot = cell.slot.lock().expect("task slot poisoned");
            match std::mem::replace(&mut *slot, Slot::Running) {
                Slot::Idle(future) => future,
                Slot::Running => {
                    // Another driver is polling it; ask that driver to re-queue it afterwards.
                    cell.rerun.store(true, Ordering::Release);
                    return true;
                }
                Slot::Done => {
                    *slot = Slot::Done;
                    return true;
                }
            }
        };

        let waker = Waker::from(cell.clone());
        let guard = PanicGuard {
            scheduler: self,
            cell: &cell,
        };
        let poll = future.as_mut().poll(&mut Context::from_waker(&waker));
        drop(guard);

        let finished = {
            let mut slot = cell.slot.lock().expect("task slot poisoned");
            if poll.is_ready() || cell.cancelled.load(Ordering::Acquire) {
                *slot = Slot::Done;
                true
            } else {
                *slot = Slot::Idle(future);
                false
            }
        };
        if finished {
            self.forget(&cell);
        }
        if cell.rerun.swap(false, Ordering::AcqRel) {
            cell.wake_by_ref();
        }
        true
    }

    fn forget(&self, cell: &Arc<TaskCell>) {
        self.tasks
            .lock()
            .expect("scheduler tasks poisoned")
            .retain(|task| !Arc::ptr_eq(task, cell));
    }

    fn register_driver(&self, waker: &Waker) {
        let mut drivers = self.drivers.lock().expect("scheduler drivers poisoned");
        if !drivers.iter().any(|known| known.will_wake(waker)) {
            drivers.push(waker.clone());
        }
    }

    fn wake_drivers(&self) {
        let drivers = {
            let mut drivers = self.drivers.lock().expect("scheduler drivers poisoned");
            std::mem::take(&mut *drivers)
        };
        for waker in drivers {
            waker.wake();
        }
    }
}

/// Cleans up after a task whose poll panicked, so the panic leaves neither a cell that is
/// `Running` forever nor an entry in `tasks` that pins `is_empty` to `false`.
struct PanicGuard<'a> {
    scheduler: &'a Scheduler,
    cell: &'a Arc<TaskCell>,
}

impl Drop for PanicGuard<'_> {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            return;
        }
        // The slot lock is never held across the poll, so it cannot be poisoned by the panic;
        // tolerate poisoning anyway rather than panic inside a panic, which would abort.
        let mut slot = self
            .cell
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Slot::Done;
        drop(slot);
        self.scheduler.forget(self.cell);
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // The cells may outlive the scheduler through wakers a reactor still holds; emptying the
        // slots here is what drops the futures now rather than whenever those wakers go.
        let tasks = std::mem::take(self.tasks.get_mut().expect("scheduler tasks poisoned"));
        for cell in tasks {
            let future = {
                let mut slot = cell.slot.lock().expect("task slot poisoned");
                std::mem::replace(&mut *slot, Slot::Done)
            };
            drop(future);
        }
    }
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field(
                "tasks",
                &self.tasks.lock().expect("scheduler tasks poisoned").len(),
            )
            .finish_non_exhaustive()
    }
}

struct TaskCell {
    slot: Mutex<Slot>,
    /// Set while the cell sits in the ready queue; suppresses duplicate entries.
    scheduled: AtomicBool,
    /// Set by a driver that found the task being polled by another driver.
    rerun: AtomicBool,
    cancelled: AtomicBool,
    scheduler: Weak<Scheduler>,
}

enum Slot {
    Idle(BoxFuture),
    Running,
    Done,
}

impl Wake for TaskCell {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self.scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(scheduler) = self.scheduler.upgrade() else {
            return;
        };
        scheduler
            .ready
            .lock()
            .expect("scheduler ready queue poisoned")
            .push_back(self.clone());
        scheduler.wake_drivers();
    }
}

/// Joins a spawned task; dropping it cancels the task unless [`JoinHandle::detach`] was called.
pub(crate) struct JoinHandle<T> {
    cell: Arc<TaskCell>,
    output: Arc<Mutex<Output<T>>>,
    detached: bool,
}

struct Output<T> {
    value: Option<T>,
    finished: bool,
    joiner: Option<Waker>,
}

impl<T> Default for Output<T> {
    fn default() -> Self {
        Self {
            value: None,
            finished: false,
            joiner: None,
        }
    }
}

struct FinishOnDrop<T>(Arc<Mutex<Output<T>>>);

impl<T> Drop for FinishOnDrop<T> {
    fn drop(&mut self) {
        let joiner = {
            let mut output = self.0.lock().expect("task output poisoned");
            output.finished = true;
            output.joiner.take()
        };
        if let Some(joiner) = joiner {
            joiner.wake();
        }
    }
}

impl<T> JoinHandle<T> {
    /// Lets the task run to completion on its own.
    pub(crate) fn detach(mut self) {
        self.detached = true;
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if self.detached {
            return;
        }
        self.cell.cancelled.store(true, Ordering::Release);
        let idle = {
            let mut slot = self.cell.slot.lock().expect("task slot poisoned");
            match std::mem::replace(&mut *slot, Slot::Done) {
                Slot::Idle(future) => Some(future),
                // A running task is dropped by its driver once the poll returns.
                Slot::Running => {
                    *slot = Slot::Running;
                    None
                }
                Slot::Done => None,
            }
        };
        if let Some(future) = idle {
            drop(future);
            if let Some(scheduler) = self.cell.scheduler.upgrade() {
                scheduler.forget(&self.cell);
            }
        }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = io::Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut output = self.output.lock().expect("task output poisoned");
        if let Some(value) = output.value.take() {
            return Poll::Ready(Ok(value));
        }
        if output.finished {
            return Poll::Ready(Err(io::Error::other("task cancelled")));
        }
        output.joiner = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl<T> std::fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinHandle")
            .field("detached", &self.detached)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use event_listener::Event;
    use futures_lite::future::{block_on, yield_now};
    use ntest::timeout;
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::atomic::AtomicUsize,
        thread,
        time::Duration,
    };

    /// Resolves once `scheduler` has no tasks left, re-polling on every wake.
    async fn drained(scheduler: &Scheduler) {
        poll_fn(|cx| {
            if scheduler.is_empty() {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await
    }

    #[test]
    #[timeout(15000)]
    fn spawn_and_join() {
        let scheduler = Arc::new(Scheduler::new());
        let handle = scheduler.spawn(async { 42 });
        assert_eq!(block_on(scheduler.run(handle)).unwrap(), 42);
        assert!(scheduler.is_empty());
    }

    #[test]
    #[timeout(5000)]
    fn wake_from_another_thread_reaches_the_driver() {
        let scheduler = Arc::new(Scheduler::new());
        let event = Arc::new(Event::new());
        let handle = {
            let event = event.clone();
            scheduler.spawn(async move {
                let listener = event.listen();
                listener.await;
                "woken"
            })
        };
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            event.notify(1);
        });
        assert_eq!(block_on(scheduler.run(handle)).unwrap(), "woken");
    }

    #[test]
    #[timeout(15000)]
    fn task_woken_during_its_own_poll_runs_again() {
        struct WakeOnce(bool);
        impl Future for WakeOnce {
            type Output = ();
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                if self.0 {
                    Poll::Ready(())
                } else {
                    self.0 = true;
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
        }
        let scheduler = Arc::new(Scheduler::new());
        let handle = scheduler.spawn(WakeOnce(false));
        block_on(scheduler.run(handle)).unwrap();
    }

    #[test]
    #[timeout(15000)]
    fn dropping_the_handle_cancels_and_drops_the_future() {
        struct SetOnDrop(Arc<AtomicBool>);
        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let scheduler = Arc::new(Scheduler::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let handle = {
            let marker = SetOnDrop(dropped.clone());
            scheduler.spawn(async move {
                let _marker = marker;
                std::future::pending::<()>().await;
            })
        };
        // Let the task register its (never-firing) wakeup so it is idle, not merely queued.
        block_on(scheduler.run(yield_now()));
        drop(handle);
        assert!(dropped.load(Ordering::Acquire));
        assert!(scheduler.is_empty());
    }

    #[test]
    #[timeout(15000)]
    fn detached_task_runs_to_completion() {
        let scheduler = Arc::new(Scheduler::new());
        let ran = Arc::new(AtomicBool::new(false));
        {
            let ran = ran.clone();
            scheduler
                .spawn(async move {
                    yield_now().await;
                    ran.store(true, Ordering::Release);
                })
                .detach();
        }
        block_on(scheduler.run(drained(&scheduler)));
        assert!(ran.load(Ordering::Acquire));
    }

    #[test]
    #[timeout(15000)]
    fn dropping_the_scheduler_fails_pending_joins() {
        let scheduler = Arc::new(Scheduler::new());
        let handle = scheduler.spawn(std::future::pending::<()>());
        drop(scheduler);
        assert!(block_on(handle).is_err());
    }

    #[test]
    #[timeout(15000)]
    fn run_yields_to_the_inner_future_between_batches() {
        let scheduler = Arc::new(Scheduler::new());
        let polls = Arc::new(AtomicUsize::new(0));
        // Proves the batch bound keeps an always-ready task from monopolising `run`: this task
        // is always ready and reschedules itself, yet the inner future still gets polled.
        scheduler
            .spawn(async {
                loop {
                    yield_now().await;
                }
            })
            .detach();
        let inner = {
            let polls = polls.clone();
            poll_fn(move |cx| {
                if polls.fetch_add(1, Ordering::AcqRel) >= 3 {
                    Poll::Ready(())
                } else {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            })
        };
        block_on(scheduler.run(inner));
        assert!(polls.load(Ordering::Acquire) >= 4);
    }

    #[test]
    #[timeout(5000)]
    fn run_re_arms_itself_when_a_batch_leaves_tasks_ready() {
        // More immediately-ready tasks than one batch: after the first batch, nothing but
        // `run`'s own re-arm can get the rest polled, `last` (which we wait on) included.
        let scheduler = Arc::new(Scheduler::new());
        let total = BATCH + 4;
        let mut handles = Vec::new();
        for _ in 0..total {
            handles.push(scheduler.spawn(async {}));
        }
        let last = handles.pop().expect("at least one task was spawned");
        block_on(scheduler.run(last)).unwrap();
        assert!(scheduler.is_empty());
    }

    #[test]
    #[timeout(15000)]
    fn tick_runs_one_task() {
        let scheduler = Arc::new(Scheduler::new());
        let count = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let count = count.clone();
            scheduler
                .spawn(async move {
                    count.fetch_add(1, Ordering::AcqRel);
                })
                .detach();
        }
        block_on(scheduler.tick());
        assert_eq!(count.load(Ordering::Acquire), 1);
        block_on(scheduler.tick());
        assert_eq!(count.load(Ordering::Acquire), 2);
        assert!(scheduler.is_empty());
    }

    #[test]
    #[timeout(15000)]
    fn panicking_task_propagates_and_is_forgotten() {
        let scheduler = Arc::new(Scheduler::new());
        let handle = scheduler.spawn(async { panic!("task panicked on purpose") });
        let outcome = catch_unwind(AssertUnwindSafe(|| block_on(scheduler.tick())));
        assert!(
            outcome.is_err(),
            "the panic must propagate out of the driver"
        );
        assert!(
            scheduler.is_empty(),
            "a panicked task must not stay registered"
        );
        assert!(
            block_on(handle).is_err(),
            "joining a panicked task reports cancellation"
        );
    }
}
