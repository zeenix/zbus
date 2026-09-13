# Runtime abstraction — implementation plan (PR 2 of 4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every connection takes its tasks, async locks and timers from one runtime value, which
is either a compiled-in backend or an implementation of the new public `runtime::traits::Runtime`
trait supplied through `Builder::runtime`; a build with neither `async-io` nor `tokio` becomes
valid.

**Architecture:** A public trait family (`Runtime`, `IoRegistration`, `Task`, `Mutex`, `RwLock`)
describes what a runtime supplies. Inside zbus a private `Runtime` enum has one zero-cost variant
per compiled backend and an `External` variant holding a type-erased trait object; the crate's
task handle and lock wrappers are enums with the same shape. The async-io backend becomes the
`AsyncIo` type, one implementor of the trait that the default path uses through its own variant.
`internal_executor`, `Connection::executor`, and the public `Executor`/`Task` types go away.

**Tech Stack:** Rust 1.87 (MSRV; return-position `impl Trait` in traits, GATs), async-io /
async-executor / async-task / async-lock behind `async-io`, tokio behind `tokio`, `event-listener`
and `futures-lite` as shared dependencies.

**Spec:** `docs/superpowers/specs/2026-09-12-external-runtime-design.md`: "Public API",
"Runtime selection", "Internals", "Feature and dependency model", "Testing strategy" (PR 2).

## Global Constraints

- No new normal dependencies; `Cargo.lock` must not change. Dev-dependencies may add
  `async-io`, `async-executor` and `async-lock` (already in the lock) for the test runtime.
- No crate the `async-io` feature owns is referenced outside `#[cfg(feature = "async-io")]` code:
  `async_io`, `async_executor`, `async_task`, `async_lock`, `async_process`, `blocking`.
- No handwritten scheduler or synchronization primitive. Everything zbus needs comes from a
  backend or from the trait.
- No `unsafe`. No `#[allow(...)]` beyond the pre-existing `#[allow(unused)]` on the `name`
  parameters of spawning functions.
- The automatic default behaviour is unchanged: `Connection::session()` and friends on the
  default features still start one `zbus::Connection executor` thread per connection and run on
  async-io; with `tokio` and a current Tokio runtime they run on Tokio with no thread.
- 6.0 is unreleased: remove replaced API outright, no deprecations.
- 100 characters per line in code, comments and docs. No trailing whitespace. Comments explain
  non-obvious *why*. Doc titles have no `Get`/`Return` prefix. Tests are not prefixed `test_`.
- Format with `cargo +nightly fmt --all` and act on any warning it prints.
- Commit prefix: curated gimoji emoji + `zb: ` (or `book: `). Bodies wrap at 72 columns (CI's
  commitlint rejects lines over 74). Commit with
  `/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign`. Every commit ends
  with exactly these trailers after a blank line:

  ```text
  Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
  Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
  ```

- Use `/usr/bin/git`, `/usr/bin/grep`, `~/.cargo/bin/cargo` (the bare names are shims). Tests
  that talk to a bus need `dbus-run-session --`. `ibus_connection` in `zbus/tests/basic.rs` fails
  in this environment for an unrelated reason (stale ibus socket path); every other failure is
  real. Tests that can hang carry `#[ntest::timeout(15000)]`.
- The external-only feature set used throughout this plan is
  `EXT="blocking-api,proxy,service,object-manager,unixexec,ibus,tracing,p2p"` with
  `--no-default-features` (every default feature except `async-io`, plus `p2p` for the channel
  tests).

## Branch

```bash
/usr/bin/git fetch zeenix
/usr/bin/git switch -c runtime-abstraction zeenix/runtime-module
```

## File structure

| File | Responsibility after this PR |
| --- | --- |
| `zbus/src/runtime/traits.rs` | Public `Runtime`, `IoRegistration`, `Task`, `Mutex`, `RwLock` |
| `zbus/src/runtime/io_source.rs` | Public `IoSource` and `Interest` |
| `zbus/src/runtime/async_io.rs` | `AsyncIo`: the built-in implementor, owns executor and thread |
| `zbus/src/runtime/erased.rs` | Object-safe mirrors of the traits and the blanket impl |
| `zbus/src/runtime/locks.rs` | Crate-private `Mutex`/`RwLock` enums and guards |
| `zbus/src/runtime/executor.rs` | Crate-private `Task` enum (the `Executor` type is gone) |
| `zbus/src/runtime/timeout.rs` | `Runtime::timeout` |
| `zbus/src/runtime/test_runtime.rs` | `cfg(test)` implementor over dev-dependencies |
| `zbus/src/runtime/mod.rs` | The private `Runtime` enum, selection, spawning, lock constructors |
| `zbus/src/connection/{mod,builder,socket_reader}.rs` | Use the runtime; `Builder::runtime` |
| `zbus/src/object_server/{mod,interface/*}.rs` | Locks from the runtime; `Box<dyn Interface>` |
| `zbus/src/proxy/mod.rs` | Property cache task spawned on the runtime |
| `zbus/src/{lib,utils}.rs` | Re-exports; no-backend `block_on` |
| `.github/workflows/rust.yml` | External-only leg |
| `book/src/upgrading-to-6.md` | Migration entry |

---

### Task 1: The runtime traits and `IoSource`

**Files:**
- Create: `zbus/src/runtime/traits.rs`, `zbus/src/runtime/io_source.rs`
- Modify: `zbus/src/runtime/mod.rs` (declare and re-export)

**Interfaces:**
- Produces: `zbus::runtime::traits::{Runtime, IoRegistration, Task, Mutex, RwLock}`,
  `zbus::runtime::{IoSource, Interest}`. Nothing consumes them yet; they are `pub`, so no
  dead-code warnings.

- [ ] **Step 1: `io_source.rs`**

```rust
//! The handle a runtime registers for readiness.

#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
#[cfg(windows)]
use std::os::windows::io::{AsRawSocket, AsSocket, BorrowedSocket, OwnedSocket, RawSocket};
use std::sync::Arc;

/// A socket or pipe that zbus owns and a runtime watches for readiness.
///
/// It is a shared owner: zbus keeps one clone for the I/O it performs itself and the runtime's
/// registration keeps whatever it needs. The descriptor stays open as long as either lives.
#[derive(Clone, Debug)]
pub struct IoSource(Arc<Owned>);

#[cfg(unix)]
type Owned = OwnedFd;
#[cfg(windows)]
type Owned = OwnedSocket;

impl IoSource {
    pub(crate) fn new(owned: Owned) -> Self {
        Self(Arc::new(owned))
    }
}

#[cfg(unix)]
impl AsFd for IoSource {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

#[cfg(unix)]
impl AsRawFd for IoSource {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(windows)]
impl AsSocket for IoSource {
    fn as_socket(&self) -> BorrowedSocket<'_> {
        self.0.as_socket()
    }
}

#[cfg(windows)]
impl AsRawSocket for IoSource {
    fn as_raw_socket(&self) -> RawSocket {
        self.0.as_raw_socket()
    }
}

/// The readiness an I/O operation waits for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Interest {
    Readable,
    Writable,
}
```

`IoSource::new` is `pub(crate)` and unused until PR 3; that is a dead-code warning, so until
then mark it `#[cfg(any(test, feature = "async-io"))]` and use it from the `AsyncIo` tests in
Task 2 (they register a `UnixStream` pair). Remove the gate in PR 3.

- [ ] **Step 2: `traits.rs`**

