//! A runtime for a connection that runs on one thread and starts none of its own.
//!
//! Readiness comes from a `polling::Poller`, tasks from `async-task`, and timers from a map of
//! deadlines the host walks itself. [`Host::run`] drives all of it, together with the caller's
//! own future, on the thread that calls it: nothing here starts a thread, so a thread the process
//! gains while a connection is alive is a thread the connection asked for.
//!
//! [`traits::Runtime::spawn_blocking`] keeps its default, which runs each call on a std thread
//! that exits with the work. Nothing on the path these tests take reaches it, but a connection
//! on Linux or Android does have one caller: [`zbus::Connection::peer_creds`] looks the peer's
//! supplementary groups up through it, and the default would then start one short-lived thread
//! per call. A host with a pool of its own should override the hook rather than copy this.

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    future::Future,
    io,
    pin::{Pin, pin},
    sync::{Arc, Mutex as SyncMutex, MutexGuard, PoisonError, Weak},
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};

use async_task::Runnable;
use polling::{Event, Events, Poller};
use zbus::runtime::{Interest, IoSource, traits};

/// A host: a poller, a queue of runnables and a map of timers, driven by one thread.
///
/// Releasing the host cancels every task it still holds, whether the task is waiting in the
/// queue as a runnable, parked in the timer or source maps as a waker, or detached and left in
/// the host's keeping.
pub struct Host(Arc<Inner>);

impl Host {
    /// A host with a poller of its own.
    pub fn new() -> io::Result<Self> {
        let inner = Inner {
            poller: Poller::new()?,
            queue: SyncMutex::new(VecDeque::new()),
            detached: SyncMutex::default(),
            timers: SyncMutex::default(),
            sources: SyncMutex::default(),
        };

        Ok(Self(Arc::new(inner)))
    }

    /// The runtime to hand to [`zbus::connection::Builder::runtime`].
    pub fn handle(&self) -> Handle {
        Handle(Arc::clone(&self.0))
    }

    /// Runs `future` on this thread, along with every task, timer and socket of this host.
    ///
    /// Each turn runs what the queue holds, polls `future`, and then sleeps in the poller until
    /// a source is ready, a timer is due or a wakeup arrives.
    pub fn run<F>(&self, future: F) -> F::Output
    where
        F: Future,
    {
        let inner = &self.0;
        let waker = Waker::from(Arc::new(Notify(Arc::clone(inner))));
        let mut cx = Context::from_waker(&waker);
        let mut future = pin!(future);
        let mut events = Events::new();

        loop {
            loop {
                // The runnable is taken before it runs: running it may queue another one.
                let next = lock(&inner.queue).pop_front();
                match next {
                    Some(runnable) => {
                        runnable.run();
                    }
                    None => break,
                }
            }

            // A detached task is the host's to hold until it ends, and only the host can notice
            // that it has: letting the finished ones go here keeps that hold to what is running.
            //
            // The finished ones leave the list under the lock and are released after it, because
            // a task that has ended carries what it produced and that value's destructor is free
            // to reach back in: a `Handle` handed on to it can spawn and detach a task of its
            // own, and `detach` wants this very lock.
            let finished = {
                let mut detached = lock(&inner.detached);
                let (finished, running): (Vec<_>, Vec<_>) = std::mem::take(&mut *detached)
                    .into_iter()
                    .partition(|task| task.is_finished());
                *detached = running;

                finished
            };
            drop(finished);

            if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                return output;
            }

            let timeout = self.timeout();
            events.clear();
            inner
                .poller
                .wait(&mut events, timeout)
                .expect("the poller waits for readiness");

            for event in events.iter() {
                self.wake_source(event.key);
            }
            self.wake_due_timers();
        }
    }

    /// How many timers are still waiting for their deadline.
    pub fn pending_timers(&self) -> usize {
        lock(&self.0.timers).pending.len()
    }

    /// A watch on the state this host shares with the handles it hands out.
    ///
    /// The watch itself keeps nothing alive, so it can be taken before the host is released and
    /// asked afterwards whether anything outlived it.
    pub fn probe(&self) -> Probe {
        Probe(Arc::downgrade(&self.0))
    }

    /// How long the poller may sleep: until the earliest deadline, and not at all while there
    /// are runnables waiting to run.
    fn timeout(&self) -> Option<Duration> {
        if !lock(&self.0.queue).is_empty() {
            return Some(Duration::ZERO);
        }

        let timers = lock(&self.0.timers);
        let (deadline, _) = timers.pending.keys().next()?;

        Some(deadline.saturating_duration_since(Instant::now()))
    }

    /// Wakes whoever waits on the source `key` names.
    ///
    /// Both directions are woken for either event: the poller watches every source in one-shot
    /// mode, so one event leaves the whole registration disarmed, and a waiter has to run again
    /// to ask for the readiness it still wants.
    fn wake_source(&self, key: usize) {
        let Some(state) = lock(&self.0.sources).states.get(&key).cloned() else {
            return;
        };
        let (readable, writable) = {
            let mut wakers = lock(&state.wakers);

            (wakers.readable.take(), wakers.writable.take())
        };

        for waker in [readable, writable].into_iter().flatten() {
            waker.wake();
        }
    }

    /// Wakes and forgets every timer whose deadline has passed.
    fn wake_due_timers(&self) {
        let due = {
            let mut timers = lock(&self.0.timers);
            // Ids are handed out from zero upwards, so no timer carries `u64::MAX` and the split
            // leaves behind exactly the deadlines that have passed.
            let later = timers.pending.split_off(&(Instant::now(), u64::MAX));

            std::mem::replace(&mut timers.pending, later)
        };

        for waker in due.into_values() {
            waker.wake();
        }
    }
}

