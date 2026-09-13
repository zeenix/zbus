# External runtime integration — design

Resolves z-galaxy/zbus#1960 ("Support external runtimes while retaining the async-io default").
The follow-up proposal in #1959 (replacing the smol-based internals of the built-in backend) is
out of scope; this design only has to leave it possible, and the private scheduler and locks it
introduces are the seed of that work.

One sentence of #1960 is deliberately not followed: "External connections reuse async-executor
through zbus's existing Executor/Task abstractions." That sentence entered the issue when it was
split out of #1959 and contradicts the point of the split. An external-runtime user must not
depend on any of the crates the `async-io` feature owns.

## The problem

A connection needs three things from a runtime: readiness wakeups for its socket, timers, and
somewhere to run its tasks (the socket reader, the object server dispatcher, name-lost watchers,
spawned method handlers). Today those are coupled to two compiled-in backends:

- `async-io`: sockets are `Arc<async_io::Async<T>>`, timers are `async_io::Timer`, tasks run on an
  `async_executor::Executor` ticked by a dedicated `zbus::Connection executor` thread that the
  builder spawns after setup (`internal_executor(false)` leaves ticking to the caller through
  `Connection::executor().tick()`).
- `tokio`: sockets are `tokio::net` types, timers are `tokio::time`, tasks are `tokio::spawn`ed.

Nothing else can supply wakeups. A GLib, `polling`, `mio` or custom event loop host cannot use zbus
without accepting async-io's reactor thread and executor thread in its process, and a Tokio user
on an unusual configuration cannot integrate through Tokio's `AsyncFd` either. The choice is also
made per call in places (`select_runtime!`, `use_tokio()`), which the code itself documents as
safe only for call sites that are independent of the socket's reactor.

## Goals

- Let a host supply socket readiness, timers and, optionally, task driving and blocking work,
  through a small public trait, while zbus keeps creating and connecting sockets, doing
  authentication, framing and FD passing.
- An external-runtime user depends on none of the crates behind the `async-io` feature:
  `async-io`, `async-executor`, `async-task`, `async-lock`, `async-process`, `blocking` and
  their transitive dependencies. A build with `comms` and neither `async-io` nor `tokio` (an
  "external-only" build) compiles and works with an explicit reactor.
- Keep the default behaviour: session/system connections stay automatic, `async-io` stays the
  default non-Tokio backend, native Tokio keeps spawning tasks directly.
- Add no zbus thread on the external path, at any point of the connection's lifetime.
- Keep `Builder` and `Connection` non-generic and keep wire-only builds untouched.

## Non-goals

