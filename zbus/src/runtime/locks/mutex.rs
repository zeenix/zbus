//! A mutual-exclusion lock that a future can hold across an await point.
//!
//! A connection shares mutable state between the futures that drive it: the socket it writes
//! messages to, the set of names it has registered, the senders waiting for a reply or a signal.
//! Touching that state is not instantaneous — writing a message waits for the socket to take it —
//! so the lock has to stay held while the future waits. [`std::sync::Mutex`] cannot serve there:
//! its guard is `!Send`, so a future holding one cannot be spawned on an executor that moves work
//! between threads, and blocking the thread on a contended lock would stall every other future
//! sharing it. Waiting for this lock suspends the future instead and leaves the thread free.

use std::{
    cell::UnsafeCell,
    fmt,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

use event_listener::Event;

/// A mutual-exclusion lock whose `lock` future waits without blocking the thread.
pub(crate) struct Mutex<T: ?Sized> {
    /// Whether a guard exists.
    ///
    /// Taken with an `Acquire` compare-exchange and given back with a `Release` store, so that a
    /// holder sees what the one before it did to the value. A release between a `lock`'s check
    /// and its wait is not lost: the event fences both its registration of a listener and its
    /// notification.
    locked: AtomicBool,
    unlocked: Event,
    value: UnsafeCell<T>,
}

// SAFETY: the mutex owns the value outright, so handing the mutex to another thread hands that
// thread the value and leaves nothing behind that can reach it.
unsafe impl<T> Send for Mutex<T> where T: ?Sized + Send {}
// SAFETY: at most one guard exists at a time and the value is reachable through a guard alone, so
// sharing the mutex only ever lets threads take turns with the value, which is what `Send` allows.
unsafe impl<T> Sync for Mutex<T> where T: ?Sized + Send {}

impl<T> Mutex<T> {
    /// A new mutex holding `value`, unlocked.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            unlocked: Event::new(),
            value: UnsafeCell::new(value),
        }
    }
}

impl<T> Mutex<T>
where
    T: ?Sized,
{
    /// Acquires the lock, waiting for the current holder to release it.
    pub async fn lock(&self) -> MutexGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            // Listen before re-checking so a release between the check and the wait is seen.
            let listener = self.unlocked.listen();
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            listener.await;
        }
    }

    fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;

        Some(MutexGuard(self))
    }
}

impl<T> fmt::Debug for Mutex<T>
where
    T: ?Sized + fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("Mutex");
        match self.try_lock() {
            Some(guard) => s.field("value", &&*guard),
            None => s.field("value", &format_args!("<locked>")),
        };

        s.finish()
    }
}

#[must_use]
pub(crate) struct MutexGuard<'a, T: ?Sized>(&'a Mutex<T>);

// SAFETY: the guard is the only path to the value while it exists, so handing it to another
// thread hands that thread the value, which is what `Send` allows.
unsafe impl<T> Send for MutexGuard<'_, T> where T: ?Sized + Send {}
// SAFETY: sharing the guard only shares the `&T` it derefs to, which `Sync` allows.
unsafe impl<T> Sync for MutexGuard<'_, T> where T: ?Sized + Sync {}

impl<T> Deref for MutexGuard<'_, T>
where
    T: ?Sized,
{
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the guard exists, so `locked` is set and only this guard clears it; the cell is
        // reached through a guard alone, so no other reference to its contents can be live.
        unsafe { &*self.0.value.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T>
where
    T: ?Sized,
{
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`, this guard is the only path to the cell's contents, and `&mut
        // self` rules out a second reference taken through the guard itself.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T>
where
    T: ?Sized,
{
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
        // A notification whose listener is dropped before polling it is passed on to the next
        // listener, so a `lock` future abandoned after being woken strands nobody behind it.
        self.0.unlocked.notify(1);
    }
}