```rust
//! What a runtime supplies to a connection.
//!
//! zbus ships no runtime of its own. A connection waits for socket readiness and timers, runs
//! its tasks and takes its locks on `async-io` (the default), on Tokio, or on whatever
//! implements [`Runtime`] and is handed to [`Builder::runtime`]. Implementing it takes a
//! readiness registration, a timer, a task handle, and a mutex and readers-writer lock, all of
//! which every async runtime already has; the host examples in the repository show the shape
//! for Tokio and for a plain `polling` event loop.
//!
//! [`Builder::runtime`]: crate::connection::Builder::runtime

use std::{
    future::Future,
    io,
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use super::{Interest, IoSource};

/// An async runtime, as seen by a connection.
pub trait Runtime: Send + Sync + 'static {
    type Registration: IoRegistration;
    type Sleep: Future<Output = ()> + Send + 'static;
    type Task<T: Send + 'static>: Task<T>;
    type Mutex<T: Send + 'static>: Mutex<T>;
    type RwLock<T: Send + Sync + 'static>: RwLock<T>;

    /// Register a socket or pipe for readiness notifications.
    ///
    /// The registration must stop watching the source before the [`IoSource`] it was given is
    /// released.
    fn register(&self, source: IoSource) -> io::Result<Self::Registration>;

    /// A future that completes at `deadline`. Dropping it cancels the timer.
    fn sleep_until(&self, deadline: Instant) -> Self::Sleep;

    /// Run `future` to completion in the background.
    ///
    /// The task runs concurrently with the caller, on whichever thread the runtime chooses. The
    /// handle resolves to `Err` if the runtime loses the task. Dropping the handle cancels the
    /// task; [`Task::detach`] lets it run on.
    fn spawn<T>(&self, future: impl Future<Output = T> + Send + 'static) -> Self::Task<T>
    where
        T: Send + 'static;

    /// A mutex holding `value`.
    fn mutex<T: Send + 'static>(&self, value: T) -> Self::Mutex<T>;

    /// A readers-writer lock holding `value`.
    fn rwlock<T: Send + Sync + 'static>(&self, value: T) -> Self::RwLock<T>;

    /// Run `work` off the event loop.
    ///
    /// `None` means the runtime offers no such service; zbus then reports the operation that
    /// needed it as unsupported instead of blocking anything. The future resolves to `Err` if the
    /// runtime loses the work.
    fn spawn_blocking<T>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Option<Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'static>>>
    where
        T: Send + 'static,
    {
        let _ = work;
        None
    }
}

/// Readiness for one registered [`IoSource`].
pub trait IoRegistration: Send + Sync + 'static {
    /// Wait for `interest`, then run `operation` on the polling thread.
    ///
    /// Return the first success (a partial write counts) or any error other than `WouldBlock`
    /// immediately. On `WouldBlock`, clear the readiness that was observed, arrange for `cx` to
    /// be woken when the source is ready again, and return `Pending`. Bound the number of
    /// retries, never spin, never block, and never keep `operation` beyond the call.
    fn poll_io<T>(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        operation: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>>;
}

/// A handle to a spawned task.
///
/// Dropping the handle cancels the task. A Tokio implementation wraps its `JoinHandle` in a
/// newtype that aborts on drop; an async-task style handle already behaves this way.
pub trait Task<T>: Future<Output = io::Result<T>> + Send + Unpin + 'static {
    /// Let the task run to completion on its own.
    fn detach(self);
}

/// A mutual-exclusion lock whose guard can be held across an `.await`.
pub trait Mutex<T>: Send + Sync + 'static {
    type Guard<'a>: DerefMut<Target = T> + Send
    where
        Self: 'a;

    fn lock(&self) -> impl Future<Output = Self::Guard<'_>> + Send;
}

/// A readers-writer lock whose guards can be held across an `.await`.
pub trait RwLock<T>: Send + Sync + 'static {
    type ReadGuard<'a>: Deref<Target = T> + Send + Sync
    where
        Self: 'a;
    type WriteGuard<'a>: DerefMut<Target = T> + Send
    where
        Self: 'a;

    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>> + Send;
    fn write(&self) -> impl Future<Output = Self::WriteGuard<'_>> + Send;
}
```

- [ ] **Step 3: wire and check**

In `zbus/src/runtime/mod.rs`, add at the top of the item list:

```rust
pub mod traits;

mod io_source;
pub use io_source::{Interest, IoSource};
```

Run `cargo +nightly fmt --all`, `cargo check -p zbus`, `cargo check -p zbus --no-default-features
--features tokio,proxy,service`, `cargo check -p zbus --target x86_64-pc-windows-gnu`,
`RUSTDOCFLAGS="-D warnings" cargo doc -p zbus --all-features --no-deps`, `cargo clippy -p zbus
--all-targets -- -D warnings`. Expected: clean.

- [ ] **Step 4: Commit**

```bash
/usr/bin/git add zbus/src/runtime
/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign -F - <<'EOF'
✨ zb: Add the traits through which a runtime serves a connection

A connection needs four things from an async runtime: readiness for its
socket, timers, somewhere to run its tasks and locks it can hold across
an await. Until now only the two compiled-in backends could supply them.
These traits describe exactly that surface so that any runtime can, with
the readiness registration kept separate from the socket itself: zbus
keeps creating, connecting and reading the socket, the runtime only
says when it is ready.

Nothing uses the traits yet; the built-in backend and the builder follow.

Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
EOF
```

---

### Task 2: One runtime value per connection for the built-in backends

This is the large task. It turns the cfg-selected executor and locks into a per-connection
`Runtime` value with a zero-cost variant per compiled backend, moves the async-io backend into
`AsyncIo`, and removes `internal_executor`, `Connection::executor`, `Executor` and the public
`Task`. No external runtime yet: that is Task 3.

**Files:**
- Create: `zbus/src/runtime/async_io.rs`, `zbus/src/runtime/locks.rs`
- Rewrite: `zbus/src/runtime/executor.rs`, `zbus/src/runtime/timeout.rs`, `zbus/src/runtime/mod.rs`
- Delete: `zbus/src/runtime/async_lock.rs`
- Modify: `zbus/src/lib.rs`, `zbus/src/connection/{mod,builder,socket_reader}.rs`,
  `zbus/src/object_server/{mod,interface/mod,interface/interface_ref,interface/interface_deref}.rs`,
  `zbus/src/proxy/mod.rs`, `zbus/src/address/transport/mod.rs` (only the `select_runtime!` import
  path if it moves)

**Interfaces:**
- Consumes: Task 1's traits and `IoSource`.
- Produces (crate-private): `runtime::Runtime` enum with `default_for_build() -> Result<Self>`,
  `spawn(&self, fut, name) -> Task<T>`, the free function `runtime::spawn_blocking(f, name)`,
  `mutex(&self, v) -> locks::Mutex<T>`, `rwlock(&self, v) -> locks::RwLock<T>`,
  `timeout(&self, fut, dur) -> Result<T>`; `Connection::runtime(&self) -> &Runtime`;
  `runtime::Task<T>` (`Future<Output = io::Result<T>>`, `detach`, drop cancels);
  `locks::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard}` with `lock`/`read`/
  `write`. Public: `zbus::runtime::AsyncIo` (`async-io` feature), `Builder` without
  `internal_executor`, `Connection` without `executor`.

- [ ] **Step 1: `async_io.rs`, the built-in implementor**

