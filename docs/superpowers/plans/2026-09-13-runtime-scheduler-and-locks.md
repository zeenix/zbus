# Private scheduler and locks — implementation plan (PR 2 of 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give zbus a private task scheduler and private async locks so that a connection can run
without async-executor or async-lock, and make `Executor`/`Task` enums over the compiled backends
and that scheduler.

**Architecture:** Two new private modules under `zbus::runtime`. `sync` holds `Mutex`, `RwLock`
and `Semaphore` over `event-listener`; `scheduler` holds a ready-queue scheduler with
cancel-on-drop join handles, no threads and no `unsafe`. Both are compiled only in builds with
neither `async-io` nor `tokio` (which cannot yet be built, see below) and under `cfg(test)`
everywhere, so their unit tests run in every configuration. `Executor` and `Task` gain a
`Scheduler` variant; the built-in backends keep async-executor and tokio unchanged.

**Tech Stack:** Rust 1.87 (MSRV), `event-listener` 5, `futures-lite` 2 (both already `comms`
dependencies), std `Mutex`/`VecDeque`/`task::Wake`.

**Spec:** `docs/superpowers/specs/2026-09-12-external-runtime-design.md`, sections "Executor and
tasks" and "Feature and dependency model"; decision 2.

## Global Constraints

- MSRV 1.87.0; no new dependencies; `Cargo.lock` must not change.
- No crate the `async-io` feature owns may be referenced outside `#[cfg(feature = "async-io")]`
  code: `async_io`, `async_executor`, `async_task`, `async_lock`, `async_process`, `blocking`.
- The `compile_error!` for `comms` without a backend (`zbus/src/lib.rs:57-66`) stays. A build
  with neither backend is made valid in PR 3, not here.
- 100 characters per line in code, comments and docs. No trailing whitespace.
- Format with `cargo +nightly fmt --all` and act on any warning it prints.
- `unsafe` only where a lock hands out references to its value, each block minimal and carrying
  a `// SAFETY:` comment. The scheduler has no `unsafe` at all.
- No `#[allow(dead_code)]`, no `#[allow(unused)]` beyond the ones already in `executor.rs`.
- Comments explain non-obvious *why*; never describe what changed.
- Doc titles have no `Get`/`Return` prefix. Test functions are not prefixed with `test_`.
- Commit prefix: curated gimoji emoji + `zb: `. Commit bodies wrap at 72 columns (CI's
  commitlint rejects body lines over 74). Commit with
  `/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign` in this checkout.
- Every commit ends with exactly these trailers, in this order, after a blank line:

  ```text
  Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
  Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
  ```

- Use `/usr/bin/git`, `/usr/bin/grep` and `~/.cargo/bin/cargo` (the bare names are shims).
- Tests that talk to a bus need a session bus: prefix `cargo test` with `dbus-run-session --`.
  The `ibus_connection` integration test fails in this environment for an unrelated,
  environmental reason (stale ibus socket path); every other failure is real.

## Branch

