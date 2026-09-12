//! Async locks for builds without a runtime backend.
//!
//! With neither `async-io` nor `tokio` compiled in there is no lock crate to lean on, so these
//! primitives are built on `event-listener`, which every `comms` build already has. They offer
//! exactly the API the rest of the crate uses: no `try_` variants, no upgrades, no fairness
//! guarantees beyond "a release wakes a waiter". The `RwLock` is write-preferring, so, as with
//! `async-lock` and `tokio`, a task already holding a read guard must not call `read()` again:
//! a writer registering in between would deadlock it.

use event_listener::Event;
use std::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::{
        Mutex as SyncMutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

/// A mutual-exclusion lock whose `lock` future waits without blocking the thread.
pub(crate) struct Mutex<T: ?Sized> {
    locked: AtomicBool,
    unlocked: Event,
    value: UnsafeCell<T>,
}

// SAFETY: the lock hands out at most one guard at a time, so a `T: Send` only ever moves between
// the threads that take turns holding that guard; sharing the mutex grants no other access.
unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            unlocked: Event::new(),
            value: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> Mutex<T> {
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
            .is_ok()
            .then(|| MutexGuard(self))
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: ?Sized + std::fmt::Debug> std::fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

// SAFETY: a guard is the unique access path to the value while it exists, so it is `Send` when
// the value can move between threads and `Sync` when the value can be shared.
unsafe impl<T: ?Sized + Send> Send for MutexGuard<'_, T> {}
unsafe impl<T: ?Sized + Sync> Sync for MutexGuard<'_, T> {}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `locked` is set and only this guard clears it, so nothing else touches the
        // value.
        unsafe { &*self.0.value.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`, plus `&mut self` rules out another reference through this
        // guard.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.0.locked.store(false, Ordering::Release);
        // event-listener hands a notification on to the next listener if the notified one is
        // dropped before it is polled, so a cancelled `lock` cannot strand the waiter after it.
        self.0.unlocked.notify(1);
    }
}

/// A readers-writer lock. A waiting writer blocks new readers so that a stream of readers
/// cannot starve it.
pub(crate) struct RwLock<T: ?Sized> {
    state: SyncMutex<RwState>,
    readers_may_enter: Event,
    writer_may_enter: Event,
    value: UnsafeCell<T>,
}

struct RwState {
    readers: usize,
    writer: bool,
    writers_waiting: usize,
}

// SAFETY: readers only get shared references and a writer gets the only reference, which is the
// same discipline as `std::sync::RwLock`; hence the same bounds.
unsafe impl<T: ?Sized + Send> Send for RwLock<T> {}
unsafe impl<T: ?Sized + Send + Sync> Sync for RwLock<T> {}

impl<T> RwLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            state: SyncMutex::new(RwState {
                readers: 0,
                writer: false,
                writers_waiting: 0,
            }),
            readers_may_enter: Event::new(),
            writer_may_enter: Event::new(),
            value: UnsafeCell::new(value),
        }
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Acquires shared access, waiting while a writer holds or waits for the lock.
    pub async fn read(&self) -> RwLockReadGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_read() {
                return guard;
            }
            let listener = self.readers_may_enter.listen();
            if let Some(guard) = self.try_read() {
                return guard;
            }
            listener.await;
        }
    }

    /// Acquires exclusive access, waiting for every reader and writer to leave.
    pub async fn write(&self) -> RwLockWriteGuard<'_, T> {
        let waiting = WaitingWriter::register(self);
        loop {
            if let Some(guard) = self.try_write() {
                waiting.granted();
                return guard;
            }
            let listener = self.writer_may_enter.listen();
            if let Some(guard) = self.try_write() {
                waiting.granted();
                return guard;
            }
            listener.await;
        }
    }

    fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        let mut state = self.state.lock().expect("RwLock state poisoned");
        if state.writer || state.writers_waiting > 0 {
            return None;
        }
        state.readers += 1;
        Some(RwLockReadGuard(self))
    }

    fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        let mut state = self.state.lock().expect("RwLock state poisoned");
        if state.writer || state.readers > 0 {
            return None;
        }
        state.writer = true;
        Some(RwLockWriteGuard(self))
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

// No `T: Debug` bound: the crate stores `RwLock<dyn Interface>`, and the `dyn` type is not
// `Debug`, so this impl must serve unsized, non-`Debug` values too and therefore prints no
// fields.
impl<T: ?Sized> std::fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RwLock").finish_non_exhaustive()
    }
}

/// Counts a `write` call as waiting for as long as its future lives, so that readers are held
/// back only while a writer really is waiting: a cancelled `write` lets them in again.
struct WaitingWriter<'a, T: ?Sized> {
    lock: &'a RwLock<T>,
    counted: bool,
}

impl<'a, T: ?Sized> WaitingWriter<'a, T> {
    fn register(lock: &'a RwLock<T>) -> Self {
        lock.state
            .lock()
            .expect("RwLock state poisoned")
            .writers_waiting += 1;
        Self {
            lock,
            counted: true,
        }
    }

    fn granted(mut self) {
        self.counted = false;
        let mut state = self.lock.state.lock().expect("RwLock state poisoned");
        state.writers_waiting -= 1;
    }
}

impl<T: ?Sized> Drop for WaitingWriter<'_, T> {
    fn drop(&mut self) {
        if !self.counted {
            return;
        }
        let mut state = self.lock.state.lock().expect("RwLock state poisoned");
        state.writers_waiting -= 1;
        if state.writers_waiting == 0 && !state.writer {
            drop(state);
            self.lock.readers_may_enter.notify(usize::MAX);
        }
    }
}

#[must_use]
pub(crate) struct RwLockReadGuard<'a, T: ?Sized>(&'a RwLock<T>);

// SAFETY: a read guard only ever yields `&T`, so it is `Send`/`Sync` exactly when `&T` is.
unsafe impl<T: ?Sized + Sync> Send for RwLockReadGuard<'_, T> {}
unsafe impl<T: ?Sized + Sync> Sync for RwLockReadGuard<'_, T> {}

impl<T: ?Sized> Deref for RwLockReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `readers > 0` and no writer holds the lock while this guard exists.
        unsafe { &*self.0.value.get() }
    }
}

impl<T: ?Sized> Drop for RwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("RwLock state poisoned");
        state.readers -= 1;
        if state.readers == 0 {
            drop(state);
            self.0.writer_may_enter.notify(1);
        }
    }
}

#[must_use]
pub(crate) struct RwLockWriteGuard<'a, T: ?Sized>(&'a RwLock<T>);

// SAFETY: a write guard is the unique access path to the value while it exists; same bounds as
// `MutexGuard`.
unsafe impl<T: ?Sized + Send> Send for RwLockWriteGuard<'_, T> {}
unsafe impl<T: ?Sized + Sync> Sync for RwLockWriteGuard<'_, T> {}

impl<T: ?Sized> Deref for RwLockWriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: `writer` is set and nobody else holds the lock while this guard exists.
        unsafe { &*self.0.value.get() }
    }
}

impl<T: ?Sized> DerefMut for RwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`, plus `&mut self` rules out another reference through this
        // guard.
        unsafe { &mut *self.0.value.get() }
    }
}

impl<T: ?Sized> Drop for RwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("RwLock state poisoned");
        state.writer = false;
        let writers_waiting = state.writers_waiting;
        drop(state);
        if writers_waiting > 0 {
            self.0.writer_may_enter.notify(1);
        } else {
            self.0.readers_may_enter.notify(usize::MAX);
        }
    }
}