```rust
//! The built-in runtime: async-io for readiness and timers, async-executor for tasks, async-lock
//! for locks, `blocking` for blocking work.

use std::{
    future::Future,
    io,
    pin::Pin,
    sync::{Arc, OnceLock, Weak},
    task::{Context, Poll},
    time::Instant,
};

use async_executor::Executor;
use async_io::{Async, Timer};

use super::{
    Interest, IoSource,
    traits::{self, IoRegistration},
};

/// The runtime zbus uses by default: async-io, async-executor and async-lock.
///
/// Every connection built without an explicit runtime on the `async-io` feature runs on an
/// instance of this type, with one executor thread per instance that starts on the first spawn
/// and exits once the instance is gone and its tasks have finished. Handing an instance to
/// [`Builder::runtime`] selects it explicitly, even inside a Tokio runtime.
///
/// [`Builder::runtime`]: crate::connection::Builder::runtime
#[derive(Clone, Debug, Default)]
pub struct AsyncIo {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    executor: Executor<'static>,
    thread: OnceLock<()>,
}

impl AsyncIo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the executor thread if it is not running yet.
    ///
    /// The thread holds only a weak reference between ticks: it keeps running while tasks
    /// remain, and exits once every `AsyncIo` clone is gone and the executor is empty, which is
    /// what lets a connection's tasks finish during `graceful_shutdown`.
    fn ensure_thread(&self) {
        self.inner.thread.get_or_init(|| {
            let weak = Arc::downgrade(&self.inner);
            std::thread::Builder::new()
                .name("zbus::Connection executor".into())
                .spawn(move || run_executor(weak))
                .expect("failed to spawn the zbus executor thread");
        });
    }
}

/// Runs `Inner::executor` until nobody needs it any more.
///
/// `utils::block_on` picks tokio when both backends are enabled, which would make tasks on this
/// executor unexpectedly observe a tokio runtime, so the thread blocks on async-io directly.
fn run_executor(weak: Weak<Inner>) {
    async_io::block_on(async move {
        while let Some(inner) = weak.upgrade() {
            // Only this thread holds it and nothing is left to run: cancelled tasks were dropped
            // by the tick that observed the cancellation.
            if Arc::strong_count(&inner) == 1 && inner.executor.is_empty() {
                break;
            }
            inner.executor.tick().await;
        }
    })
}

impl traits::Runtime for AsyncIo {
    type Registration = Registration;
    type Sleep = Sleep;
    type Task<T: Send + 'static> = Task<T>;
    type Mutex<T: Send + 'static> = async_lock::Mutex<T>;
    type RwLock<T: Send + Sync + 'static> = async_lock::RwLock<T>;

    fn register(&self, source: IoSource) -> io::Result<Registration> {
        Async::new(source).map(Registration)
    }

    fn sleep_until(&self, deadline: Instant) -> Sleep {
        Sleep(Timer::at(deadline))
    }

    fn spawn<T>(&self, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        let task = self.inner.executor.spawn(future);
        self.ensure_thread();
        Task(task)
    }

    fn mutex<T: Send + 'static>(&self, value: T) -> async_lock::Mutex<T> {
        async_lock::Mutex::new(value)
    }

    fn rwlock<T: Send + Sync + 'static>(&self, value: T) -> async_lock::RwLock<T> {
        async_lock::RwLock::new(value)
    }

    fn spawn_blocking<T>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Option<Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'static>>>
    where
        T: Send + 'static,
    {
        Some(Box::pin(async move { Ok(blocking::unblock(work).await) }))
    }
}

/// An async-io registration.
#[derive(Debug)]
pub struct Registration(Async<IoSource>);

impl IoRegistration for Registration {
    fn poll_io<T>(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        mut operation: impl FnMut() -> io::Result<T>,
    ) -> Poll<io::Result<T>> {
        loop {
            match operation() {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                result => return Poll::Ready(result),
            }
            // async-io re-arms the interest and returns `Pending` unless a newer readiness
            // event is already there, so this loop cannot spin.
            let ready = match interest {
                Interest::Readable => self.0.poll_readable(cx),
                Interest::Writable => self.0.poll_writable(cx),
            };
            if let Err(e) = std::task::ready!(ready) {
                return Poll::Ready(Err(e));
            }
        }
    }
}

/// An async-io timer with the completion instant dropped.
#[derive(Debug)]
pub struct Sleep(Timer);

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll(cx).map(drop)
    }
}

/// An async-task handle: dropping it cancels the task.
#[derive(Debug)]
pub struct Task<T>(async_task::Task<T>);

impl<T: Send + 'static> traits::Task<T> for Task<T> {
    fn detach(self) {
        self.0.detach();
    }
}

impl<T> Future for Task<T> {
    type Output = io::Result<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(Ok)
    }
}
```

The trait impls for async-lock's types live in their own file, `zbus/src/runtime/async_lock.rs`
(replacing the old selector of that name), declared `#[cfg(any(feature = "async-io", test))]
mod async_lock;` in `mod.rs`, because both `AsyncIo` and the `cfg(test)` test runtime of Task 3
use them and a test build without the `async-io` feature still has the crate as a
dev-dependency:

```rust
//! The runtime lock traits for async-lock's types, used by `AsyncIo` and the test runtime.

use std::future::Future;

use super::traits;

impl<T: Send + 'static> traits::Mutex<T> for async_lock::Mutex<T> {
    type Guard<'a>
        = async_lock::MutexGuard<'a, T>
    where
        Self: 'a;

    fn lock(&self) -> impl Future<Output = Self::Guard<'_>> + Send {
        self.lock()
    }
}

impl<T: Send + Sync + 'static> traits::RwLock<T> for async_lock::RwLock<T> {
    type ReadGuard<'a>
        = async_lock::RwLockReadGuard<'a, T>
    where
        Self: 'a;
    type WriteGuard<'a>
        = async_lock::RwLockWriteGuard<'a, T>
    where
        Self: 'a;

    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>> + Send {
        self.read()
    }

    fn write(&self) -> impl Future<Output = Self::WriteGuard<'_>> + Send {
        self.write()
    }
}
```

(`async_lock::Mutex::lock` and friends are inherent methods, so `self.lock()` inside the trait
impl resolves to them, not to the trait; if rustc reports a recursion warning, call them as
`async_lock::Mutex::lock(self)`.)

`async-executor 1.14` implements `Default` and `Debug` for `Executor`; `blocking::unblock`
returns an `async_task::Task<T>` whose output is `T`. `Registration` and `Sleep` are only reached
through the trait's associated types, so they need no re-export; `Task` here is private to the
crate through the module (`pub(crate) mod async_io` with `pub use async_io::AsyncIo`).

Unit tests in the same file (`#[cfg(test)]`, each with `#[ntest::timeout(15000)]`): a spawned
task's output arrives; the executor thread exists after a spawn (`is_empty` false while a
`pending()` task lives, true after dropping its handle, and the thread name can be seen through
a task that reads `std::thread::current().name()`); `register` on one end of a
`std::os::unix::net::UnixStream::pair()` (unix only) reports readable after the other end writes,
using `poll_fn` + `poll_io(Readable, || stream.read(..))`; `sleep_until` in the past resolves
and one 20 ms ahead resolves after at least 20 ms.

- [ ] **Step 2: `locks.rs`**

```rust
//! Async locks selected by the connection's runtime.
//!
//! Each backend has its own lock types; an enum with one variant per compiled backend keeps them
//! at zero cost and leaves a slot for an external runtime's locks, which are reached through an
//! erased interface. Only the operations the crate uses exist: `lock`, `read`, `write`.

use std::{
    fmt,
    ops::{Deref, DerefMut},
};

pub(crate) enum Mutex<T> {
    #[cfg(feature = "async-io")]
    AsyncLock(async_lock::Mutex<T>),
    #[cfg(feature = "tokio")]
    Tokio(tokio::sync::Mutex<T>),
}

impl<T> Mutex<T> {
    pub(crate) async fn lock(&self) -> MutexGuard<'_, T> {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncLock(mutex) => MutexGuard::AsyncLock(mutex.lock().await),
            #[cfg(feature = "tokio")]
            Self::Tokio(mutex) => MutexGuard::Tokio(mutex.lock().await),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncLock(mutex) => mutex.fmt(f),
            #[cfg(feature = "tokio")]
            Self::Tokio(mutex) => mutex.fmt(f),
        }
    }
}

pub(crate) enum MutexGuard<'a, T> {
    #[cfg(feature = "async-io")]
    AsyncLock(async_lock::MutexGuard<'a, T>),
    #[cfg(feature = "tokio")]
    Tokio(tokio::sync::MutexGuard<'a, T>),
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncLock(guard) => guard,
            #[cfg(feature = "tokio")]
            Self::Tokio(guard) => guard,
        }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncLock(guard) => guard,
            #[cfg(feature = "tokio")]
            Self::Tokio(guard) => guard,
        }
    }
}
```

`RwLock<T>`, `RwLockReadGuard<'a, T>` and `RwLockWriteGuard<'a, T>` follow the same pattern with
`read()`/`write()`, `Deref` on both guards and `DerefMut` on the write guard. The `Debug` impl
matters: `ConnectionInner` and `ObjectServer` derive `Debug` and hold these.

- [ ] **Step 3: `executor.rs` becomes the crate-private `Task`**

Replace the file with:

```rust
//! The handle to a task a connection spawned on its runtime.

use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
};

/// A spawned task. Dropping the handle cancels the task; [`Task::detach`] lets it run on.
#[derive(Debug)]
pub(crate) struct Task<T>(pub(super) TaskInner<T>);

#[derive(Debug)]
pub(super) enum TaskInner<T> {
    #[cfg(feature = "async-io")]
    AsyncIo(super::async_io::Task<T>),
    #[cfg(feature = "tokio")]
    Tokio(TokioTask<T>),
}

impl<T> Task<T> {
    /// Detaches the task to let it keep running in the background.
    pub(crate) fn detach(self) {
        match self.0 {
            #[cfg(feature = "async-io")]
            TaskInner::AsyncIo(task) => super::traits::Task::detach(task),
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(task) => task.detach(),
        }
    }
}

impl<T> Future for Task<T> {
    type Output = io::Result<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.get_mut().0 {
            #[cfg(feature = "async-io")]
            TaskInner::AsyncIo(task) => Pin::new(task).poll(cx),
            #[cfg(feature = "tokio")]
            TaskInner::Tokio(task) => Pin::new(task).poll(cx),
        }
    }
}

/// A tokio task handle that aborts the task when dropped, matching `async_task::Task`.
#[cfg(feature = "tokio")]
#[derive(Debug)]
pub(super) struct TokioTask<T>(Option<tokio::task::JoinHandle<T>>);

#[cfg(feature = "tokio")]
impl<T> TokioTask<T> {
    pub(super) fn new(handle: tokio::task::JoinHandle<T>) -> Self {
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
    type Output = io::Result<T>;

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
                    Err(io::Error::other("tokio::task cancelled"))
                } else {
                    panic!("tokio::task::JoinHandle error: {e}")
                }
            }
        })
    }
}

/// Spawns `future` on tokio, naming the task when `tokio_unstable` is on.
#[cfg(feature = "tokio")]
pub(super) fn tokio_spawn<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
    #[allow(unused)] name: &str,
) -> tokio::task::JoinHandle<T> {
    // ... (the existing `tokio_spawn` body, unchanged)
}

/// Like [`tokio_spawn`], for blocking work.
#[cfg(feature = "tokio")]
pub(super) fn tokio_spawn_blocking<F, T>(
    f: F,
    #[allow(unused)] name: &str,
) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // ... (the existing `tokio_spawn_blocking` body, unchanged)
}
```

- [ ] **Step 4: `mod.rs`: the `Runtime` enum**

Replace `zbus/src/runtime/mod.rs`'s body after the module doc (keep the doc, rewritten as
below) with:

```rust
//! Integration with the async runtime that drives a connection.
//!
//! zbus does not ship a runtime of its own. A connection waits for socket readiness and timers,
//! runs its internal tasks and takes its locks on `async-io` (the default), on Tokio, or on any
//! implementation of [`traits::Runtime`] handed to [`Builder::runtime`]. The built-in async-io
//! backend is itself such an implementation, [`AsyncIo`]. [`AsyncDrop`] is the async counterpart
//! of [`Drop`] that zbus's own types implement.
//!
//! [`Builder::runtime`]: crate::connection::Builder::runtime

pub mod traits;

mod io_source;
pub use io_source::{Interest, IoSource};
#[cfg(feature = "async-io")]
pub(crate) mod async_io;
#[cfg(feature = "async-io")]
pub use async_io::AsyncIo;
mod async_drop;
pub use async_drop::AsyncDrop;
mod executor;
pub(crate) use executor::Task;
pub(crate) mod locks;
pub(crate) mod timeout;

// Only the `unixexec` and `ibus` transports and, on macOS, the `launchd` one run commands.
#[cfg(all(unix, any(feature = "unixexec", feature = "ibus", target_os = "macos")))]
pub(crate) mod process;

use std::future::Future;

use crate::{Error, Result};

/// The runtime a connection runs on, chosen once when it is built.
#[derive(Clone, Debug)]
pub(crate) enum Runtime {
    #[cfg(feature = "async-io")]
    AsyncIo(AsyncIo),
    #[cfg(feature = "tokio")]
    Tokio,
}

impl Runtime {
    /// The runtime for a connection built without an explicit one.
    ///
    /// Tokio when it is compiled in and a runtime is current on this thread, otherwise async-io
    /// when that is compiled in. This keeps the features additive: enabling `tokio` elsewhere in
    /// the dependency graph doesn't force every zbus user into a tokio runtime.
    pub(crate) fn default_for_build() -> Result<Self> {
        #[cfg(feature = "tokio")]
        if tokio::runtime::Handle::try_current().is_ok() {
            return Ok(Self::Tokio);
        }
        #[cfg(feature = "async-io")]
        {
            Ok(Self::AsyncIo(AsyncIo::new()))
        }
        #[cfg(not(feature = "async-io"))]
        {
            Err(Error::Unsupported)
        }
    }

    /// Spawns a task onto the runtime.
    pub(crate) fn spawn<T: Send + 'static>(
        &self,
        future: impl Future<Output = T> + Send + 'static,
        #[allow(unused)] name: &str,
    ) -> Task<T> {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncIo(runtime) => {
                Task(executor::TaskInner::AsyncIo(traits::Runtime::spawn(runtime, future)))
            }
            #[cfg(feature = "tokio")]
            Self::Tokio => Task(executor::TaskInner::Tokio(executor::TokioTask::new(
                executor::tokio_spawn(future, name),
            ))),
        }
    }

    pub(crate) fn mutex<T: Send + 'static>(&self, value: T) -> locks::Mutex<T> {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncIo(runtime) => {
                locks::Mutex::AsyncLock(traits::Runtime::mutex(runtime, value))
            }
            #[cfg(feature = "tokio")]
            Self::Tokio => locks::Mutex::Tokio(tokio::sync::Mutex::new(value)),
        }
    }

    pub(crate) fn rwlock<T: Send + Sync + 'static>(&self, value: T) -> locks::RwLock<T> {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncIo(runtime) => {
                locks::RwLock::AsyncLock(traits::Runtime::rwlock(runtime, value))
            }
            #[cfg(feature = "tokio")]
            Self::Tokio => locks::RwLock::Tokio(tokio::sync::RwLock::new(value)),
        }
    }
}
```

Add to `async_io.rs` the constructor the blocking arm needs:

```rust
impl<T> Task<T> {
    /// Wraps a `blocking::unblock` task, which is an async-task handle like the executor's.
    pub(super) fn from_blocking(task: async_task::Task<T>) -> Self {
        Self(task)
    }
}
```

`use_tokio()` and the `select_runtime!` macro are still needed by `zbus/src/address/transport/`
until PR 3; keep both in `mod.rs`, unchanged, below the enum (`use_tokio` stays gated on both
features as it is now). `Runtime::default_for_build` inlines the same check rather than calling
`use_tokio`, so the gate on `use_tokio` need not change. One visible difference in a tokio-only
build: building a connection with no Tokio runtime current now fails with `Error::Unsupported`
where it used to panic inside `tokio::spawn`; say so in the commit body if a test covers it.

- [ ] **Step 5: `timeout.rs`**

```rust
use std::{future::Future, io::ErrorKind, time::Duration};

use futures_lite::FutureExt;

use super::Runtime;
use crate::{Error, Result};

impl Runtime {
    /// Sleeps for `duration` on this runtime's timer.
    async fn sleep(&self, duration: Duration) {
        match self {
            #[cfg(feature = "async-io")]
            Self::AsyncIo(_) => {
                async_io::Timer::after(duration).await;
            }
            #[cfg(feature = "tokio")]
            Self::Tokio => tokio::time::sleep(duration).await,
        }
    }

    /// Awaits `fut`, failing with a timed-out error once `duration` has passed.
    pub(crate) async fn timeout<F, T>(&self, fut: F, duration: Duration) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        fut.or(async {
            self.sleep(duration).await;
            Err(Error::from(std::io::Error::new(
                ErrorKind::TimedOut,
                "timed out",
            )))
        })
        .await
    }
}
```

`Connection::call_method` (`zbus/src/connection/mod.rs:296`) becomes
`self.inner.runtime.timeout(reply_fut, timeout).await` (adapt the exact expression at the site).

- [ ] **Step 6: `lib.rs`**

```rust
#[cfg(feature = "comms")]
pub mod runtime;
#[cfg(feature = "comms")]
pub use runtime::AsyncDrop;
```

Search the crate for `crate::Executor`, `crate::Task` and `zbus::Task` (`/usr/bin/grep -rn
"crate::Task\b\|crate::Executor\|zbus::Task\|zbus::Executor" zbus zbus_macros book/src`) and
repoint each to `crate::runtime::Task` or to `Runtime` methods as the steps below describe.