- Replacing async-io, async-executor or async-lock for the built-in backend (#1959).
- A thread-free `zbus::blocking` facade on top of an external reactor.
- Completion-based hosts (IOCP, io_uring): they need the custom `Socket` route.
- zbus sharing one executor or driver across connections; a host may hand the same reactor to
  several builders if its implementation is cheap to clone.

## Decisions

1. **One reactor path for all non-Tokio I/O.** The async-io backend becomes the built-in
   `zbus::runtime::Reactor`, an ordinary implementor of the public trait. The transport connect
   path goes through one private registered-socket wrapper for the built-in and external reactors
   alike. Native Tokio keeps its own sockets and tasks. Each connection latches a private runtime
   choice at build time; `select_runtime!` and `use_tokio()` are removed.
2. **No smol crate for external users.** The `async-io` feature keeps owning `async-io`,
   `async-executor`, `async-task`, `async-lock`, `async-process` and `blocking`. External
   connections run zbus's tasks on a private scheduler (`runtime::scheduler`) that `Run` polls,
   whichever backends are compiled in. External-only builds use private locks
   (`runtime::sync`) built on `event-listener`, already a shared dependency; builds with a
   backend keep that backend's locks until #1959.
3. **One ancillary hook, `spawn_blocking`.** It covers DNS, NSS group lookup, nonce-file reads and
   subprocess work. Hosts that do not implement it get `Error::Unsupported` before zbus starts any
   helper.
4. **Driver state lives outside `ConnectionInner`.** `Run` and the automatic observer hold no
   strong reference to the connection, so `graceful_shutdown` keeps meaning "wait for the last
   strong handle to go away".
5. **Four sequential PRs**: the module rename (#1962); scheduler and locks; reactor traits,
   built-in reactor, socket wrapper and driving; ancillary operations, hosts, examples and docs.

## Public API

Everything below is behind the `comms` feature and lives in `zbus::runtime` unless stated.

### Module layout

`zbus/src/abstractions/` is renamed to `zbus/src/runtime/` (done in #1962). The root re-exports
`Executor`, `Task` and `AsyncDrop` are preserved, and `zbus::runtime` is a public module:

```text
zbus/src/runtime/
├── mod.rs          # pub mod traits; pub use reactor::Reactor; IoSource, Interest; private Runtime
├── traits.rs       # Reactor, IoRegistration
├── reactor.rs      # the built-in async-io implementation (feature = "async-io")
├── erased.rs       # ErasedReactor / ErasedRegistration and the blanket impl (private)
├── io.rs           # Registered<K> socket wrapper and the connect helpers (private)
├── executor.rs     # Executor, Task: enums over the compiled backends and the scheduler
├── scheduler.rs    # the private task scheduler used by external connections
├── driver.rs       # DriverState, Run (Run is re-exported from `connection`)
├── timeout.rs      # timeout over the connection's runtime
├── async_lock.rs   # selects async-lock, tokio::sync or `sync` per build
├── sync.rs         # private Mutex, RwLock, Semaphore built on event-listener
├── async_drop.rs   # unchanged
└── process.rs      # gains the reactor-path implementation in PR 4
```

### Traits

```rust
pub mod traits {
    pub trait Reactor: Send + Sync + 'static {
        type Registration: IoRegistration;
        type Sleep: Future<Output = ()> + Send + 'static;

        /// Register a socket or pipe for readiness notifications.
        fn register(&self, source: IoSource) -> io::Result<Self::Registration>;

        /// A future that completes at `deadline`. Dropping it cancels the timer.
        fn sleep_until(&self, deadline: Instant) -> Self::Sleep;

        /// Take over driving the connection after setup.
        ///
        /// `Ok(Some(driver))` hands the future back unpolled: the caller drives
        /// `Connection::run()`. `Ok(None)` means the implementation has scheduled `driver` on its
        /// runtime. `Err` fails `build()`. Must neither block nor poll `driver` inline.
        fn start_driver(&self, driver: Run) -> io::Result<Option<Run>> {
            Ok(Some(driver))
        }

        /// Run `work` off the event loop. `None` means the host offers no blocking service.
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

    pub trait IoRegistration: Send + Sync + 'static {
        /// Wait for `interest` readiness, then run `operation` on the polling thread.
        fn poll_io<T>(
            &self,
            cx: &mut Context<'_>,
            interest: Interest,
            operation: impl FnMut() -> io::Result<T>,
        ) -> Poll<io::Result<T>>;
    }
}
```

`poll_io` contract, documented on the trait: return the first success (a partial write counts) or
any error other than `WouldBlock` immediately; on `WouldBlock` clear the readiness the
implementation observed, arrange a wakeup for `cx` and return `Pending`; bound retries, never
spin, never block, never retain `operation`. The registration must deregister the source before
its `IoSource` is released. `Interest` is `enum Interest { Readable, Writable }`.

`spawn_blocking` returns an `io::Result<T>` because host executors can lose a task (a cancelled
Tokio `JoinHandle`, a torn-down thread pool). Its future is boxed; blocking work happens a handful
of times per connection, at setup, so the allocation is irrelevant.

### `IoSource`

```rust
#[derive(Clone, Debug)]
pub struct IoSource(Arc<Owned>);   // Owned = OwnedFd on unix, OwnedSocket on windows
```

Implements `AsFd` and `AsRawFd` (unix) or `AsSocket` and `AsRawSocket` (windows). It is a shared
owner: the socket wrapper keeps one clone for I/O and the registration keeps whatever it needs.
Users receive one from `register` and never construct one. `async_io::Async::new` accepts it
directly because `Arc<T>: AsFd` where `T: AsFd`.

### Built-in reactor

```rust
#[cfg(feature = "async-io")]
#[derive(Clone, Debug, Default)]
pub struct Reactor;   // zbus::runtime::Reactor
```

`register` wraps the source in `async_io::Async::new`; `Registration::poll_io` is the
`poll_readable`/`poll_writable` loop that `Arc<Async<UnixStream>>` uses today. `sleep_until` is
`async_io::Timer::at` behind a small future that drops the `Instant` output. `start_driver` spawns
the existing `zbus::Connection executor` thread running `async_io::block_on(driver)` and returns
`Ok(None)`. `spawn_blocking` returns `blocking::unblock`. Selecting it explicitly with
`Builder::reactor(Reactor)` forces async-io even inside a Tokio runtime, and such a connection
runs its tasks on the private scheduler like any other explicit reactor.

### Builder and Connection

```rust
impl Builder<'_> {
    /// Use `reactor` for this connection's readiness, timers and driving.
    pub fn reactor(self, reactor: impl traits::Reactor) -> Self;
    /// Unchanged signature; see selection rules.
    pub fn internal_executor(self, enabled: bool) -> Self;
}

impl Connection {
    /// The connection driver, or a completion observer when the connection drives itself.
    pub fn run(&self) -> Run;
}

/// `Send + 'static`, `Future<Output = zbus::Result<()>>`.
pub struct Run { .. }   // zbus::connection::Run
```

`reactor` erases the implementation into `Arc<dyn ErasedReactor>` immediately. Calling `reactor`
and `internal_executor` on the same builder, in either order, records `Error::Unsupported` in the
builder's deferred-error slot and `build()` reports it.

## Runtime selection

At the start of `build_inner`, before any I/O or discovery, the builder picks one of:

| Builder state | Runtime | Tasks | Driving |
| --- | --- | --- | --- |
| `reactor(r)` | `Reactor(r)` | private scheduler | `r.start_driver` |
| neither, `tokio` compiled and a runtime is current | `Tokio` | `tokio::spawn` | Tokio |
| neither, `async-io` compiled | `Reactor(built-in)` | async-executor | thread or caller |
| neither, no backend compiled | `Error::Unsupported` | | |

`Runtime` is a private `enum Runtime { #[cfg(feature = "tokio")] Tokio, Reactor(Arc<dyn
ErasedReactor>) }` stored in `ConnectionInner` and passed to `Transport::connect`, the handshake
and `timeout`. With `internal_executor(false)` on the built-in reactor the builder skips
`start_driver`; the caller then ticks async-executor through `Connection::executor()` as today, or
polls `run()`, which does the same thing.

## Executor and tasks

### `Executor` and `Task`

Both keep their public surface and become enums over what is compiled in:

```rust
pub struct Executor<'a> {
    inner: Inner,
    // The lifetime is part of the public type only; every future zbus spawns is `'static`.
    lifetime: PhantomData<&'a ()>,
}

enum Inner {
    #[cfg(feature = "async-io")]
    AsyncExecutor(Arc<async_executor::Executor<'static>>),
    #[cfg(feature = "tokio")]
    Tokio,
    Scheduler(Arc<scheduler::Scheduler>),
}

pub struct Task<T>(TaskInner<T>);

enum TaskInner<T> {
    #[cfg(feature = "async-io")]
    AsyncExecutor(async_task::Task<T>),
    #[cfg(feature = "tokio")]
    Tokio(tokio::task::JoinHandle<T>),
    Scheduler(scheduler::JoinHandle<T>),
}
```

`spawn`, `is_empty`, `tick`, `run`, `detach` and `Future::poll` dispatch on the variant; the
tokio variant's abort-on-drop lives in a private `TokioTask` wrapper so `Task` itself needs no
`Drop`. `Executor<'a>` thereby goes from invariant (async-executor's `Executor<'a>` is) to
covariant in `'a`, a relaxation nobody can observe since only `Executor<'static>` is ever
handed out. `Executor::new()` keeps today's outcome for the two built-in backends; explicit
reactors and external-only builds get `Scheduler`. In PR 2 the `Scheduler` variant, the
`scheduler` and `sync` modules are gated `#[cfg(any(test, not(any(feature = "async-io",
feature = "tokio"))))]`; PR 3 widens the variant and the scheduler module to every build (an
explicit reactor uses them whichever backends are compiled), gates `sync::RwLock` on `service`
like the re-export, and rewrites the `zbus::runtime` module doc for the third choice.
`Task::spawn_blocking` becomes a method on the connection's
runtime in PR 3 (tokio pool, `blocking::unblock`, or the reactor's hook); until then it keeps its
`select_runtime!` body.

### The private scheduler (`runtime::scheduler`)

Deliberately small; a `std::sync::Mutex` plus `VecDeque` is enough. No `unsafe`.

```rust
pub(crate) struct Scheduler {
    ready: Mutex<VecDeque<Arc<TaskCell>>>,
    // Every task that has neither finished nor been cancelled. Owning the cells is what lets
    // dropping the scheduler drop every pending future; a counter could not reach them.
    tasks: Mutex<Vec<Arc<TaskCell>>>,
    // Wakers of everyone currently inside `run` or `tick`; a single slot would drop one.
    drivers: Mutex<Vec<Waker>>,
}

struct TaskCell {
    slot: Mutex<Slot>,            // Idle(future) | Running | Done
    scheduled: AtomicBool,        // in the ready queue; suppresses duplicate enqueues
    rerun: AtomicBool,            // set by a driver that found the cell `Running` elsewhere
    cancelled: AtomicBool,
    scheduler: Weak<Scheduler>,
}

impl Wake for TaskCell {
    fn wake(self: Arc<Self>) { /* enqueue unless already scheduled, then wake the drivers */ }
}

pub(crate) struct JoinHandle<T> {
    cell: Arc<TaskCell>,
    output: Arc<Mutex<Output<T>>>,  // value, `finished` flag and the joiner's Waker
    detached: bool,
}
```

- `spawn` wraps the future so that a marker created *before* the async block (a block only runs
  its body once polled, so a task dropped before its first poll would otherwise never signal)
  marks the output finished and wakes the joiner whenever the future is dropped, completed or
  not; creates the cell, enqueues it, wakes the drivers, returns the handle.
- `tick().await` polls the next queued task; `run(fut)` interleaves bounded batches (16 tasks)
  with polls of `fut`; after a full batch it wakes its own waker and returns `Pending`, so
  unrelated host futures progress and the rest of the queue is reached even when no task wakes
  anything. A task that panics propagates the panic out of `run`/`tick` and is forgotten as if
  cancelled: a guard live across the poll sets its slot `Done` and removes it from `tasks` on
  unwind, so `is_empty()` stays truthful.
- Polling a task moves its future out of the slot (`Idle` → `Running`), clears `scheduled`,
  polls with no lock held, and restores `Idle` unless it finished. A wake during the poll
  re-enqueues the cell; a second driver that pops a `Running` cell sets `rerun` instead of
  polling, and the first driver re-enqueues it after its poll. Wakers and futures are never
  invoked under any of the scheduler's locks.
- Dropping a `JoinHandle` that is not detached cancels: `cancelled` is set, then the slot is set
  `Done` (dropping the future) if it is `Idle`; if it is `Running` the polling driver drops it
  after the poll. `detach()` lets the task finish on its own. `JoinHandle::poll` returns `Ok(T)`
  once the value is stored, or `Err(io::Error::other("task cancelled"))` once the task is
  finished without one.
- `is_empty()` is `tasks.is_empty()` and has no wakeup source of its own: nothing may wait on it
  without a notification. Dropping the last `Arc<Scheduler>` sets every slot `Done`, which
  drops every remaining future even though reactors may still hold their wakers.

### The private locks (`runtime::sync`)

Compiled when neither backend is present (and under `cfg(test)` everywhere so the unit tests run
in every configuration). `async_lock.rs` becomes the selector:

```rust
#[cfg(feature = "async-io")]
pub(crate) use async_lock::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard, Semaphore, ...};
#[cfg(all(feature = "tokio", not(feature = "async-io")))]
pub(crate) use tokio::sync::{...};                 // as today
#[cfg(not(any(feature = "async-io", feature = "tokio")))]
pub(crate) use super::sync::{...};
```

The API the crate uses, and therefore all `sync` has to provide: `Mutex::new`, `lock()`;
`RwLock::new`, `read()`, `write()`, with `T: ?Sized` so `Arc<RwLock<dyn Interface>>` coerces and
the guards deref to `dyn Interface`; `Semaphore::new` (`const`), `acquire()` returning a guard.
All three are built on one `event_listener::Event` per lock plus a `std::sync::Mutex` or atomics
for the state:

- `Mutex`: `locked: AtomicBool`; `lock()` loops try-lock / `listen()` / re-try / await.
  Unlock calls `notify(1)`; event-listener forwards a notification whose listener is dropped
  before receiving it, so a cancelled `lock()` future cannot strand the next waiter.
- `RwLock`: `state: Mutex<{ readers: usize, writer: bool, writers_waiting: usize }>` and two
  events, `readers` (`notify(usize::MAX)`) and `writer` (`notify(1)`). A waiting writer blocks new
  readers, which is the writer-preferring policy the object server wants: `add_interface` must not
  starve behind a stream of method calls. No upgrading, no `try_` variants: nothing uses them.
- `Semaphore`: `permits: AtomicUsize` plus one event; `acquire()` is the mutex loop generalised to
  `n` permits. Releasing a permit uses `notify_additional(1)`: plain `notify(1)` coalesces with
  a notification that is already pending, so two permits released concurrently would wake one
  of two waiters. `SERIAL_NUM_SEMAPHORE` is a `static`, hence the `const fn new`.

None of the three guarantees fairness beyond "a release wakes a waiter": the try-before-listen
loop lets a newcomer barge ahead of a woken waiter, as async-lock's does. Guards are
`#[must_use]`, implement `Deref`/`DerefMut`, and are `Send`/`Sync` under exactly async-lock's
bounds. `RwLock`'s `Debug` has no `T: Debug` bound, so that `RwLock<dyn Interface>` stays
`Debug`; it therefore prints no fields.

## Driving

### State

```rust
pub(crate) struct DriverState {
    executor: Executor,
    terminated: Event,               // replaces ConnectionInner::drop_event
    mode: OnceLock<Mode>,            // Automatic | Manual, set after setup
    claimed: AtomicBool,             // a manual Run is active
    pending_method_calls: PendingMethodCalls,
    socket_status: Arc<SocketStatus>,
}
```

`ConnectionInner` holds an `Arc<DriverState>` and notifies `terminated` from its `Drop`. `Run`
holds only an `Arc<DriverState>` and a listener. None of those is a strong reference to the
connection, so `graceful_shutdown` still waits exactly for `ConnectionInner`'s destruction and a
`Run` cannot keep a connection alive.

### Construction

`build_inner` runs the whole setup inside `executor.run(build_)` as today, so the executor is
driven temporarily while connecting, authenticating, acquiring names and starting the object
server. For the private scheduler that means `Scheduler::run` interleaves the setup future with
the tasks it spawns; the host's event loop is what wakes it. No thread is started on any path
before setup completes. Afterwards:

- `Tokio`: `mode = Automatic`, nothing to hand off.
- Built-in reactor with `internal_executor(false)`: `mode = Manual`, nothing called.
- Otherwise: create a `Run`, call `reactor.start_driver(run)`. `Ok(None)` sets `Automatic`;
  `Ok(Some(run))` sets `Manual` and drops the unpolled `Run`, which is a no-op; `Err` fails the
  build and the connection scope is cleaned up by dropping it.

### `Run` semantics

- `Connection::run()` returns a fresh `Run` from any handle, at any time.
- Automatic mode: `poll` waits on `terminated` and resolves `Ok(())`. Dropping it stops observing.
- Manual mode: the first `poll` claims the driver; a second concurrent claim resolves
  `Err(Error::Unsupported)` without touching the active driver. The claimed `Run` then drives
  `executor.run(terminated.listen())`, boxed once, until termination; both async-executor's `run`
  and the private scheduler's interleave task batches with polls of the inner future, so
  unrelated host futures keep making progress and shutdown work keeps running.
- Dropping an unpolled `Run` does nothing. Dropping a claimed `Run` is an abrupt stop:
  `pending_method_calls.fail_all(..)`, `socket_status.closed = true`, `closed_event.notify`. No
  replacement thread is started; tasks left in the scheduler are dropped with its last reference.
- A driver accepted by `start_driver` and then discarded by the host is the same abrupt stop.
- `Executor::tick()` keeps working next to an active driver on either executor; no coordination
  beyond the claim flag is needed. `Connection::executor()` is kept and its docs point to `run()`
  as the preferred way to drive.

## I/O path

### Registered sockets

```rust
pub(crate) struct Registered<K> {
    source: IoSource,
    registration: Box<dyn ErasedRegistration>,
    kind: K,
}
```

`K` selects the syscalls: `UnixStream` (recvmsg/sendmsg with SCM_RIGHTS via the existing
`fd_recvmsg`/`fd_sendmsg` helpers, peer credentials), `TcpStream` and `VsockStream` (recv/send, no
FDs) and, in PR 4, `Pipe` (read/write for unixexec). `ReadHalf` and `WriteHalf` are implemented on
`Arc<Registered<K>>`, mirroring today's `Arc<Async<T>>` impls, with `read_with`/`write_with` helpers
built on `poll_fn` + `poll_io`. The public `Socket` impls for `Async<T>` and the Tokio types stay
for user-supplied sockets.

### Connecting

Transports create sockets with `socket2` (already in the lock through Tokio; added to `comms`) as
non-blocking, close-on-exec sockets. `connect` either succeeds or pends with `EINPROGRESS`
(`WouldBlock` on unix sockets with a full backlog); a pending connect registers the source, waits
for writable readiness and then checks `take_error()`. This is the same for unix, tcp and vsock
(`SockAddr::vsock` on Linux). Peer credentials on the reactor path call `SO_PEERCRED` /
`getpeereid` / `SO_PEERPIDFD` directly; the supplementary-group lookup (`getpwuid_r`,
`getgrouplist`) goes through `spawn_blocking` and stays `None` when the hook is absent.

### Timers

`runtime::timeout(runtime, fut, duration)` races `fut` against `tokio::time::sleep` or
`reactor.sleep_until(Instant::now() + duration)`. `Connection::call_method` passes
`self.inner.runtime`.

### Ancillary operations (PR 4)

| Operation | Tokio | Built-in reactor | External reactor |
| --- | --- | --- | --- |
| DNS for tcp hostnames | tokio resolver | `blocking::unblock` | hook, else `Unsupported` |
| nonce-tcp file | `tokio::fs` | `blocking::unblock` | hook, else `Unsupported` |
| ibus/launchd `output()` | tokio process | async-process | hook + std process, else `Unsupported` |
| unixexec | tokio process | async-process | std spawn, `Registered<Pipe>`, hook reaps |
| autolaunch (Windows) | win32 | win32 | win32 (no blocking involved) |

Every `Unsupported` is returned before a process is spawned or a file is opened. The check is
`runtime.spawn_blocking(..)`, which maps a `None` hook to `Error::Unsupported` with a message
naming the operation.

## Private erasure

```rust
trait ErasedReactor: Send + Sync {
    fn register(&self, source: IoSource) -> io::Result<Box<dyn ErasedRegistration>>;
    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>>;
    fn start_driver(&self, driver: Run) -> io::Result<Option<Run>>;
    fn spawn_blocking(&self, work: Box<dyn FnOnce() + Send>)
        -> Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>>;
}

trait ErasedRegistration: Send + Sync {
    fn poll_io(&self, cx: &mut Context<'_>, interest: Interest,
               operation: &mut dyn FnMut() -> io::Result<()>) -> Poll<io::Result<()>>;
}
```

A blanket `impl<R: traits::Reactor> ErasedReactor for R` boxes the registration and the sleep
future once each. `Registered::poll_io::<T>` passes a closure that stores the `T` in a local
`Option` and returns `io::Result<()>`, so each I/O poll is one virtual call and no allocation.
`spawn_blocking` stores its result in an `Arc<Mutex<Option<T>>>` captured by the erased closure.

## Feature and dependency model

The feature lists do not change: `async-io` keeps every smol crate, `tokio` keeps `tokio`, and
`comms` gains only `socket2` (PR 3). What changes is which builds are valid:

- The `compile_error!` for `comms` without a backend goes away in PR 3, once an explicit reactor
  makes such a build useful. Until then external-only builds stay rejected; the scheduler and
  locks land in PR 2 gated on `not(any(feature = "async-io", feature = "tokio"))` plus
  `cfg(test)`, so their unit tests run in every configuration.
- In an external-only build `utils::block_on` is `futures_lite::future::block_on` (futures-lite
  is already a shared dependency with `std` on); `zbus::blocking` compiles but can only build
  connections that carry an explicit reactor whose host loop runs on another thread, which the
  docs say.
- CI gains an external-only leg once PR 3 lands: `check`, `clippy` and the unit tests with
  `--no-default-features --features comms,proxy,service`, plus `cargo tree` asserting that none
  of the `async-io`-owned crates appear in that graph.
- MSRV stays 1.87.

## Documentation

- Rustdoc on `zbus::runtime` explaining the three choices (reactor, executor, driver), the
  external-only build, and which transport/authentication combinations an external reactor
  supports.
- `Connection::executor()` and `Builder::internal_executor()` docs rewritten around `run()`.
- Book: a "Runtimes" section in `connection.md` with the caller-driven example from the issue and
  a pointer to the host examples; FAQ entry updated.
- `upgrading-to-6.md`: `run()`, the external-only build, and that `comms` without a backend no
  longer fails to compile.

## Testing strategy

PR 1 (#1962): the existing suite in all feature combinations; no behaviour change.

PR 2, unit tests in `runtime/scheduler.rs` and `runtime/sync.rs`, run in every configuration:

- scheduler: spawn and join; wake from another thread reaches the driver; a task woken during
  its own poll is re-run; cancel on drop frees the future and fails the join; `detach` lets the
  task finish; `is_empty`; the batch bound keeps an always-ready task from monopolising `run`;
  `run` re-arms itself when a batch leaves externally woken tasks queued (fails by hanging
  without the re-arm, so it carries a timeout); dropping the scheduler fails pending joins; a
  panicking task propagates and is forgotten. Every test that can hang carries an `ntest`
  timeout. One `block_on` per assertion: futures-lite's `block_on` shares a parker per thread,
  so a stale unpark from an earlier call masks a missing wake in the next.
- locks: mutual exclusion under contention across threads; a release wakes a waiter; a
  cancelled `lock()` does not strand the next waiter; `RwLock` allows concurrent readers, a
  waiting writer blocks new readers and gets the lock, a cancelled writer lets readers in again;
  `Semaphore` with `n` permits admits at most `n`; guards deref to unsized targets
  (`RwLock<dyn Trait>`). Tests that cancel a future must own it (`Box::pin`): dropping a `pin!`
  binding drops only the `Pin<&mut F>`.
- `Executor`/`Task` enum dispatch through the existing suites (default, tokio-only,
  all-features).

PR 3, unit tests in `runtime/driver.rs` and `connection/mod.rs`:

- driver claim: second `Run` errors, first keeps running; unpolled drop is a no-op; claimed drop
  fails a pending call and closes the socket;
- termination observation in automatic mode; `graceful_shutdown` with retained clones and an
  active handler on a caller-driven connection;
- an end-to-end unix session connection through `Builder::reactor(runtime::Reactor)` (private
  scheduler, thread from `start_driver`), and through `internal_executor(false)` with `run()`
  driven by `async_io::block_on`;
- a `start_driver` that returns `Err` fails `build()`;
- socket wrapper: partial writes, FD passing, readiness races (a peer that writes before the
  reader registers), timer cancellation (dropping a timed-out `call_method` future);
- the external-only build compiles and its `Builder::session().build()` fails with
  `Error::Unsupported`.

PR 4, integration tests in `zbus/tests/`:

- `polling`-based single-threaded test host (dev-dependency): connect to the session bus, fetch
  peer credentials, run a method call with a timeout, shut down; assert the process thread count
  is unchanged across the whole lifecycle (Linux: `/proc/self/task`); run it in the external-only
  configuration too;
- Tokio `AsyncFd` host (`tokio` feature): same lifecycle, driver spawned on the runtime through
  `start_driver`, and the caller-driven variant with `join!`;
- unsupported ancillary operations (`tcp:host=name`, `unixexec:`) fail on a host without the hook
  before any helper starts;
- GLib example under an `examples`-only feature (`glib` dev-dependency, optional) so CI needs no
  system library.

Required checks stay: `cargo test --all-features`, `--no-default-features`, `--no-default-features
--features tokio`, and the existing cross-platform `cargo check` targets.

## Risks

- **A hand-written scheduler and locks.** Small, `unsafe`-free and unit-tested under contention,
  but new code on a hot path for external connections. Mitigated by keeping the built-in backends
  on async-executor/async-lock until #1959 proves the replacements there too.
- **Default-path regressions** from routing the built-in reactor through the wrapper. Mitigated by
  the wrapper being the same `poll_readable`/`recvmsg` loop, and by the full suite running on the
  default features in PR 3.
- **Windows unix-socket connect.** `uds_windows` has no non-blocking connect; `socket2` handles
  `AF_UNIX` on Windows, so the same connect helper applies. If it does not on some Windows version,
  the fallback is `spawn_blocking` on the reactor path, documented.
- **Abrupt stop leaves tasks un-dropped until the scheduler goes away.** Acceptable: the
  scheduler's reference count is bounded by live handles and the discarded driver, and the socket
  is closed so nothing holds kernel resources open indefinitely.
- **GAT-free `spawn_blocking` boxes its future.** Deliberate; see the API section.

## Follow-ups (out of scope)

- #1959: moving the built-in backend onto the private scheduler and locks and dropping the smol
  crates.
- Migrating the public `Async<T>` socket impls onto the wrapper.
- A thread-free `blocking` facade.
- Deprecating `internal_executor` and `Connection::executor()` once `run()` has shipped.