Work on `runtime-scheduler`, created from the rename branch (PR #1962), after a fresh fetch:

```bash
/usr/bin/git fetch origin zeenix
/usr/bin/git switch -c runtime-scheduler zeenix/runtime-module
```

## File structure

| File | Responsibility after this PR |
| --- | --- |
| `zbus/src/runtime/sync.rs` | Private `Mutex`, `RwLock`, `Semaphore` over `event-listener` |
| `zbus/src/runtime/scheduler.rs` | Private `Scheduler` and `JoinHandle` |
| `zbus/src/runtime/async_lock.rs` | Selects async-lock, `tokio::sync` or `sync` per build |
| `zbus/src/runtime/executor.rs` | `Executor`/`Task` as enums; `Executor::with_scheduler` |
| `zbus/src/runtime/mod.rs` | Module wiring |

The gate used throughout, spelled exactly like this so the three sites stay greppable:

```rust
#[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
```

---

### Task 1: Private async locks

**Files:**
- Create: `zbus/src/runtime/sync.rs`
- Modify: `zbus/src/runtime/async_lock.rs` (add the no-backend selection)
- Modify: `zbus/src/runtime/mod.rs:67` (declare the module)
- Test: unit tests inside `zbus/src/runtime/sync.rs`

**Interfaces:**
- Produces: `crate::runtime::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
  Semaphore, SemaphorePermit}` with `Mutex::new` (const), `lock()`; `RwLock::new` (const),
  `read()`, `write()`, `T: ?Sized` on the lock and both guards; `Semaphore::new` (const),
  `acquire()`. Guards implement `Deref` (`DerefMut` for the exclusive ones).

- [ ] **Step 1: Write the module with its tests**

Create `zbus/src/runtime/sync.rs`:

```rust
//! Async locks for builds without a runtime backend.
//!
//! With neither `async-io` nor `tokio` compiled in there is no lock crate to lean on, so these
//! primitives are built on `event-listener`, which every `comms` build already has. They offer
//! exactly the API the rest of the crate uses: no `try_` variants, no upgrades, no fairness
//! guarantees beyond "a release wakes a waiter".

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

#[derive(Default)]
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
        self.0.released.notify(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future::block_on;
    use std::{
        future::Future,
        pin::pin,
        sync::Arc,
        task::{Context, Poll, Waker},
        thread,
    };

    /// Polls `future` once with a no-op waker; the locks re-check their state on every poll, so
    /// this is enough to observe "would wait" versus "acquired".
    fn poll_once<F: Future>(future: &mut std::pin::Pin<&mut F>) -> Poll<F::Output> {
        future.as_mut().poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
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
        let mut first = pin!(mutex.lock());
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
        let mut writer = pin!(lock.write());
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
```

- [ ] **Step 2: Wire the module and the selector**

In `zbus/src/runtime/mod.rs`, after the line `pub(crate) mod async_lock;`, add:

```rust
#[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
pub(crate) mod sync;
```

In `zbus/src/runtime/async_lock.rs`, after the two `tokio::sync` `use` lines (line 8), add:

```rust
#[cfg(not(any(feature = "async-io", feature = "tokio")))]
pub(crate) use super::sync::{Mutex, Semaphore, SemaphorePermit};
#[cfg(all(not(any(feature = "async-io", feature = "tokio")), feature = "service"))]
pub(crate) use super::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
```

Leave the rest of the file (the async-lock/tokio `Semaphore` wrapper) untouched; it is gated on
those backends and does not apply to the no-backend build.

- [ ] **Step 3: Run the lock tests in three configurations**

```bash
cargo +nightly fmt --all
cargo test -p zbus --lib runtime::sync::
cargo test -p zbus --no-default-features --features tokio,proxy,service --lib runtime::sync::
cargo test -p zbus --all-features --lib runtime::sync::
```

Expected: 8 tests pass in each run, no warnings in the build output.

- [ ] **Step 4: Lint in the configurations CI uses**

```bash
cargo clippy -p zbus --all-targets -- -D warnings
cargo clippy -p zbus --no-default-features --all-targets --features tokio,p2p,proxy,service \
    -- -D warnings
cargo clippy --all-features --all-targets -p zbus -- -D warnings
cargo check -p zbus --no-default-features
```

Expected: all clean. `--all-targets` compiles the test module, which is what proves nothing in
`sync` is dead code in a test build (every item is used by a test).

- [ ] **Step 5: Commit**

```bash
/usr/bin/git add zbus/src/runtime/sync.rs zbus/src/runtime/async_lock.rs zbus/src/runtime/mod.rs
/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign -F - <<'EOF'
🧵 zb: Add async locks for builds without a runtime backend

A connection driven by an external reactor must not pull in async-lock,
and a build with neither `async-io` nor `tokio` has no lock crate at
all. These three primitives cover exactly the API the crate uses
(`lock`, `read`, `write`, `acquire`) on top of event-listener, which
every comms build already depends on. They are selected only in such
builds and compiled for their unit tests everywhere else; the two
backends keep their own locks for now.

The `RwLock` is writer-preferring: a waiting writer holds back new
readers, so the object server's interface registrations cannot starve
behind a stream of method calls. A cancelled `write` releases the
readers it held back.

Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
EOF
/usr/bin/git log -1 --format=%B | awk 'length > 74 {print "LONG: " $0}'
```

Expected: the `awk` prints nothing.

---

### Task 2: Private task scheduler

**Files:**
- Create: `zbus/src/runtime/scheduler.rs`
- Modify: `zbus/src/runtime/mod.rs` (declare the module next to `sync`)
- Test: unit tests inside `zbus/src/runtime/scheduler.rs`

**Interfaces:**
- Produces: `crate::runtime::scheduler::{Scheduler, JoinHandle}`. `Scheduler::new() -> Self`;
  `Scheduler::spawn(self: &Arc<Self>, fut) -> JoinHandle<T>`; `tick(&self) -> impl Future<()>`;
  `run(&self, fut) -> impl Future<Output = T>`; `is_empty(&self) -> bool`. `JoinHandle<T>` is
  `Unpin`, `Future<Output = std::io::Result<T>>`, cancels its task when dropped unless
  `detach(self)` was called.

- [ ] **Step 1: Write the module with its tests**

Create `zbus/src/runtime/scheduler.rs`:

```rust
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
    tasks: Mutex<Vec<Arc<TaskCell>>>,
    /// Wakers of the callers currently inside `run` or `tick`.
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
        let wrapped = {
            let output = output.clone();
            async move {
                // Dropping this marker, on completion or cancellation alike, wakes the joiner.
                let _finish = FinishOnDrop(output.clone());
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
    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.lock().expect("scheduler tasks poisoned").is_empty()
    }

    /// Runs one ready task, waiting for one to become ready if none is.
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
        let poll = future.as_mut().poll(&mut Context::from_waker(&waker));

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
        let mut drivers = self.drivers.lock().expect("scheduler drivers poisoned");
        let drivers = std::mem::take(&mut *drivers);
        for waker in drivers {
            waker.wake();
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        // The cells may outlive the scheduler through wakers a reactor still holds; emptying the
        // slots here is what drops the futures now rather than whenever those wakers go.
        let tasks = std::mem::take(self.tasks.get_mut().expect("scheduler tasks poisoned"));
        for cell in tasks {
            *cell.slot.lock().expect("task slot poisoned") = Slot::Done;
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
            .finish()
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
        let mut output = self.0.lock().expect("task output poisoned");
        output.finished = true;
        if let Some(joiner) = output.joiner.take() {
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
    use std::{sync::atomic::AtomicUsize, thread, time::Duration};

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
    fn spawn_and_join() {
        let scheduler = Arc::new(Scheduler::new());
        let handle = scheduler.spawn(async { 42 });
        assert_eq!(block_on(scheduler.run(handle)).unwrap(), 42);
        assert!(scheduler.is_empty());
    }

    #[test]
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
    fn dropping_the_scheduler_fails_pending_joins() {
        let scheduler = Arc::new(Scheduler::new());
        let handle = scheduler.spawn(std::future::pending::<()>());
        drop(scheduler);
        assert!(block_on(handle).is_err());
    }

    #[test]
    fn run_yields_to_the_inner_future_between_batches() {
        let scheduler = Arc::new(Scheduler::new());
        let polls = Arc::new(AtomicUsize::new(0));
        // Always ready: without yielding between batches it would starve the inner future.
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
}
```

- [ ] **Step 2: Wire the module**

In `zbus/src/runtime/mod.rs`, directly after the `sync` declaration added in Task 1, add:

```rust
#[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
pub(crate) mod scheduler;
```

- [ ] **Step 3: Run the scheduler tests in three configurations**

```bash
cargo +nightly fmt --all
cargo test -p zbus --lib runtime::scheduler::
cargo test -p zbus --no-default-features --features tokio,proxy,service --lib runtime::scheduler::
cargo test -p zbus --all-features --lib runtime::scheduler::
```

Expected: 8 tests pass in each run, no warnings.

- [ ] **Step 4: Lint**

```bash
cargo clippy -p zbus --all-targets -- -D warnings
cargo clippy -p zbus --no-default-features --all-targets --features tokio,p2p,proxy,service \
    -- -D warnings
cargo clippy --all-features --all-targets -p zbus -- -D warnings
```

Expected: all clean.

- [ ] **Step 5: Commit**

```bash
/usr/bin/git add zbus/src/runtime/scheduler.rs zbus/src/runtime/mod.rs
/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign -F - <<'EOF'
🧵 zb: Add a task scheduler for externally driven connections

A connection driven by an external reactor has nowhere to run its
socket reader, object-server dispatcher and name watchers: async-executor
is a dependency such a user must not need, and Tokio may not be compiled
in. This scheduler is the smallest thing that does the job. It keeps a
ready queue that whoever polls `run` or `tick` drains in bounded
batches, yielding between batches so the host's other futures progress,
and it has no threads of its own; the reactor's wakeups are the only
ones it gets.

Join handles cancel on drop like async-task's, and dropping the
scheduler drops every pending future, which is what an abrupt stop of a
connection driver relies on. Like the locks, it is selected only in
builds without a backend and compiled for its tests everywhere.

Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
EOF
/usr/bin/git log -1 --format=%B | awk 'length > 74 {print "LONG: " $0}'
```

Expected: the `awk` prints nothing.

---

### Task 3: `Executor` and `Task` as enums over the backends

**Files:**
- Rewrite: `zbus/src/runtime/executor.rs`
- Test: a unit test inside `zbus/src/runtime/executor.rs`, plus the existing suites

**Interfaces:**
- Consumes: `Scheduler`, `JoinHandle` from Task 2.
- Produces: unchanged public API of `Executor<'a>` and `Task<T>`; new
  `Executor::with_scheduler() -> Self` under the no-backend/test gate; `needs_internal_driver`
  unchanged; `Executor::new()` returns the scheduler variant only in a no-backend build.

- [ ] **Step 1: Rewrite the file**

Replace `zbus/src/runtime/executor.rs` with the following. Doc comments marked `/// ...` are the
existing ones; copy them verbatim from `/usr/bin/git show HEAD:zbus/src/runtime/executor.rs`.
The bodies of `tokio_spawn` and `tokio_spawn_blocking` are unchanged too.

```rust
#[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
use super::scheduler::{JoinHandle, Scheduler};
#[cfg(feature = "async-io")]
use async_executor::Executor as AsyncExecutor;
#[cfg(feature = "async-io")]
use async_task::Task as AsyncTask;
#[cfg(feature = "tokio")]
use std::io::Error;
#[cfg(any(feature = "async-io", test, not(feature = "tokio")))]
use std::sync::Arc;
use std::{
    future::Future,
    io::Result,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};
#[cfg(feature = "tokio")]
use tokio::task::JoinHandle as TokioJoinHandle;

/// ... (existing `Executor` docs)
#[derive(Debug, Clone)]
pub struct Executor<'a> {
    inner: Inner,
    // The lifetime is part of the public type; nothing inside needs it since every spawned
    // future is `'static`.
    lifetime: PhantomData<&'a ()>,
}

