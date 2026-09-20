//! The runtime zbus brings along, for a connection that is handed no runtime of its own.
//!
//! It is made of a scheduler, which holds the tasks and hands them out to be polled, a reactor,
//! which watches the sockets and keeps the timers, and a seat, which one thread at a time is
//! in to run the two. Everything [`traits::Runtime`] asks of a runtime reaches one of them: a
//! spawned future is queued on the scheduler, a registered source is the reactor's and so is a
//! timer, from the first poll of it onwards, and blocking work goes to a thread of its own, as
//! the trait's default has it.
//!
//! A thread that runs `block_on` has a runtime of its own, shared by every connection it builds
//! from inside such a call and alive for as long as any of them or a thread running it is: one
//! channel for breaking a wait from another thread — a pipe on unix and a socket pair on Windows,
//! two descriptors either way — and no thread until there is work with nobody to run it. Two
//! threads that each call `block_on` drive their own connections, in parallel; a connection used
//! from a thread other than the one that built it is driven by the latter, and each wake of the
//! former's future crosses between the two. A connection built on a thread that is inside no
//! `block_on` and in no seat — one that some other executor polls — has no thread to look to, and
//! goes on a runtime the whole process shares, which a helper thread runs.
//!
//! The thread in a runtime's seat is the one inside [`block_on`](crate::block_on), where a program
//! has one: it runs the tasks and waits on the reactor between two polls of its own future. Where
//! none has — the connection is polled from some other executor, or work is left once
//! `block_on` has returned — a helper thread takes the seat, and leaves in the round it finds
//! nothing left to run, watch or time. A `block_on` that arrives while the helper is in the seat
//! is given it, the helper parking until that call leaves, so that a program calling `block_on`
//! once per operation runs each of them on its own thread.
//!
//! A panic in a task is caught and fails that task's handle. A panic outside a task — in a
//! waker, say — unwinds the thread in the seat: out of `block_on`, to whoever called it, with
//! the seat freed on the way; out of the helper's loop, the helper putting itself down as gone so
//! that the next spawn, registration or timer poll starts another. A reactor wait takes the ready
//! I/O wakers out of its maps and wakes them, then does the same with the due timers, so a panic
//! partway through waking one of those batches drops the rest of that batch, still unwoken, in
//! the unwind: gone from the maps already, they are not seen again, and the waiters they belonged
//! to stay pending.
//!
//! Nothing here takes a runtime down while it has work: a detached task that never finishes
//! keeps a helper, and everything that task's future holds, for the life of the process.

mod driver;
mod poll;
mod reactor;
mod scheduler;

use std::{
    fmt,
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
    task::{Context, Poll},
    time::Duration,
};

pub(crate) use reactor::RegisteredIoSource;

use reactor::Reactor;
use scheduler::{JoinHandle, Scheduler};

use super::{IoSource, traits};

/// The runtime zbus brings along: a scheduler, a reactor, and a seat for whoever runs them.
#[derive(Clone)]
pub(crate) struct Builtin {
    inner: Arc<Inner>,
}

impl Builtin {
    /// A handle on the runtime for what this thread builds, brought into being here if none is
    /// alive.
    ///
    /// What can fail is the reactor: it opens the channel a wait is broken through.
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            inner: Inner::current()?,
        })
    }

    /// A handle on `inner`, whatever registry it is or is not in.
    #[cfg(test)]
    pub(super) fn from_inner(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    /// What this handle is on.
    #[cfg(test)]
    pub(super) fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }

    /// Whether the helper thread is running.
    #[cfg(test)]
    pub(super) fn helper_running(&self) -> bool {
        lock(&self.inner.seat).helper_running()
    }

    /// Whether the helper thread is parked for want of the seat.
    #[cfg(test)]
    pub(super) fn helper_parked(&self) -> bool {
        lock(&self.inner.seat).helper_parked()
    }
}

impl fmt::Debug for Builtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builtin")
            .field("helper", &lock(&self.inner.seat).helper_running())
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
        let registered = self.inner.register(source)?;
        // Asked for once the source is in the reactor's map, so that a helper starting here
        // takes it into its very first wait.
        self.inner.ensure_progress();

        Ok(registered)
    }

    fn sleep(&self, duration: Duration) -> Sleep {
        Sleep(self.inner.sleep(duration))
    }

    fn spawn<T>(&self, name: &str, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        let task = Task(self.inner.spawn(name, future));
        // Asked for once the task is on the scheduler's queue, so that a helper starting here
        // finds it there.
        self.inner.ensure_progress();

        task
    }
}

/// Runs `future` to completion on the calling thread, running that thread's runtime alongside
/// it: see [`driver::block_on`].
#[cfg(not(feature = "tokio"))]
pub(crate) fn block_on<F>(future: F) -> F::Output
where
    F: Future,
{
    driver::block_on(
        &|| OWN.try_with(|own| lock(own).upgrade()).ok().flatten(),
        future,
    )
}

/// A timer on a built-in runtime, which keeps a thread on it for as long as it has a deadline.
///
/// The reactor takes a timer's deadline on the first poll of it rather than where it is made, and
/// the thread that is to fire it has to be there from that poll onwards, however long ago the
/// timer was asked for. So it is the poll that asks for one, and a timer nobody ever polls costs
/// nothing at all.
pub(crate) struct Sleep(reactor::Sleep);

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        if Pin::new(&mut this.0).poll(cx).is_ready() {
            return Poll::Ready(());
        }
        // A timer that never comes due leaves no deadline behind and needs no thread: one
        // started for it would find nothing to wait on and retire in the round it started.
        if this.0.never_fires() {
            return Poll::Pending;
        }
        // Asked for once the deadline is in the reactor's map, so that a helper starting here
        // waits on it.
        this.0.runtime().ensure_progress();

        Poll::Pending
    }
}