/// A counting semaphore.
pub(crate) struct Semaphore {
    permits: AtomicUsize,
    released: Event,
}

impl Semaphore {
    pub const fn new(permits: usize) -> Self {
        Self {
            permits: AtomicUsize::new(permits),
            released: Event::new(),
        }
    }

    /// Takes one permit, waiting for one to be released if none is free.
    pub async fn acquire(&self) -> SemaphorePermit<'_> {
        loop {
            if let Some(permit) = self.try_acquire() {
                return permit;
            }
            let listener = self.released.listen();
            if let Some(permit) = self.try_acquire() {
                return permit;
            }
            listener.await;
        }
    }

    fn try_acquire(&self) -> Option<SemaphorePermit<'_>> {
        self.permits
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |permits| {
                permits.checked_sub(1)
            })
            .is_ok()
            .then(|| SemaphorePermit(self))
    }
}

impl std::fmt::Debug for Semaphore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Semaphore")
            .field("permits", &self.permits.load(Ordering::Relaxed))
            .finish()
    }
}

#[must_use]
pub(crate) struct SemaphorePermit<'a>(&'a Semaphore);

impl Drop for SemaphorePermit<'_> {
    fn drop(&mut self) {
        self.0.permits.fetch_add(1, Ordering::AcqRel);
        // `notify_additional`, not `notify`: each released permit is one more waiter that can
        // proceed, and `notify` would coalesce with a still-pending notification from another
        // permit released concurrently, stranding a waiter that has a permit available to it.
        self.0.released.notify_additional(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future::block_on;
    use ntest::timeout;
    use std::{
        future::Future,
        ops::DerefMut,
        pin::pin,
        sync::Arc,
        task::{Context, Poll, Waker},
        thread,
    };

    /// Polls `future` once with a no-op waker; the locks re-check their state on every poll, so
    /// this is enough to observe "would wait" versus "acquired". Generic over the pointer so it
    /// takes a `Box::pin`-ed future as readily as a `pin!`-ed one: a test that needs to cancel a
    /// future early must own it through a `Box`, since dropping a `pin!`-ed binding only ends
    /// that local borrow and leaves the future itself alive (and registered) until the enclosing
    /// scope exits.
    fn poll_once<F: Future, P: DerefMut<Target = F>>(
        future: &mut std::pin::Pin<P>,
    ) -> Poll<F::Output> {
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    #[timeout(15000)]
    fn mutex_excludes_concurrent_holders() {
        let counter = Arc::new(Mutex::new(0u32));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let counter = counter.clone();
                thread::spawn(move || {
                    for _ in 0..1000 {
                        block_on(async {
                            let mut value = counter.lock().await;
                            let seen = *value;
                            thread::yield_now();
                            *value = seen + 1;
                        });
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(*block_on(counter.lock()), 8000);
    }

    #[test]
    fn mutex_wakes_a_waiter_on_release() {
        let mutex = Mutex::new(());
        let held = block_on(mutex.lock());
        let mut waiter = pin!(mutex.lock());
        assert!(poll_once(&mut waiter).is_pending());
        drop(held);
        assert!(poll_once(&mut waiter).is_ready());
    }

    #[test]
    fn cancelled_lock_does_not_strand_the_next_waiter() {
        let mutex = Mutex::new(());
        let held = block_on(mutex.lock());
        // `Box::pin` so the `drop` below actually cancels the future, rather than just ending
        // the borrow of a `pin!`-ed one while its storage lives on until the test returns.
        let mut first = Box::pin(mutex.lock());
        let mut second = pin!(mutex.lock());
        assert!(poll_once(&mut first).is_pending());
        assert!(poll_once(&mut second).is_pending());
        // The release notifies `first`, which is gone; the notification must reach `second`.
        drop(first);
        drop(held);
        assert!(poll_once(&mut second).is_ready());
    }

    #[test]
    fn rwlock_shares_between_readers() {
        let lock = RwLock::new(1u8);
        let first = block_on(lock.read());
        let second = block_on(lock.read());
        assert_eq!(*first + *second, 2);
    }

    #[test]
    fn rwlock_waiting_writer_blocks_new_readers() {
        let lock = RwLock::new(0u8);
        let reader = block_on(lock.read());
        let mut writer = pin!(lock.write());
        assert!(poll_once(&mut writer).is_pending());
        let mut late_reader = pin!(lock.read());
        assert!(poll_once(&mut late_reader).is_pending());
        drop(reader);
        let Poll::Ready(mut guard) = poll_once(&mut writer) else {
            panic!("the writer must get the lock once the last reader leaves");
        };
        *guard = 7;
        assert!(poll_once(&mut late_reader).is_pending());
        drop(guard);
        let Poll::Ready(value) = poll_once(&mut late_reader) else {
            panic!("readers must get in once the writer leaves");
        };
        assert_eq!(*value, 7);
    }

    #[test]
    fn cancelled_writer_lets_readers_in_again() {
        let lock = RwLock::new(());
        let reader = block_on(lock.read());
        // `Box::pin` so the `drop` below actually cancels the future; see the comment on the
        // equivalent `Mutex` test.
        let mut writer = Box::pin(lock.write());
        assert!(poll_once(&mut writer).is_pending());
        let mut late_reader = pin!(lock.read());
        assert!(poll_once(&mut late_reader).is_pending());
        drop(writer);
        assert!(poll_once(&mut late_reader).is_ready());
        drop(reader);
    }

    #[test]
    fn rwlock_coerces_to_an_unsized_value() {
        trait Named {
            fn name(&self) -> &'static str;
        }
        struct Thing;
        impl Named for Thing {
            fn name(&self) -> &'static str {
                "thing"
            }
        }
        let lock: Arc<RwLock<dyn Named + Send + Sync>> = Arc::new(RwLock::new(Thing));
        assert_eq!(block_on(lock.read()).name(), "thing");
        assert_eq!(block_on(lock.write()).name(), "thing");
    }

    #[test]
    fn semaphore_admits_at_most_its_permits() {
        static SEMAPHORE: Semaphore = Semaphore::new(2);
        let first = block_on(SEMAPHORE.acquire());
        let second = block_on(SEMAPHORE.acquire());
        let mut third = pin!(SEMAPHORE.acquire());
        assert!(poll_once(&mut third).is_pending());
        drop(first);
        assert!(poll_once(&mut third).is_ready());
        drop(second);
    }
}