/// Releases the tasks the host still holds, which nothing else can.
///
/// A task's future owns the [`Registration`]s and [`Sleep`]s it is waiting on, and each of those
/// holds the shared state; the shared state holds the task right back, as a runnable in the queue,
/// as a waker in the timer or source maps, or as a detached task the host keeps. That ring keeps
/// itself alive, and the host is the only party that knows there is no longer anyone to run it,
/// so it cuts the ring here.
///
/// One pass is not enough: dropping a runnable drops its future, which drops the handles on any
/// task that future spawned, and cancelling a task hands its runnable back to the queue so that
/// its own future is dropped by whoever owns the queue. Each pass therefore takes what is there,
/// drops it, and looks again, until a pass finds nothing left.
///
/// Nothing taken is dropped while a lock is held, because those drops reach back in: a runnable's
/// drop can call the schedule closure, and a future's drop can delete a source.
impl Drop for Host {
    fn drop(&mut self) {
        loop {
            let queued = std::mem::take(&mut *lock(&self.0.queue));
            let detached = std::mem::take(&mut *lock(&self.0.detached));
            let timers = std::mem::take(&mut lock(&self.0.timers).pending);
            let states: Vec<_> = lock(&self.0.sources).states.values().cloned().collect();
            let wakers: Vec<_> = states
                .iter()
                .flat_map(|state| {
                    let mut wakers = lock(&state.wakers);

                    [wakers.readable.take(), wakers.writable.take()]
                })
                .flatten()
                .collect();

            if queued.is_empty() && detached.is_empty() && timers.is_empty() && wakers.is_empty() {
                break;
            }

            drop(wakers);
            drop(timers);
            drop(detached);
            drop(queued);
        }
    }
}

/// A watch on a [`Host`]'s shared state, handed out by [`Host::probe`].
pub struct Probe(Weak<Inner>);

impl Probe {
    /// Whether the state the host shared with its handles is gone.
    ///
    /// False while anything at all still holds it: a handle, a registration, a timer, or a task
    /// that was never dropped.
    pub fn is_released(&self) -> bool {
        self.0.strong_count() == 0
    }
}

/// What a [`Host`] hands to a connection: a shared handle on its poller, queue and timers.
#[derive(Clone)]
pub struct Handle(Arc<Inner>);

impl traits::Runtime for Handle {
    type RegisteredIoSource = Registration;
    type Sleep = Sleep;
    type Task<T>
        = Task<T>
    where
        T: Send + 'static;

    fn register_io_source(&self, source: IoSource) -> io::Result<Registration> {
        let mut sources = lock(&self.0.sources);
        let key = sources.next_key;
        let state = Arc::new(SourceState {
            source,
            wakers: SyncMutex::default(),
            key,
        });

        // SAFETY: the registration deletes the source from the poller when it is dropped, and it
        // holds the state below, which owns a clone of the `IoSource`. The descriptor therefore
        // stays open until after that delete has run.
        unsafe { self.0.poller.add(&state.source, Event::none(key)) }?;

        sources.next_key += 1;
        sources.states.insert(key, Arc::clone(&state));

        Ok(Registration {
            inner: Arc::clone(&self.0),
            state,
        })
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        Sleep {
            inner: Arc::clone(&self.0),
            // This host's timers run on the standard clock, so a length of time is a deadline
            // on it; a runtime with a clock of its own would measure `duration` on that instead.
            deadline: Instant::now() + duration,
            id: None,
        }
    }

    fn spawn<T>(&self, _name: &str, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        // This host has nowhere to record a task's name, so the name goes no further than here.
        // A queued runnable owns this closure, so the hold on the host has to be weak: an
        // `Arc` here would keep a released host's queue, timers and sources alive for good.
        let inner = Arc::downgrade(&self.0);
        let schedule = move |runnable: Runnable| {
            let Some(inner) = inner.upgrade() else {
                // The host is gone, and dropping the runnable cancels the task.
                return;
            };
            lock(&inner.queue).push_back(runnable);
            // The host may be asleep in the poller: bring it back for the new runnable.
            let _ = inner.poller.notify();
        };

        let (runnable, task) = async_task::spawn(future, schedule);
        runnable.schedule();

        Task {
            task,
            inner: Arc::downgrade(&self.0),
        }
    }
}