#[derive(Debug, Clone)]
enum Inner {
    #[cfg(feature = "async-io")]
    AsyncExecutor(Arc<AsyncExecutor<'static>>),
    // tokio spawns onto the ambient runtime; there is nothing to hold.
    #[cfg(feature = "tokio")]
    Tokio,
    #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
    Scheduler(Arc<Scheduler>),
}

impl Executor<'_> {
    /// Spawns a task onto the executor.
    #[doc(hidden)]
    pub fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
        #[allow(unused)] name: &str,
    ) -> Task<T> {
        match &self.inner {
            #[cfg(feature = "async-io")]
            Inner::AsyncExecutor(executor) => {
                Task(TaskInner::AsyncExecutor(executor.spawn(future)))
            }
            #[cfg(feature = "tokio")]
            Inner::Tokio => Task(TaskInner::Tokio(TokioTask::new(tokio_spawn(future, name)))),
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            Inner::Scheduler(scheduler) => Task(TaskInner::Scheduler(scheduler.spawn(future))),
        }
    }

    /// ... (existing `is_empty` docs)
    pub fn is_empty(&self) -> bool {
        match &self.inner {
            #[cfg(feature = "async-io")]
            Inner::AsyncExecutor(executor) => executor.is_empty(),
            #[cfg(feature = "tokio")]
            Inner::Tokio => true,
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            Inner::Scheduler(scheduler) => scheduler.is_empty(),
        }
    }

    /// ... (existing `tick` docs)
    pub async fn tick(&self) {
        match &self.inner {
            #[cfg(feature = "async-io")]
            Inner::AsyncExecutor(executor) => executor.tick().await,
            #[cfg(feature = "tokio")]
            Inner::Tokio => std::future::pending().await,
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            Inner::Scheduler(scheduler) => scheduler.tick().await,
        }
    }

    /// Create a new `Executor`.
    pub(crate) fn new() -> Self {
        #[cfg(all(feature = "async-io", feature = "tokio"))]
        {
            if super::use_tokio() {
                Self::from_inner(Inner::Tokio)
            } else {
                Self::with_async_executor()
            }
        }
        #[cfg(all(feature = "async-io", not(feature = "tokio")))]
        {
            Self::with_async_executor()
        }
        #[cfg(all(feature = "tokio", not(feature = "async-io")))]
        {
            Self::from_inner(Inner::Tokio)
        }
        #[cfg(not(any(feature = "async-io", feature = "tokio")))]
        {
            Self::with_scheduler()
        }
    }

    fn from_inner(inner: Inner) -> Self {
        Self {
            inner,
            lifetime: PhantomData,
        }
    }

    #[cfg(feature = "async-io")]
    fn with_async_executor() -> Self {
        Self::from_inner(Inner::AsyncExecutor(Arc::new(AsyncExecutor::new())))
    }

    /// An executor over zbus's own scheduler, for connections that no backend drives.
    #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
    pub(crate) fn with_scheduler() -> Self {
        Self::from_inner(Inner::Scheduler(Arc::new(Scheduler::new())))
    }

    /// Whether this executor needs an external driver thread (only the `async-io` backend does).
    #[cfg(feature = "async-io")]
    pub(crate) fn needs_internal_driver(&self) -> bool {
        matches!(self.inner, Inner::AsyncExecutor(_))
    }

    /// ... (existing `run` docs)
    pub(crate) async fn run<T>(&self, future: impl Future<Output = T>) -> T {
        match &self.inner {
            #[cfg(feature = "async-io")]
            Inner::AsyncExecutor(executor) => executor.run(future).await,
            #[cfg(feature = "tokio")]
            Inner::Tokio => future.await,
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            Inner::Scheduler(scheduler) => scheduler.run(future).await,
        }
    }
}