/// A task spawned on a built-in runtime, which cancels that task when dropped.
pub(crate) struct Task<T>(JoinHandle<T>);

impl<T> fmt::Debug for Task<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Task").field(&self.0).finish()
    }
}

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

/// What a runtime is made of, shared by every handle on it and by the thread that runs it.
pub(super) struct Inner {
    scheduler: Scheduler,
    reactor: Reactor,
    /// Who runs this runtime; the lock the hand-over decisions are made under.
    seat: Mutex<driver::Seat>,
}

impl Inner {
    /// The runtime for what the calling thread builds, made here if none is alive: the one it is
    /// in the seat of, where it is in one; its own, where it is inside a `block_on` and so has a
    /// thread to run what it builds; and the process's otherwise.
    ///
    /// Once this thread's `OWN` local is gone, there is no registry left to hold what it builds:
    /// inside a `block_on`, that goes on a runtime in no registry, which a helper thread runs;
    /// outside one, it still goes on the process's shared registry as before.
    fn current() -> io::Result<Arc<Self>> {
        if let Some(inner) = driver::driven() {
            return Ok(inner);
        }
        if !driver::in_block_on() {
            return Self::shared_in(&SHARED);
        }
        match OWN.try_with(Self::shared_in) {
            Ok(inner) => inner,
            Err(_) => Self::new(),
        }
    }

    /// Whether `inner` is the calling thread's own runtime, the one its `block_on` drives.
    ///
    /// Nothing is a thread's own once its locals are gone, so work handed over then gets a
    /// helper, as work handed to any runtime the caller does not drive does.
    pub(super) fn is_own(inner: &Arc<Self>) -> bool {
        OWN.try_with(|own| std::ptr::eq(lock(own).as_ptr(), Arc::as_ptr(inner)))
            .unwrap_or(false)
    }

    /// The runtime `registry` names, made here if none is alive.
    pub(super) fn shared_in(registry: &Mutex<Weak<Self>>) -> io::Result<Arc<Self>> {
        let mut shared = lock(registry);
        if let Some(inner) = shared.upgrade() {
            return Ok(inner);
        }
        let inner = Self::new()?;
        *shared = Arc::downgrade(&inner);

        Ok(inner)
    }

    /// A runtime with a scheduler and a reactor of its own, in no registry.
    pub(super) fn new() -> io::Result<Arc<Self>> {
        let reactor = Reactor::new()?;
        // The hook is made before the runtime it belongs to, so it can only hold the runtime
        // weakly; a wake that finds it gone has no wait left to break.
        Ok(Arc::new_cyclic(|runtime: &Weak<Self>| {
            let runtime = runtime.clone();
            Self::assemble(reactor, move || {
                if let Some(inner) = runtime.upgrade() {
                    inner.reactor.notify();
                }
            })
        }))
    }

    /// A runtime whose scheduler calls `notify` in place of the reactor, for tests of the
    /// scheduler alone.
    #[cfg(test)]
    pub(super) fn with_notify(notify: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self::assemble(Reactor::new().unwrap(), notify))
    }

    fn assemble(reactor: Reactor, notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            scheduler: Scheduler::new(notify),
            reactor,
            seat: Mutex::new(driver::Seat::new()),
        }
    }

    /// Whether anything is left to run, watch or time.
    pub(super) fn is_busy(&self) -> bool {
        self.scheduler.has_ready() || self.scheduler.live_tasks() > 0 || !self.reactor.is_idle()
    }

    /// Sees to it that the work just handed over is run: a helper is started unless a thread is
    /// in the seat or about to take it. Called after that work is in place, never before.
    ///
    /// Whoever runs the work holds a runtime of its own while it does, so what has been handed
    /// over runs to completion even once every [`Builtin`] clone is gone.
    fn ensure_progress(self: &Arc<Self>) {
        driver::ensure_helper(self);
    }
}

/// Makes `inner` the calling thread's own runtime until the value handed back is dropped, which
/// puts back whatever was there before, whether the call returns or unwinds.
///
/// For a test that drives a runtime of its own making: the runtime a `block_on` resolves to is
/// always the one its thread builds on, so a spawn from inside such a call asks for no helper,
/// and a test that drives a runtime nobody's registry names would be told otherwise.
#[cfg(test)]
pub(super) fn own_for_the_call(inner: &Arc<Inner>) -> impl Drop {
    struct Restore(Weak<Inner>);

    impl Drop for Restore {
        fn drop(&mut self) {
            OWN.with(|own| *lock(own) = std::mem::take(&mut self.0));
        }
    }

    OWN.with(|own| Restore(std::mem::replace(&mut *lock(own), Arc::downgrade(inner))))
}

thread_local! {
    /// This thread's runtime, if one is alive: the one a `block_on` on this thread drives, and
    /// the one a connection built inside such a call goes on.
    ///
    /// A `Weak`, so that the runtime and the two descriptors its reactor holds go once the last
    /// handle and any thread running it are gone, and the next handle brings a fresh one. A
    /// thread that ends with connections alive leaves them to the helper thread that took them
    /// over when its last `block_on` returned.
    static OWN: Mutex<Weak<Inner>> = const { Mutex::new(Weak::new()) };
}

/// The runtime for connections built on a thread that is inside no `block_on` and in no seat:
/// ones some other executor polls, wherever in the process they are built.
///
/// Such a connection has no thread of its own to look to, so a helper runs it, and one runtime
/// for all of them is one helper and one pair of descriptors rather than a set per thread. A
/// `Weak`, for the same reason [`OWN`] is.
static SHARED: Mutex<Weak<Inner>> = Mutex::new(Weak::new());

/// The value behind a lock, taken whether or not a panic poisoned it.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