/// One source the host's poller watches.
pub struct Registration {
    inner: Arc<Inner>,
    state: Arc<SourceState>,
}

impl traits::PollIo for Registration {
    fn poll_io<T>(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        mut operation: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        match operation() {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            result => return Poll::Ready(result),
        }

        // Asking the poller while the wakers are held keeps the two in step: what the poller
        // watches for this source is exactly what someone is waiting for.
        let mut wakers = lock(&self.state.wakers);
        match interest {
            Interest::Readable => wakers.readable = Some(cx.waker().clone()),
            Interest::Writable => wakers.writable = Some(cx.waker().clone()),
            // `Interest` is open, and this host watches for the two readiness kinds it knows.
            _ => return Poll::Ready(Err(io::ErrorKind::Unsupported.into())),
        }
        let event = Event::new(
            self.state.key,
            wakers.readable.is_some(),
            wakers.writable.is_some(),
        );
        // One-shot interest is level-triggered, so a source that went ready between the
        // operation above and this call is reported as soon as the host waits again.
        if let Err(e) = self.inner.poller.modify(&self.state.source, event) {
            return Poll::Ready(Err(e));
        }

        Poll::Pending
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        lock(&self.inner.sources).states.remove(&self.state.key);
        let _ = self.inner.poller.delete(&self.state.source);
    }
}

/// A timer the host wakes once its deadline has passed.
pub struct Sleep {
    inner: Arc<Inner>,
    deadline: Instant,
    // Handed out on the first poll, which is when the timer joins the host's map.
    id: Option<u64>,
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if Instant::now() >= this.deadline {
            return Poll::Ready(());
        }

        let mut timers = lock(&this.inner.timers);
        let id = match this.id {
            Some(id) => id,
            None => {
                let id = timers.next_id;
                timers.next_id += 1;
                this.id = Some(id);

                id
            }
        };
        timers
            .pending
            .insert((this.deadline, id), cx.waker().clone());

        Poll::Pending
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            lock(&self.inner.timers)
                .pending
                .remove(&(self.deadline, id));
        }
    }
}

/// A spawned task: dropping the handle cancels it.
pub struct Task<T> {
    task: async_task::Task<T>,
    // Only to reach the host when the task is detached, so it must not keep the host alive.
    inner: Weak<Inner>,
}

impl<T> Future for Task<T> {
    type Output = io::Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.task).poll(cx).map(Ok)
    }
}

impl<T> traits::TaskHandle<T> for Task<T>
where
    T: Send + 'static,
{
    /// Hands the task to the host, which holds it until it ends or the host is released.
    ///
    /// `async_task::Task::detach` is not what a detached task wants here: it would leave the task
    /// owned by nothing, and one that is parked on something that never fires would then be
    /// unreachable for good, along with the connection state its future holds. A runtime owns the
    /// tasks spawned on it and ends them when it ends, and this host is no different.
    fn detach(self) {
        let Some(inner) = self.inner.upgrade() else {
            // The host is gone, and dropping the task is all that cancelling it takes.
            return;
        };

        lock(&inner.detached).push(Box::new(self.task));
    }
}

/// Everything a [`Host`] shares with the handles it hands out.
struct Inner {
    poller: Poller,
    queue: SyncMutex<VecDeque<Runnable>>,
    detached: SyncMutex<Vec<Box<dyn Detached>>>,
    timers: SyncMutex<Timers>,
    sources: SyncMutex<Sources>,
}

/// A detached task the host holds on to, whatever that task produces.
///
/// Tasks of every output type wait in one list, and the only thing the host asks of one is
/// whether it has ended, so that is all this trait exposes.
trait Detached: Send {
    fn is_finished(&self) -> bool;
}

impl<T> Detached for async_task::Task<T>
where
    T: Send,
{
    fn is_finished(&self) -> bool {
        async_task::Task::is_finished(self)
    }
}

/// The waker the host polls the caller's future with.
struct Notify(Arc<Inner>);

impl Wake for Notify {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.0.poller.notify();
    }
}

/// The timers waiting for their deadline, keyed so that two of the same deadline stay apart.
#[derive(Default)]
struct Timers {
    pending: BTreeMap<(Instant, u64), Waker>,
    next_id: u64,
}

/// The sources the poller watches, under the keys it reports them by.
#[derive(Default)]
struct Sources {
    states: HashMap<usize, Arc<SourceState>>,
    next_key: usize,
}

/// One watched source: the descriptor, and who to wake for each direction of it.
struct SourceState {
    source: IoSource,
    wakers: SyncMutex<Wakers>,
    key: usize,
}

/// Who waits for each direction of one source.
#[derive(Default)]
struct Wakers {
    readable: Option<Waker>,
    writable: Option<Waker>,
}

/// The value behind a lock, taken whether or not a panic poisoned it.
fn lock<T>(mutex: &SyncMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