- [ ] **Step 7: `connection/mod.rs`**

1. `ConnectionInner`: replace `executor: Executor<'static>` with `runtime: Runtime`; the four
   lock fields keep their names with types `runtime::locks::Mutex<..>`; add
   `serial_lock: locks::Mutex<()>`. Import `runtime::{Runtime, Task, locks::{Mutex, MutexGuard}}`
   and drop the `async_lock::{Semaphore, SemaphorePermit}` and `timeout::timeout` imports.
2. `Connection::new(auth, bus_connection, runtime: Runtime, method_timeout)`: create every lock
   through `runtime.mutex(..)` (`msg_senders: Arc::new(runtime.mutex(msg_senders))`,
   `subscriptions`, `socket_write`, `registered_names`, `serial_lock: runtime.mutex(())`) and
   store `runtime`.
3. Replace `pub fn executor(&self) -> &Executor<'static>` and its whole doc comment with

   ```rust
   /// The runtime this connection runs on.
   pub(crate) fn runtime(&self) -> &Runtime {
       &self.inner.runtime
   }
   ```

4. Every `self.executor().spawn(..)`, `inner.executor.spawn(..)` and `self.inner.executor.spawn`
   becomes the same call on `runtime()`/`inner.runtime`. `Task<()>` in `NameStatus` and the two
   `OnceLock<Task<()>>` fields refer to `runtime::Task`.
5. The flatpak workaround: delete `static SERIAL_NUM_SEMAPHORE` and `acquire_serial_num_semaphore`
   and add, in the same place,

   ```rust
   impl Connection {
       /// Makes serial assignment and sending one atomic step, but only inside Flatpak, where
       /// xdg-dbus-proxy needs it: <https://github.com/flatpak/xdg-dbus-proxy/issues/46>.
       async fn serial_lock(&self) -> Option<MutexGuard<'_, ()>> {
           if is_flatpak() {
               Some(self.inner.serial_lock.lock().await)
           } else {
               None
           }
       }
   }
   ```

   and change the five `let _permit = acquire_serial_num_semaphore().await;` call sites to
   `let _permit = self.serial_lock().await;`.
6. Tests: `client1.executor().spawn(..)` → `client1.runtime().spawn(..)`;
   `unix_p2p_async_io_backend` replaces the two `needs_internal_driver()` assertions with
   `assert!(matches!(server1.runtime(), Runtime::AsyncIo(_)))` (same for `client1`) and spawns
   its probe task with `server1.runtime().spawn(..)`; import `crate::runtime::Runtime` in the
   test module.

- [ ] **Step 8: `connection/builder.rs`**

1. Delete the `internal_executor` field, the `internal_executor` setter and its doc, the
   `start_internal_executor` function, and the `use crate::Executor` import.
2. `build_inner`:

   ```rust
   let runtime = Runtime::default_for_build()?;
   // Box the future as it's large and can cause stack overflow.
   Box::pin(self.build_(runtime, activate_msg_stream)).await
   ```

   (`build_` takes `runtime: Runtime` and passes it to `Connection::new`.) There is no
   `executor.run(..)` any more: the runtime runs what setup spawns.
3. `Interfaces<'a>` becomes
   `HashMap<ObjectPath<'a>, HashMap<InterfaceName<'static>, Box<dyn Interface>>>`; `serve_at`
   inserts `Box::new(iface)`; in `build_` the loop wraps each with
   `ArcInterface::new(&runtime, iface)` (see Step 9) before `add_arc_interface`. Check that
   `Interface` is object-safe with `Send + Sync + 'static` supertraits
   (`/usr/bin/grep -n "pub trait Interface" -A3 zbus/src/object_server/interface/mod.rs`); it is
   already used as `dyn Interface`.

- [ ] **Step 9: object server**

1. `ArcInterface { instance: Arc<RwLock<Box<dyn Interface>>>, spawn_tasks_for_methods: bool }`
   with

   ```rust
   impl ArcInterface {
       pub fn new(runtime: &Runtime, iface: Box<dyn Interface>) -> Self {
           let spawn_tasks_for_methods = iface.spawn_tasks_for_methods();
           Self {
               instance: Arc::new(runtime.rwlock(iface)),
               spawn_tasks_for_methods,
           }
       }
   }
   ```

   (`RwLock` is `crate::runtime::locks::RwLock`; the manual `Debug` impl keeps printing
   `Arc<RwLock<dyn Interface>>`, adjust the name to the new type.)
2. `ObjectServer` gains a `runtime: Runtime` field set in `new(conn)` from `conn.runtime().clone()`
   and creates `root` with `runtime.rwlock(Node::new(..))`; `root()` returns `&locks::RwLock<Node>`.
3. `at()` builds with `ArcInterface::new(&self.runtime, Box::new(iface))` inside the closure.
4. `dispatch_call_to_iface(iface: Arc<RwLock<Box<dyn Interface>>>, ..)` and the spawn in the
   `with_spawn` branch use `connection.runtime().spawn(..)` instead of `connection.executor()`.
5. `InterfaceRef::lock: Arc<RwLock<Box<dyn Interface>>>`; `InterfaceDeref::iface:
   RwLockReadGuard<'d, Box<dyn Interface>>` and the write guard likewise, from
   `crate::runtime::locks`. The `downcast_ref`/`downcast_mut` calls auto-deref through the
   `Box`, so their bodies do not change.
6. Anything else in `object_server/` naming `async_lock` (`/usr/bin/grep -rn async_lock
   zbus/src`) moves to `runtime::locks`.

- [ ] **Step 10: `socket_reader.rs` and `proxy/mod.rs`**

`SocketReader::spawn(self, runtime: &Runtime) -> Task<()>` calls `runtime.spawn(..)`;
`init_socket_reader` passes `&inner.runtime`. `PropertiesCache::new(.., runtime: &Runtime, ..)`
spawns on it; its caller passes `conn.runtime()`. The proxy test's
`server_conn.executor().spawn(server_fut, "server_task")` becomes `server_conn.runtime().spawn`.

- [ ] **Step 11: blocking work call sites**

`crate::Task::spawn_blocking(f, name)` is called from backend-specific socket code
(`connection/socket/{unix,tcp,vsock}.rs`) and from the transports
(`address/transport/{mod,tcp}.rs`).
Those files are not on this PR's path and stay backend-selected until PR 3; give them the
minimal change: a crate-private free function in `runtime/mod.rs`,

```rust
/// Blocking work for code that does not have a connection's runtime at hand yet.
///
/// Transports and sockets are rewritten to take the runtime in the I/O-path PR; until then they
/// pick the backend per call, as `select_runtime!` does.
pub(crate) fn spawn_blocking<F, T>(f: F, #[allow(unused)] name: &str) -> Task<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    select_runtime! {
        tokio: Task(executor::TaskInner::Tokio(executor::TokioTask::new(
            executor::tokio_spawn_blocking(f, name),
        ))),
        async_io: Task(executor::TaskInner::AsyncIo(async_io::Task::from_blocking(
            blocking::unblock(f),
        ))),
    }
}
```

and change the call sites to `crate::runtime::spawn_blocking(..)`. `Runtime` gets no
`spawn_blocking` method in this PR: nothing on this PR's path would call it, and the I/O and
ancillary PRs add it where the transports start taking the runtime.

- [ ] **Step 12: verify**

```bash
cargo +nightly fmt --all
cargo check -p zbus
cargo check -p zbus --no-default-features --features tokio,proxy,service
cargo check -p zbus --all-features
cargo check -p zbus --no-default-features
cargo check -p zbus --target x86_64-pc-windows-gnu
cargo check -p zbus --target x86_64-apple-darwin
cargo clippy -p zbus --all-targets -- -D warnings
cargo clippy -p zbus --no-default-features --all-targets --features tokio,p2p,proxy,service \
    -- -D warnings
cargo clippy -p zbus --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p zbus --all-features --no-deps
/usr/bin/grep -rn "internal_executor\|\.executor()\|needs_internal_driver\|Executor" zbus/src \
    book/src zbus_macros/src | /usr/bin/grep -v "async_executor\|AsyncExecutor"
dbus-run-session -- cargo test -p zbus
dbus-run-session -- cargo test -p zbus --no-default-features --features tokio,proxy,service --tests
dbus-run-session -- cargo test -p zbus --all-features -- --skip fdpass_systemd
```