#[cfg(feature = "tokio")]
fn tokio_spawn<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
    #[allow(unused)] name: &str,
) -> TokioJoinHandle<T> {
    // ... (existing body, unchanged)
}

/// ... (existing `Task` docs)
#[doc(hidden)]
#[derive(Debug)]
pub struct Task<T>(TaskInner<T>);

#[derive(Debug)]
enum TaskInner<T> {
    #[cfg(feature = "async-io")]
    AsyncExecutor(AsyncTask<T>),
    #[cfg(feature = "tokio")]
    Tokio(TokioTask<T>),
    #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
    Scheduler(JoinHandle<T>),
}

impl<T> Task<T> {
    /// Detaches the task to let it keep running in the background.
    pub fn detach(self) {
        match self.0 {
            #[cfg(feature = "async-io")]
            TaskInner::AsyncExecutor(task) => task.detach(),
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(task) => task.detach(),
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            TaskInner::Scheduler(handle) => handle.detach(),
        }
    }
}

impl<T> Task<T>
where
    T: Send + 'static,
{
    /// ... (existing `spawn_blocking` docs)
    #[allow(unused)]
    pub(crate) fn spawn_blocking<F>(f: F, #[allow(unused)] name: &str) -> Self
    where
        F: FnOnce() -> T + Send + 'static,
    {
        super::select_runtime! {
            tokio: Self(TaskInner::Tokio(TokioTask::new(tokio_spawn_blocking(f, name)))),
            async_io: Self(TaskInner::AsyncExecutor(blocking::unblock(f))),
        }
    }
}

#[cfg(feature = "tokio")]
fn tokio_spawn_blocking<F, T>(f: F, #[allow(unused)] name: &str) -> TokioJoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // ... (existing body, unchanged)
}

impl<T> Future for Task<T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.get_mut().0 {
            #[cfg(feature = "async-io")]
            TaskInner::AsyncExecutor(task) => Pin::new(task).poll(cx).map(Ok),
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(task) => Pin::new(task).poll(cx),
            #[cfg(any(test, not(any(feature = "async-io", feature = "tokio"))))]
            TaskInner::Scheduler(handle) => Pin::new(handle).poll(cx),
        }
    }
}