Expected: all clean; the grep prints only the `book/src` mentions that Task 5 rewrites and
nothing in `zbus/src`; every suite passes except the environmental `ibus_connection`. The
`connection::Connection::executor` doctest no longer exists, so the tokio-only doc run in CI
(`--features tokio,service connection::Connection::executor`) will find nothing; Task 4 updates
that CI line.

- [ ] **Step 13: Commit**

```bash
/usr/bin/git add -A zbus/src
/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign -F - <<'EOF'
♻️ zb: Take a connection's tasks, locks and timers from one runtime value

The executor, the locks and the timeout each picked their backend on
their own, some by cargo feature and some per call, and the builder
started a driver thread by hand. A connection now carries one `Runtime`
value chosen when it is built, and everything that spawns, locks or
sleeps goes through it. The async-io backend becomes `AsyncIo`, an
implementor of the new runtime traits that owns its executor and starts
the executor thread on the first spawn, so the default path and an
explicitly selected async-io runtime are the same code.

With the runtime doing the driving there is nothing left to tick:
`Builder::internal_executor`, `Connection::executor` and the public
`Executor` and `Task` types go. Whoever ticked zbus's executor from
their own runtime will implement `Runtime::spawn` instead, which the
next commit makes possible. Two internal consequences: the flatpak
serial-number semaphore becomes a per-connection mutex, since no lock
can exist before a runtime does, and interfaces are wrapped in the
runtime's lock when the object server adds them rather than when the
builder collects them.

Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
EOF
/usr/bin/git log -1 --format=%B | awk 'length > 74 {print "LONG: " $0}'
```

---

### Task 3: External runtimes and the external-only build

**Files:**
- Create: `zbus/src/runtime/erased.rs`, `zbus/src/runtime/test_runtime.rs`
- Modify: `zbus/src/runtime/{mod,locks,executor,timeout}.rs`, `zbus/src/connection/builder.rs`,
  `zbus/src/{lib,utils}.rs`, `zbus/Cargo.toml` (dev-dependencies), and every site the
  external-only build fails on (see Step 6)

**Interfaces:**
- Consumes: Task 2's `Runtime` enum and enums.
- Produces: `Builder::runtime(impl traits::Runtime)`; `Runtime::External(Arc<dyn ErasedRuntime>)`
  with `External` variants on `Task`, `Mutex`, `RwLock` and their guards; a valid
  `--no-default-features --features $EXT` build.

- [ ] **Step 1: `erased.rs`**

```rust
//! Object-safe mirrors of the runtime traits.
//!
//! `Builder::runtime` takes any implementation and the connection must not be generic over it,
//! so the implementation is boxed behind these traits once. Operations that are generic over a
//! value type erase the value too: a task's output travels through a slot the typed handle reads,
//! and a lock holds `Box<dyn Any + Send>` that the typed wrapper downcasts, which is a `TypeId`
//! comparison per access.

use std::{
    any::Any,
    fmt,
    future::Future,
    io,
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::{Arc, Mutex as SyncMutex},
    task::{Context, Poll},
    time::Instant,
};

use super::{
    Interest, IoSource,
    traits::{self, IoRegistration},
};

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait ErasedRuntime: Send + Sync {
    fn register(&self, source: IoSource) -> io::Result<Box<dyn ErasedRegistration>>;
    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()>;
    fn spawn(&self, future: BoxFuture<'static, ()>) -> Box<dyn ErasedTask>;
    fn mutex(&self, value: Box<dyn Any + Send>) -> Box<dyn ErasedMutex>;
    fn rwlock(&self, value: Box<dyn Any + Send + Sync>) -> Box<dyn ErasedRwLock>;
    fn spawn_blocking(
        &self,
        work: Box<dyn FnOnce() + Send>,
    ) -> Option<BoxFuture<'static, io::Result<()>>>;
}

impl fmt::Debug for dyn ErasedRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<external runtime>")
    }
}

impl<R: traits::Runtime> ErasedRuntime for R {
    fn register(&self, source: IoSource) -> io::Result<Box<dyn ErasedRegistration>> {
        traits::Runtime::register(self, source)
            .map(|registration| Box::new(registration) as Box<dyn ErasedRegistration>)
    }

    fn sleep_until(&self, deadline: Instant) -> BoxFuture<'static, ()> {
        Box::pin(traits::Runtime::sleep_until(self, deadline))
    }

    fn spawn(&self, future: BoxFuture<'static, ()>) -> Box<dyn ErasedTask> {
        Box::new(traits::Runtime::spawn(self, future))
    }

    fn mutex(&self, value: Box<dyn Any + Send>) -> Box<dyn ErasedMutex> {
        Box::new(traits::Runtime::mutex(self, value))
    }

    fn rwlock(&self, value: Box<dyn Any + Send + Sync>) -> Box<dyn ErasedRwLock> {
        Box::new(traits::Runtime::rwlock(self, value))
    }

    fn spawn_blocking(
        &self,
        work: Box<dyn FnOnce() + Send>,
    ) -> Option<BoxFuture<'static, io::Result<()>>> {
        traits::Runtime::spawn_blocking(self, work)
    }
}

pub(crate) trait ErasedRegistration: Send + Sync {
    fn poll_io(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        operation: &mut dyn FnMut() -> io::Result<()>,
    ) -> Poll<io::Result<()>>;
}

impl<R: IoRegistration> ErasedRegistration for R {
    fn poll_io(
        &self,
        cx: &mut Context<'_>,
        interest: Interest,
        operation: &mut dyn FnMut() -> io::Result<()>,
    ) -> Poll<io::Result<()>> {
        IoRegistration::poll_io(self, cx, interest, operation)
    }
}

pub(crate) trait ErasedTask: Future<Output = io::Result<()>> + Send + Unpin {
    fn detach(self: Box<Self>);
}

impl<T: traits::Task<()>> ErasedTask for T {
    fn detach(self: Box<Self>) {
        traits::Task::detach(*self)
    }
}

/// A guard over an erased value; the typed wrapper downcasts through it.
pub(crate) trait ErasedGuard<V: ?Sized>: DerefMut<Target = V> + Send {}

impl<G, V: ?Sized> ErasedGuard<V> for G where G: DerefMut<Target = V> + Send {}

pub(crate) trait ErasedMutex: Send + Sync {
    fn lock(&self) -> BoxFuture<'_, Box<dyn ErasedGuard<Box<dyn Any + Send>> + '_>>;
}

impl<M: traits::Mutex<Box<dyn Any + Send>>> ErasedMutex for M {
    fn lock(&self) -> BoxFuture<'_, Box<dyn ErasedGuard<Box<dyn Any + Send>> + '_>> {
        Box::pin(async move {
            Box::new(traits::Mutex::lock(self).await)
                as Box<dyn ErasedGuard<Box<dyn Any + Send>> + '_>
        })
    }
}

/// A read guard: shared access only, so it is `Sync` too.
pub(crate) trait ErasedReadGuard<V: ?Sized>: Deref<Target = V> + Send + Sync {}

impl<G, V: ?Sized> ErasedReadGuard<V> for G where G: Deref<Target = V> + Send + Sync {}

pub(crate) trait ErasedRwLock: Send + Sync {
    fn read(&self) -> BoxFuture<'_, Box<dyn ErasedReadGuard<Box<dyn Any + Send + Sync>> + '_>>;
    fn write(&self) -> BoxFuture<'_, Box<dyn ErasedGuard<Box<dyn Any + Send + Sync>> + '_>>;
}

impl<L: traits::RwLock<Box<dyn Any + Send + Sync>>> ErasedRwLock for L {
    fn read(&self) -> BoxFuture<'_, Box<dyn ErasedReadGuard<Box<dyn Any + Send + Sync>> + '_>> {
        Box::pin(async move {
            Box::new(traits::RwLock::read(self).await)
                as Box<dyn ErasedReadGuard<Box<dyn Any + Send + Sync>> + '_>
        })
    }

    fn write(&self) -> BoxFuture<'_, Box<dyn ErasedGuard<Box<dyn Any + Send + Sync>> + '_>> {
        Box::pin(async move {
            Box::new(traits::RwLock::write(self).await)
                as Box<dyn ErasedGuard<Box<dyn Any + Send + Sync>> + '_>
        })
    }
}

/// The output slot a typed task reads once its erased handle resolves.
pub(crate) type OutputSlot<T> = Arc<SyncMutex<Option<T>>>;
```