/// A tokio task handle that aborts the task when dropped, matching `async_task::Task`.
#[cfg(feature = "tokio")]
#[derive(Debug)]
struct TokioTask<T>(Option<TokioJoinHandle<T>>);

#[cfg(feature = "tokio")]
impl<T> TokioTask<T> {
    fn new(handle: TokioJoinHandle<T>) -> Self {
        Self(Some(handle))
    }

    fn detach(mut self) {
        // Dropping a tokio `JoinHandle` detaches it.
        drop(self.0.take());
    }
}

#[cfg(feature = "tokio")]
impl<T> Drop for TokioTask<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

#[cfg(feature = "tokio")]
impl<T> Future for TokioTask<T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let handle = self
            .get_mut()
            .0
            .as_mut()
            .expect("a `TokioTask` is only polled before it is detached or dropped");
        Pin::new(handle).poll(cx).map(|r| match r {
            Ok(v) => Ok(v),
            Err(e) => {
                if e.is_cancelled() {
                    Err(Error::other("tokio::task cancelled"))
                } else {
                    panic!("tokio::task::JoinHandle error: {e}")
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Executor;
    use futures_lite::future::block_on;

    #[test]
    fn scheduler_backed_executor_runs_its_tasks() {
        let executor = Executor::with_scheduler();
        let task = executor.spawn(async { 7 }, "seven");
        assert!(!executor.is_empty());
        assert_eq!(block_on(executor.run(task)).unwrap(), 7);
        assert!(executor.is_empty());

        executor.spawn(async {}, "detached").detach();
        block_on(executor.run(executor.tick()));
        assert!(executor.is_empty());
    }
}
```

Two things to get right while transcribing:

- The `use std::sync::Arc;` gate must cover every configuration that names `Arc`: `async-io`
  (the async-executor variant), `test` and no-backend (the scheduler variant). The line above
  spells that out; if `cargo check` in some configuration reports `Arc` unused or missing, fix
  the gate rather than adding an `allow`.
- `Inner` deliberately has no lifetime parameter: the async-executor variant uses
  `AsyncExecutor<'static>` (every future zbus spawns is `'static`), and the public `Executor<'a>`
  keeps its lifetime only through the `PhantomData` field. Do not reintroduce `Inner<'a>`; in a
  tokio-only or no-backend build the parameter would be unused and fail to compile.

- [ ] **Step 2: Check every configuration compiles and is warning-free**

```bash
cargo +nightly fmt --all
cargo check -p zbus
cargo check -p zbus --no-default-features --features tokio,proxy,service
cargo check -p zbus --all-features
cargo check -p zbus --no-default-features
cargo clippy -p zbus --all-targets -- -D warnings
cargo clippy -p zbus --no-default-features --all-targets --features tokio,p2p,proxy,service \
    -- -D warnings
cargo clippy --all-features --all-targets -p zbus -- -D warnings
cargo check -p zbus --target x86_64-pc-windows-gnu
cargo check -p zbus --target x86_64-apple-darwin
```

Expected: all clean; `/usr/bin/git status --short Cargo.lock` prints nothing.

- [ ] **Step 3: Run the suites for each backend and the new test**

```bash
cargo test -p zbus --lib runtime::
dbus-run-session -- cargo test -p zbus
dbus-run-session -- cargo test -p zbus --no-default-features --features tokio,proxy,service --tests
dbus-run-session -- cargo test -p zbus --no-default-features --features tokio,service --doc \
    connection::Connection::executor
dbus-run-session -- cargo test -p zbus --all-features -- --skip fdpass_systemd
```

Expected: PASS (only `ibus_connection` may fail, environmentally). The tokio-only doctest is the
one that drives a connection through `internal_executor(false)` and proves the tokio variant
still detaches/aborts as before; the all-features run includes `unix_p2p_async_io_backend`,
which pins `needs_internal_driver`.

- [ ] **Step 4: Commit**

```bash
/usr/bin/git add zbus/src/runtime/executor.rs
/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign -F - <<'EOF'
♻️ zb: Make Executor and Task enums over the task backends

The two wrappers used to be structs of optional backend fields, which
only worked while there were two backends and one of them was always
present. With zbus's own scheduler as a third way to run tasks that
shape no longer fits: an enum with one variant per compiled backend
says which one a connection uses and lets the compiler check that every
operation handles it.

Behaviour is unchanged for the async-io and tokio backends; the
scheduler variant is selected only in builds without either, which the
crate still refuses to compile until the reactor that would make such a
build useful exists.

Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
EOF
/usr/bin/git log -1 --format=%B | awk 'length > 74 {print "LONG: " $0}'
```

Expected: the `awk` prints nothing.

---

### Task 4: Final verification and PR

**Files:** none new.

- [ ] **Step 1: Rebase on a fresh rename branch and re-run the CI-equivalent matrix**

```bash
/usr/bin/git fetch origin zeenix
/usr/bin/git rebase zeenix/runtime-module
cargo +nightly fmt --all -- --check
cargo check -p zbus --no-default-features
cargo check -p zbus --no-default-features --target x86_64-pc-windows-gnu
cargo check -p zbus --no-default-features --target x86_64-apple-darwin
cargo check -p zbus --target x86_64-pc-windows-gnu
cargo check -p zbus --target x86_64-apple-darwin
cargo clippy -p zbus --no-default-features --all-targets --features async-io,blocking-api,p2p \
    -- -D warnings
cargo clippy -p zbus --no-default-features --all-targets --features tokio,p2p,proxy \
    -- -D warnings
cargo clippy -p zbus --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p zbus --all-features --no-deps
dbus-run-session -- cargo test -p zbus --all-features -- --skip fdpass_systemd
dbus-run-session -- cargo test -p zbus --no-default-features --features tokio,proxy,service --tests
/usr/bin/git status --short
for c in $(/usr/bin/git rev-list zeenix/runtime-module..HEAD); do
    /usr/bin/git log -1 --format=%B $c | awk 'length > 74 {print "LONG: " $0}'
done
```

Expected: everything green, `Cargo.lock` unchanged, working tree clean, no `LONG:` line.

- [ ] **Step 2: Review the three commits**

```bash
/usr/bin/git log --oneline zeenix/runtime-module..HEAD
/usr/bin/git diff --stat zeenix/runtime-module..HEAD
/usr/bin/grep -rn "async_lock\|async_executor\|async_task\|async_io\|blocking::" \
    zbus/src/runtime/sync.rs zbus/src/runtime/scheduler.rs
```

Expected: three commits in the order above; only `zbus/src/runtime/` touched; the grep prints
nothing.

- [ ] **Step 3: Push and open the PR**

```bash
/usr/bin/git push zeenix runtime-scheduler
/usr/bin/gh pr create --repo z-galaxy/zbus --base main --head zeenix:runtime-scheduler \
    --title "🧵 zb: Add a private task scheduler and async locks" \
    --body-file - <<'EOF'
Second of four PRs for #1960 (design: #1961). Based on #1962; only the last three commits are
new.

- `runtime::sync`: `Mutex`, writer-preferring `RwLock` and `Semaphore` over `event-listener`,
  with exactly the API the crate uses.
- `runtime::scheduler`: a ready-queue task scheduler with cancel-on-drop join handles, bounded
  batches and no threads of its own.
- `Executor`/`Task` become enums over async-executor, tokio and the scheduler.

Both modules are selected only in a build with neither `async-io` nor `tokio`, which stays a
compile error until the reactor lands in the next PR; they are compiled for their unit tests in
every configuration. The built-in backends are unchanged. No crate the `async-io` feature owns is
referenced by either module.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
EOF
```