If `impl<T: traits::Task<()>> ErasedTask for T` conflicts with coherence for some host type, the
alternative is a private newtype `Erased<T>(T)`; try the blanket impl first.

- [ ] **Step 2: `External` variants**

- `Runtime::External(Arc<dyn ErasedRuntime>)` in `mod.rs`; each `match` gains an arm:
  - `spawn`: box the future into `async move { *slot.lock().unwrap() = Some(future.await) }`
    with an `OutputSlot<T>`, call `runtime.spawn(..)`, return
    `Task(TaskInner::External { handle, output: slot })`.
  - the free `runtime::spawn_blocking` gains no external arm (it is backend-only until PR 3);
    gate it on `any(feature = "async-io", feature = "tokio")`.
  - `mutex`/`rwlock`: `locks::Mutex::External(locks::Erased::new(runtime.mutex(Box::new(value))))`.
- `locks.rs`: `Erased<T> { inner: Box<dyn ErasedMutex>, value: PhantomData<fn() -> T> }` and
  `ErasedGuardTyped<'a, T> { inner: Box<dyn ErasedGuard<Box<dyn Any + Send>> + 'a>, .. }` whose
  `Deref` is `self.inner.downcast_ref::<T>().expect("erased lock holds the value it was created
  with")`; same for the `RwLock` side with the `Send + Sync` value type. `Debug` for the
  `External` variants prints `<external>`.
- `executor.rs`: `TaskInner::External { handle: Box<dyn ErasedTask>, output: OutputSlot<T> }`
  polls the handle, then takes the slot: `Ok(value)` or
  `Err(io::Error::other("task finished without producing its output"))` if the host reported
  success but nothing was stored (cannot happen with a well-behaved host; do not panic).
  `detach` calls `ErasedTask::detach`.
- `timeout.rs`: `Self::External(runtime) => runtime.sleep_until(Instant::now() + duration).await`.
- `Runtime::default_for_build` is unchanged; `Builder` gets a `runtime: Option<Runtime>` field:

  ```rust
  /// Use `runtime` for this connection's readiness, timers, tasks and locks.
  ///
  /// Without this, a connection runs on Tokio when that is compiled in and a runtime is
  /// current, otherwise on the built-in async-io runtime. See [`crate::runtime`].
  pub fn runtime(mut self, runtime: impl traits::Runtime) -> Self {
      self.runtime = Some(Runtime::External(Arc::new(runtime)));
      self
  }
  ```

  and `build_inner` uses `self.runtime.take().map_or_else(Runtime::default_for_build, Ok)?`.

- [ ] **Step 3: `test_runtime.rs` (`cfg(test)`)**

An implementor built only from dev-dependencies, so it exists in every test build including the
external-only one: `spawn` on an `async_executor::Executor<'static>` driven by one std thread
running `futures_lite::future::block_on(executor.run(std::future::pending::<()>()))`;
`sleep_until` = `async_io::Timer::at`; locks = `async_lock`'s; `register` = `async_io::Async`
(same `poll_io` loop as `AsyncIo`'s); `spawn_blocking` = `Some(Box::pin(async {
Ok(blocking::unblock(work).await) }))`. Add `async-io`, `async-executor`, `async-task`,
`async-lock` and `blocking` to `[dev-dependencies]` in `zbus/Cargo.toml` with `workspace = true`
(all already in `Cargo.lock`; confirm `git diff Cargo.lock` stays empty). Name it `TestRuntime`
with `TestRuntime::new()`.

- [ ] **Step 4: tests for the external path** (in `zbus/src/runtime/mod.rs`'s test module, or a
  `runtime/tests.rs`; all gated `cfg(all(test, feature = "p2p"))` where a `Channel` is used;
  each `#[ntest::timeout(15000)]`):

```rust
#[test]
#[timeout(15000)]
fn external_runtime_drives_a_channel_connection() {
    futures_lite::future::block_on(async {
        let guid = crate::Guid::generate();
        let (c1, c2) = crate::connection::socket::Channel::pair();
        let server = Builder::authenticated_socket(c1, guid.clone())
            .p2p()
            .runtime(TestRuntime::new())
            .build()
            .await
            .unwrap();
        let client = Builder::authenticated_socket(c2, guid)
            .p2p()
            .runtime(TestRuntime::new())
            .build()
            .await
            .unwrap();
        assert!(matches!(server.runtime(), Runtime::External(_)));

        // A Peer.Ping round trip proves the reader task, the write lock and the timeout all run
        // on the external runtime.
        let reply = client
            .call_method(None::<()>, "/", Some("org.freedesktop.DBus.Peer"), "Ping", &())
            .await
            .unwrap();
        assert!(reply.body().signature().is_empty());

        // The client is gone before the server; shutting the server down must not hang.
        drop(client);
        server.graceful_shutdown().await;
    });
}
```

(Adapt `call_method`'s argument shapes to the current signature; `Peer` is served for every
connection when `service` is on, so gate on `feature = "service"` or call a method that exists
on a p2p connection without the object server — check `handshake`/`fdo` for what `Ping`
needs.) Add: `#[cfg(not(any(feature = "async-io", feature = "tokio")))]`
`session_without_runtime_is_unsupported` asserting
`block_on(Builder::session().build()).unwrap_err()` matches `Error::Unsupported`; an erasure
test that locks a `runtime.mutex(5u8)` on a `TestRuntime` through `Runtime::External`, holds the
guard across a `yield_now().await`, and reads 5; a task test: spawn `async { 7 }` through
`Runtime::External`, join `Ok(7)`; a cancelled task: spawn `pending()`, drop the `Task`, and the
runtime's executor reports empty (expose `TestRuntime::is_empty()` for the test).

- [ ] **Step 5: `utils::block_on` and `lib.rs`**

Remove the `compile_error!` block in `lib.rs` (lines 57-66). In `utils.rs`:

```rust
#[cfg(all(not(feature = "tokio"), feature = "async-io"))]
#[doc(hidden)]
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    async_io::block_on(future)
}

/// With neither backend a connection's tasks run on its runtime, so the blocking facade only
/// has to poll the caller's future.
#[cfg(not(any(feature = "tokio", feature = "async-io")))]
#[doc(hidden)]
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    futures_lite::future::block_on(future)
}
```

- [ ] **Step 6: make the external-only build compile**

```bash
EXT="blocking-api,proxy,service,object-manager,unixexec,ibus,tracing,p2p"
cargo check -p zbus --no-default-features --features $EXT
```

Fix every error and warning the build reports, with these rules: code that only makes sense with
a backend gets `#[cfg(any(feature = "async-io", feature = "tokio"))]`; a transport or discovery
path that needs a backend returns `Err(Error::Unsupported)` in the no-backend build (the runtime
path for those is PR 3 and PR 4); never delete functionality from the backend builds. Expected
sites, from a dry run: `connection/builder.rs` (`use async_io::Async`, the stream `Target`
variants), `address/transport/mod.rs` (`Transport::connect`: add
a no-backend arm returning `Err(Error::Unsupported)`
at the top and gate the rest), `address/transport/{unixexec,ibus,launchd}.rs` (`connect`/
`bus_address` return `Unsupported` without a backend), `runtime/process.rs` (gate the module on a
backend), `connection/socket/unix.rs` free functions used only by backend impls,
`runtime/mod.rs` `spawn_blocking` (gate on a backend), `blocking/connection/builder.rs`. Repeat
for `--target x86_64-pc-windows-gnu` and `--target x86_64-apple-darwin` with the same features,
then `cargo clippy -p zbus --no-default-features --all-targets --features $EXT -- -D warnings`
and `cargo test -p zbus --no-default-features --features $EXT --lib`.

Then prove the dependency claim:

```bash
FORBIDDEN="async-io|async-executor|async-task|async-lock|async-process|blocking|polling"
FORBIDDEN="$FORBIDDEN|async-channel|async-signal|piper"
cargo tree -e normal -p zbus --no-default-features --features $EXT \
    | /usr/bin/grep -E "$FORBIDDEN" \
    && echo "FORBIDDEN CRATE PRESENT" || echo "dependency graph clean"
```

Expected: `dependency graph clean`.

- [ ] **Step 7: full verification**

Everything from Task 2 Step 12, plus the external-only check/clippy/test/tree commands above and
`RUSTDOCFLAGS="-D warnings" cargo doc -p zbus --no-default-features --features $EXT --no-deps`.

- [ ] **Step 8: Commit**

```bash
/usr/bin/git add -A zbus
/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign -F - <<'EOF'
✨ zb: Accept any runtime through Builder::runtime

An application whose event loop is neither async-io nor Tokio can now
hand zbus an implementation of the runtime traits and have every task,
lock and timer of the connection run there, with none of the crates
behind the `async-io` feature in its dependency graph. The
implementation is boxed once behind object-safe mirrors of the traits;
the built-in backends keep their zero-cost variants, so only the
external path pays an allocation per spawn and per lock acquisition.

A build with neither backend is now valid: it needs an explicit
runtime for every connection and otherwise reports the connection as
unsupported. Until the I/O path learns to create sockets on the
runtime, such a build connects over a user-supplied `Socket`; the
transports and discovery helpers that need a backend say so with an
unsupported-operation error rather than failing to compile.

Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
EOF
/usr/bin/git log -1 --format=%B | awk 'length > 74 {print "LONG: " $0}'
```

---

### Task 4: CI: the external-only leg

**Files:**
- Modify: `.github/workflows/rust.yml`

- [ ] **Step 1: edit the workflow**

1. `check` job: after the wire-format-only checks, add
   ```yaml
   # The external-runtime-only build: no async-io, no tokio.
   EXT=blocking-api,proxy,service,object-manager,unixexec,ibus,tracing,p2p
   cargo --locked check -p zbus --no-default-features --features $EXT
   cargo --locked check -p zbus --no-default-features --features $EXT \
     --target x86_64-pc-windows-gnu
   cargo --locked check -p zbus --no-default-features --features $EXT \
     --target x86_64-apple-darwin
   ```
2. `clippy` job: the same feature set with `--all-targets -- -D warnings`.
3. `linux_test` matrix: add `external` to `suite`, and a step
   ```yaml
   - name: Test the external-runtime-only build
     if: matrix.suite == 'external'
     run: |
       FEATURES=blocking-api,proxy,service,object-manager,unixexec,ibus,tracing,p2p
       cargo --locked test --release --verbose -p zbus --no-default-features \
         --features $FEATURES --lib
       # Nothing behind the async-io feature may reach an external-runtime user.
       FORBIDDEN="async-io|async-executor|async-task|async-lock|async-process|blocking"
       FORBIDDEN="$FORBIDDEN|polling|async-channel|async-signal|piper"
       if cargo --locked tree -e normal -p zbus --no-default-features --features $FEATURES \
           | grep -E "$FORBIDDEN"; then
         echo "a crate owned by the async-io feature is in the external-only graph" >&2
         exit 1
       fi
   ```
4. The tokio doc step: replace the `connection::Connection::executor` doctest filter (the doctest
   no longer exists) with the whole `--doc` run for `--features tokio,service`, keeping the `-p
   zbus` comment.

- [ ] **Step 2: validate the YAML** (`python3 -c 'import yaml; yaml.safe_load(open("FILE"))'`
  with the workflow path, if PyYAML is available, otherwise a careful re-read) and run the three
  new check commands and the test step's commands locally.

- [ ] **Step 3: Commit**

```bash
/usr/bin/git add .github/workflows/rust.yml
/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign -F - <<'EOF'
👷 zb: Check the external-runtime-only build in CI

The promise of an external runtime is a dependency graph without the
crates behind the `async-io` feature, which only holds if something
checks it. This leg builds, lints and unit-tests zbus with neither
backend and fails if any of those crates shows up in the normal
dependency tree. The tokio doc run loses its filter for the executor
doctest, which no longer exists.

Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
EOF
```

---

### Task 5: The 6.0 upgrade guide

**Files:**
- Modify: `book/src/upgrading-to-6.md` (a `###` subsection under "Other changes in 6.0"),
  `book/src/faq.md` (the entry that mentions `internal_executor`, if any:
  `/usr/bin/grep -n "internal_executor\|executor()" book/src/*.md`)

- [ ] **Step 1: write the subsection**, placed before `### Logging through `tracing` is a feature`:

```markdown
### `Builder::runtime` replaces `internal_executor`

A connection used to pick its executor and locks by cargo feature, with
`Builder::internal_executor(false)` and `Connection::executor().tick()` as the way to drive
zbus's tasks from another runtime. That pair is gone, together with the `Executor` and `Task`
types. A connection now takes its readiness, timers, tasks and locks from one runtime: Tokio when
the `tokio` feature is on and a runtime is current, otherwise the built-in async-io runtime, or
whatever you pass to `Builder::runtime`:

```rust,ignore
let conn = zbus::connection::Builder::session()
    .runtime(my_runtime)
    .build()
    .await?;
```

`my_runtime` implements `zbus::runtime::traits::Runtime`: a readiness registration, a timer, a
task handle and two locks, all of which every async runtime already has. Code that ticked the
executor from Tokio implements `spawn` with `tokio::spawn` and is done. Nothing changes for
connections built without `runtime`.

A build with `comms` but neither `async-io` nor `tokio` is now valid; every connection in it
needs a `runtime`, and until the standard transports learn to create sockets on it, such a build
connects over a socket you supply.
```

(Use ```` ```rust,ignore ```` only because the snippet is a fragment; the book's other snippets
do the same for fragments. Check `/usr/bin/grep -c "rust,ignore" book/src/upgrading-to-6.md`.)

- [ ] **Step 2: `mdbook build` if installed, `awk 'length > 100'` on the file, commit**

```bash
/usr/bin/git add book/src
/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign -F - <<'EOF'
📝 book: Document Builder::runtime in the 6.0 guide

`internal_executor` and `Connection::executor` are gone and
`Builder::runtime` is how a connection is put on another runtime now;
the upgrade guide says so next to the other 6.0 API changes.

Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ
EOF
```

---

### Task 6: Final verification and PR

- [ ] **Step 1**: `/usr/bin/git fetch zeenix && /usr/bin/git rebase zeenix/runtime-module`, then the
  CI-equivalent matrix: `cargo +nightly fmt --all -- --check`; the `check` job's commands
  (including the new external-only ones and the three cross targets); the clippy job's feature
  sets; `RUSTDOCFLAGS=-D warnings` doc builds for all-features, tokio-only and external-only;
  `dbus-run-session -- cargo test --all-features -- --skip fdpass_systemd`;
  `dbus-run-session -- cargo test -p zbus --no-default-features --features tokio,proxy,service
  --tests`;
  `cargo test -p zbus --no-default-features --features $EXT --lib`; the `cargo tree` assertion;
  `/usr/bin/git status --short` clean; `Cargo.lock` unchanged; every commit body ≤ 74 columns.
- [ ] **Step 2**: `/usr/bin/git log --oneline zeenix/runtime-module..HEAD` shows five commits in the
  order above; a grep for `async_lock|async_executor|async_task|async_io::|blocking::` over
  `zbus/src/runtime/{traits,io_source,erased,locks,executor,timeout}.rs` prints nothing (only
  `async_io.rs`, `async_lock.rs`, `test_runtime.rs` and the gated arms may name them).
- [ ] **Step 3**: push `runtime-abstraction` to `zeenix` and open the PR against `main`:
  title `✨ zb: Let a connection run on any runtime through Builder::runtime`, body: second of
  four PRs for #1960 (design #1961), based on #1962; the five commits in one paragraph each; the
  note that the built-in paths are zero-cost enum variants and only the external path boxes; the
  external-only build's current limits (user-supplied sockets until PR 3); the removed API; the
  usual generated-with footer and session link.
