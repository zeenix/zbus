# External runtime integration — design

Resolves z-galaxy/zbus#1960 ("Support external runtimes while retaining the async-io default").
The follow-up proposal in #1959 (replacing the smol-based internals of the built-in backend) is
out of scope; this design only has to leave it possible.

Three points of the issue text are deliberately not followed:

- "External connections reuse async-executor through zbus's existing Executor/Task abstractions."
  That sentence entered the issue when it was split out of #1959 and contradicts the point of the
  split: an external-runtime user must not depend on any crate the `async-io` feature owns.
- A caller-polled `Connection::run()` driver. zbus writes no scheduler and no lock of its own
  (those are #1959's, should it be pursued); a runtime that wants to drive zbus has to be able to
  spawn a task, and the runtime abstraction requires exactly that.
- The trait is named `Runtime`, not `Reactor`, because it supplies more than readiness.
- `internal_executor(bool)` is not kept. `Builder::runtime` is the one way to say who runs zbus's
  tasks, and with it `Connection::executor()`, `Executor::tick()` and the public `Executor`/`Task`
  types have no purpose. 6.0 is unreleased and the automatic case keeps its API, so nothing is
  retained for compatibility.

## The problem

A connection needs four things from a runtime: readiness wakeups for its socket, timers,
somewhere to run its tasks (the socket reader, the object server dispatcher, name-lost watchers,
spawned method handlers), and async locks that are held across `.await` (the socket's write half,
the message broadcasters, every interface behind the object server). Today all four are coupled
to two compiled-in backends:

- `async-io`: sockets are `Arc<async_io::Async<T>>`, timers are `async_io::Timer`, tasks run on an
  `async_executor::Executor` ticked by a dedicated `zbus::Connection executor` thread that the
  builder spawns after setup (`internal_executor(false)` leaves ticking to the caller through
  `Connection::executor().tick()`), locks are `async_lock`'s.
- `tokio`: sockets are `tokio::net` types, timers `tokio::time`, tasks `tokio::spawn`ed, locks
  `tokio::sync`'s.

Nothing else can supply them. A GLib, `polling`, `mio` or custom event loop host cannot use zbus
without accepting async-io's reactor thread and executor thread in its process, and a Tokio user
on an unusual configuration cannot integrate through Tokio's `AsyncFd` either. The choice is also
made per call in places (`select_runtime!`, `use_tokio()`), which the code itself documents as
safe only for call sites that are independent of the socket's reactor.

## Goals

- Let a host supply readiness, timers, task spawning, async locks and blocking work through one
  public trait, while zbus keeps creating and connecting sockets, doing authentication, framing
  and FD passing.
- An external-runtime user depends on none of the crates behind the `async-io` feature:
  `async-io`, `async-executor`, `async-task`, `async-lock`, `blocking` and their transitive
  dependencies. A build with `comms` and neither `async-io` nor `tokio` (an "external-only"
  build) compiles and works with an explicit runtime.
- zbus itself writes no scheduler and no synchronization primitive. Whatever a runtime cannot
  provide through the trait, zbus does not need.
- Keep the default behaviour: session/system connections stay automatic, `async-io` stays the
  default non-Tokio backend, the Tokio backend is an implementation of the trait like async-io.
- Add no zbus thread on the external path, at any point of the connection's lifetime, with one
  documented exception: the default `traits::Runtime::spawn_blocking` runs blocking work
  (hostname lookup for `tcp:`, reading a `nonce-tcp:` file, the supplementary-group lookup for
  peer credentials, waiting on the helper process of `unixexec:`/`ibus:`/`launchd:`) on a std
  thread that exits with the work; a host with a pool overrides it.
- Keep `Builder` and `Connection` non-generic and keep wire-only builds untouched.

## Non-goals

- Replacing async-io, async-executor or async-lock for the built-in backend (#1959). The private
  scheduler and locks prototyped for this issue live on branch `zeenix/runtime-scheduler`
  (withdrawn PR #1963) as input to #1959.
- A thread-free `zbus::blocking` facade on top of an external runtime.
- Completion-based hosts (IOCP, io_uring): they need the custom `Socket` route.

## Decisions

1. **One I/O path for every runtime, Tokio included.** `AsyncIo` and `Tokio` are both ordinary,
   crate-private implementors of the public trait, chosen by feature. The transport connect path
   goes through one private registered-socket wrapper for every runtime, built-in or external.
   Each connection latches a private runtime choice at build time; `select_runtime!` and
   `use_tokio()` are removed.
2. **The runtime supplies tasks and locks.** `traits::Runtime` has associated types for its task
   handle, mutex and readers-writer lock, and constructors for them. zbus's `Executor`/`Task` and
   its private lock wrappers become enums: a zero-cost variant per compiled backend and an erased
   variant for external runtimes. No smol crate moves out of the `async-io` feature.
3. **`spawn_blocking` always works.** It covers DNS, NSS group lookup, nonce-file reads and
   subprocess work. The default implementation runs the work on a std thread created for that
   call; `AsyncIo` and `Tokio` override it with their own pools (`blocking::unblock` and
   `Handle::spawn_blocking`).
4. **No driver, no manual ticking.** zbus spawns its tasks on the runtime and is never polled or
   ticked; there is no `Connection::run()`, no driver hand-off, no driver state, and no
   `internal_executor`. Whoever wants zbus's tasks on their own executor implements
   `Runtime::spawn`. The async-io backend's executor thread becomes an implementation detail of
   the built-in runtime.
5. **One PR.** #1964 delivers the runtime abstraction, the registered-socket wrapper, transports,
   ancillary operations, the reference host, examples and docs together; the module rename
   (#1962) is a separate, earlier PR.

## Public API

Everything below is behind the `comms` feature and lives in `zbus::runtime` unless stated.

### Module layout

```text
zbus/src/runtime/
├── mod.rs           # pub mod traits; pub use IoSource, Interest, AsyncDrop; private Runtime
├── traits.rs        # Runtime, IoRegistration, Task, Mutex, RwLock
├── async_io.rs      # crate-private async-io implementor (feature = "async-io")
├── tokio_rt.rs      # crate-private Tokio implementor (feature = "tokio")
├── tokio_lock.rs    # Mutex/RwLock over tokio::sync (feature = "tokio")
├── blocking_thread.rs  # default spawn_blocking hook: a std thread per call
├── erased.rs        # ErasedRuntime, ErasedRegistration, ErasedTask, ErasedMutex, ErasedRwLock
├── executor.rs      # private Executor, Task: enums over the compiled backends and erasure
├── locks.rs         # Mutex, RwLock and guards: enums over the compiled backends and erasure
├── io.rs            # Registered<K> socket wrapper and the connect helpers
├── timeout.rs       # timeout over the connection's runtime
├── async_drop.rs    # unchanged
└── process.rs       # helper processes: std spawn, registered pipes, spawn_blocking reap
```

`async_lock.rs` is replaced by `locks.rs`.

### Traits

```rust
pub mod traits {
    pub trait Runtime: Send + Sync + 'static {
        type Registration: IoRegistration;
        type Sleep: Future<Output = ()> + Send + 'static;
        type Task: Task;
        type Mutex<T: Send + 'static>: Mutex<T>;
        type RwLock<T: Send + Sync + 'static>: RwLock<T>;

        /// Register a socket or pipe for readiness notifications.
        fn register(&self, source: IoSource) -> io::Result<Self::Registration>;

        /// A future that completes once `duration` has passed on this runtime's own clock.
        /// Dropping it cancels the timer.
        fn sleep(&self, duration: Duration) -> Self::Sleep;

        /// Run `future` to completion in the background, starting now or soon. `name` is for
        /// diagnostics (a task list, a console) and a runtime may ignore it.
        fn spawn(&self, name: &str, future: impl Future<Output = ()> + Send + 'static)
            -> Self::Task;

        fn mutex<T: Send + 'static>(&self, value: T) -> Self::Mutex<T>;
        fn rwlock<T: Send + Sync + 'static>(&self, value: T) -> Self::RwLock<T>;

        /// Run `work` off the event loop. The default implementation spawns a std thread that
        /// runs `work` and exits once it is done; a host with a thread pool overrides it. zbus
        /// uses it for: hostname lookup for `tcp:`, reading a `nonce-tcp:` file, the
        /// supplementary-group lookup for peer credentials, reading the `autolaunch:` address
        /// on Windows (a named Win32 mutex with an unbounded wait), and waiting on the helper
        /// process of `unixexec:`/`ibus:`/`launchd:` once its pipes are closed.
        fn spawn_blocking<T>(
            &self,
            work: impl FnOnce() -> T + Send + 'static,
        ) -> Pin<Box<dyn Future<Output = T> + Send + 'static>>
        where
            T: Send + 'static,
        {
            blocking_thread::run(work)
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

    /// A handle to a spawned task. Dropping it cancels the task; `detach` lets it run on.
    pub trait Task: Future<Output = io::Result<()>> + Send + Sync + Unpin + 'static {
        fn detach(self);
    }

    pub trait Mutex<T>: Send + Sync + 'static {
        type Guard<'a>: DerefMut<Target = T> + Send
        where
            Self: 'a;

        fn lock(&self) -> impl Future<Output = Self::Guard<'_>> + Send;
    }

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
}
```

Contracts, documented on the traits:

- `poll_io`: return the first success (a partial write counts) or any error other than
  `WouldBlock` immediately; on `WouldBlock` clear the readiness observed, arrange a wakeup for
  `cx` and return `Pending`; bound retries, never spin, never block, never retain `operation`.
  Deregister the source before its `IoSource` is released. `Interest` is
  `enum Interest { Readable, Writable }`.
- `spawn`: the task runs on the host's executor, concurrently with the caller, on any thread the
  host chooses; the handle resolves to `Err` if the host loses the task. Dropping the handle
  cancels: a Tokio host wraps its `JoinHandle` in an abort-on-drop newtype (ten lines, shown in
  the example), an async-task-style host maps `detach` to its own. Tasks produce nothing: no
  caller in zbus reads a task's result, the handles it keeps are held for cancellation, and a
  unit output keeps the associated type free of a generic parameter and the erased path free of
  an output slot. The name is what zbus always passed to its executor; the Tokio backend hands it
  to `tokio::task::Builder` under `--cfg tokio_unstable`, where a tokio-console shows it.
- Locks: the guards are `Send` so that zbus can hold them across `.await` inside `Send` futures;
  a `RwLock` read guard is also `Sync`. Nothing else is required: no `try_` variants, no
  fairness, no `const` construction, because the constructors are methods on the runtime.
- `spawn_blocking` always resolves to `T`: the default spawns a dedicated std thread per call and
  joins it; `AsyncIo` and `Tokio` override it with their own thread pools. Its future is boxed;
  blocking work happens a handful of times per connection, at setup, and once more at the end of
  a connection that ran a helper process. That last wait occupies a worker for the helper's exit
  latency, whichever pool backs the hook, because it only starts once the connection has let go
  of the helper's pipes.

The `Send` bounds on the returned futures use return-position `impl Trait` in traits (Rust 1.75),
within MSRV 1.87.

### `IoSource`

```rust
#[derive(Clone, Debug)]
pub struct IoSource(Arc<Owned>);   // Owned = OwnedFd on unix, OwnedSocket on windows
```

Implements `AsFd` and `AsRawFd` (unix) or `AsSocket` and `AsRawSocket` (windows). It is a shared
owner: the socket wrapper keeps one clone for I/O and the registration keeps whatever it needs.
Users receive one from `register` and never construct one. `async_io::Async::new` accepts it
directly because `Arc<T>: AsFd` where `T: AsFd`.

### Built-in runtime

```rust
#[cfg(feature = "async-io")]
pub(crate) struct AsyncIo { .. }   // `Clone` is an `Arc` clone

#[cfg(feature = "tokio")]
pub(crate) struct Tokio { handle: tokio::runtime::Handle }
```

`AsyncIo` implements `traits::Runtime` with async-io, async-executor and async-lock: `register`
wraps the source in `async_io::Async::new` and `poll_io` is the `poll_readable`/`poll_writable`
loop built on `Arc<Async<UnixStream>>`; `sleep` is `async_io::Timer::after` behind a small future
that drops the `Instant`; `spawn` goes to an `async_executor::Executor` owned by the
`AsyncIo` instance, whose `zbus::Connection executor` thread starts on the first spawn, holds a
strong reference to the executor, ticks it under `async_io::block_on` until it is empty, exits,
and starts again on the next spawn (a running check under the same lock the spawner takes makes
the hand-off race-free); the locks are `async_lock`'s; `spawn_blocking` returns
`blocking::unblock`. A thread that instead waited for the last `AsyncIo` clone would block
forever in an idle executor once the connection is gone, and one that held only a weak reference
would cancel detached tasks.

`Tokio` captures `Handle::current()` at build time, so a connection keeps working when it is
later polled from outside a runtime context. `register` wraps the source in
`tokio::io::unix::AsyncFd` on unix; on Windows it returns `Error::Unsupported`, except that a
crate-private `tokio::net::TcpStream` socket covers TCP, and unix sockets stay unsupported.
`sleep` is `tokio::time::sleep` entered on the captured handle, so it runs on that runtime's
clock and a paused or advanced Tokio clock is honoured; `spawn` is `handle.spawn` (a
`tokio::task::Builder` carrying the name under `tokio_unstable`) behind an abort-on-drop task
newtype, the locks are `tokio::sync`'s, and `spawn_blocking` is `handle.spawn_blocking`.

Both types are reached through their own zero-cost variant of the private `Runtime` enum, so
there is one implementation of each backend, chosen by feature, not a second code path for the
default case. The `AsyncIo` executor thread is the only thread zbus ever starts, and only that
runtime starts it.

### Builder and Connection

```rust
impl Builder<'_> {
    /// Use `runtime` for an external runtime; the built-in backends are chosen by feature and
    /// cannot be named here.
    pub fn runtime(self, runtime: impl traits::Runtime) -> Self;

    /// Connect over an already-established stream, registered on this connection's runtime.
    pub fn unix_stream(self, stream: UnixStream) -> Self;  // std, or uds_windows on Windows
    pub fn tcp_stream(self, stream: std::net::TcpStream) -> Self;
    pub fn vsock_stream(self, stream: vsock::VsockStream) -> Self;
}
```

`runtime` erases the implementation into `Arc<dyn ErasedRuntime>` immediately; a later call
replaces the earlier one like any other setter. `Builder::internal_executor`,
`Connection::executor`, `Executor` and `Task` are removed from the public API (the root re-export
keeps only `AsyncDrop`). The manual-tick mode they served is expressed by implementing
`Runtime::spawn` on the executor that used to tick; the Tokio host example shows the shape.

`unix_stream`, `tcp_stream` and `vsock_stream` take an owned stream and register it on the
connection's runtime at build time. No `Socket`, `ReadHalf` or `WriteHalf` impl exists for
`Async<T>`, `tokio::net` or `tokio_vsock` types, and there is no `tokio-vsock` feature; a caller
passes a Tokio socket after `into_std()` and an `async_io::Async<T>` after `into_inner()`.
`Builder::socket`/`authenticated_socket` and the public `Socket` trait remain, for sockets that
are not file descriptors, such as `socket::Channel` or a completion-based host's own type.

## Runtime selection

At the start of `build_inner`, before any I/O or discovery, the builder picks one of:

| Builder/feature state | Runtime | Tasks | Locks |
| --- | --- | --- | --- |
| `runtime(r)` given | any `traits::Runtime` the host supplies | `r.spawn` | `r.mutex`/`r.rwlock` |
| `async-io` only | `AsyncIo` | async-executor | async-lock |
| `tokio` only, a runtime current | `Tokio` | `tokio::spawn` | `tokio::sync` |
| `tokio` only, no runtime current | `Error::Unsupported` | | |
| both compiled, a runtime current | `Tokio` | `tokio::spawn` | `tokio::sync` |
| both compiled, no runtime current | `AsyncIo` | async-executor | async-lock |
| neither compiled, no `runtime(r)` | `Error::Unsupported` | | |

The private enum is

```rust
pub(crate) enum Runtime {
    #[cfg(feature = "async-io")]
    AsyncIo(AsyncIo),
    #[cfg(feature = "tokio")]
    Tokio(Tokio),
    External(Arc<dyn ErasedRuntime>),
}
```

stored in `ConnectionInner` and passed to `Transport::connect`, the handshake, `timeout` and
every place that creates a lock or spawns.

## Internals

### Erasure

```rust
trait ErasedRuntime: Send + Sync {
    fn register(&self, source: IoSource) -> io::Result<Box<dyn ErasedRegistration>>;
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;
    fn spawn(&self, name: &str, future: Pin<Box<dyn Future<Output = ()> + Send>>)
        -> Box<dyn ErasedTask>;
    fn mutex(&self, value: Box<dyn Any + Send>) -> Box<dyn ErasedMutex>;
    fn rwlock(&self, value: Box<dyn Any + Send + Sync>) -> Box<dyn ErasedRwLock>;
    fn spawn_blocking(
        &self,
        work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
    ) -> Pin<Box<dyn Future<Output = Box<dyn Any + Send>> + Send>>;
}

trait ErasedRegistration: Send + Sync {
    fn poll_io(&self, cx: &mut Context<'_>, interest: Interest,
               operation: &mut dyn FnMut() -> io::Result<()>) -> Poll<io::Result<()>>;
}

trait ErasedTask: Future<Output = io::Result<()>> + Send + Sync + Unpin {
    fn detach(self: Box<Self>);
}

trait ErasedMutex: Send + Sync {
    fn lock(&self) -> Pin<Box<dyn Future<Output = Box<dyn ErasedGuard<dyn Any + Send>> + '_>>;
}
// ErasedRwLock: read() and write() likewise; ErasedGuard<T>: DerefMut<Target = T> + Send.
```

A blanket `impl<R: traits::Runtime> ErasedRuntime for R` boxes each registration, sleep future,
task and lock once. The generic-over-`T` operations are erased with the value type erased too:
a lock stores `Box<dyn Any + Send>` and the typed wrapper downcasts on every access, which is a
`TypeId` comparison; tasks have no output to erase. `poll_io` passes a closure that stores
the `T` in a local `Option`, so an I/O poll is one virtual call and no allocation.
`spawn_blocking` erases its output the same way a lock's value is erased: the closure returns
`Box<dyn Any + Send>`, and the typed `Runtime::spawn_blocking` downcasts once the returned future
resolves.

Cost on the external path: a spawn boxes the future and the handle, a lock acquisition boxes
the future and the guard and downcasts on each access, and a sleep or `spawn_blocking` call boxes
its future. Locks are taken once per sent message, once per received message and once per method
call; the message itself already costs more than that. On the built-in paths every operation is a
`match` on the enum, and an interface now sits as a `Box<dyn Interface>` inside its lock, one
indirection more per dispatch than the unsized `RwLock<dyn Interface>` it replaces. None of this
is measured: the benchmarks in tree cover the wire format, not a connection.

### Registrations

```rust
pub(crate) enum Registration {
    #[cfg(feature = "async-io")]
    AsyncIo(async_io::Registration),
    #[cfg(feature = "tokio")]
    Tokio(tokio_rt::Registration),
    External(Box<dyn ErasedRegistration>),
}
```

`poll_io` dispatches on the variant, mirroring `Task` and the lock enums: a zero-cost call into
the built-in backend's own registration type for `AsyncIo` and `Tokio`, or one virtual call into
the erased type for an external runtime. `Runtime::register` is what selects the variant, so
every socket, pipe or transport connect that registers a source has a runtime in hand.
`Registered<K>` (see "I/O path") wraps one `Registration` together with its `IoSource` and a
kind marker.

### Locks (`runtime::locks`)

```rust
pub(crate) enum Mutex<T> {
    #[cfg(feature = "async-io")]
    AsyncLock(async_lock::Mutex<T>),
    #[cfg(feature = "tokio")]
    Tokio(tokio::sync::Mutex<T>),
    External(Erased<T>),           // Box<dyn ErasedMutex> + PhantomData<T>
}
```

`lock()` is an `async fn` matching on the variant and returning a `MutexGuard<'_, T>` enum with
the same three shapes; `RwLock` and its two guards follow the same pattern. Locks are created
through the connection's runtime (`runtime.mutex(value)`), which is what selects the variant, so
every site that creates one has a runtime in hand:

- `Connection::new` creates the four connection locks (`socket_write`, `msg_senders`,
  `subscriptions`, `registered_names`).
- The object server wraps each interface as `Arc<RwLock<Box<dyn Interface>>>` when it is added
  (`ObjectServer::at`, which has the connection), not in `ArcInterface::new` at
  `Builder::serve_at` time: the builder stores `Box<dyn Interface>` and wraps at build. The
  `Box` is needed because an enum cannot hold an unsized `T`; `InterfaceDeref` derefs through it.

The proxy's property cache keeps its `std::sync::RwLock`; it is never held across an `.await`.

### Tasks

```rust
pub(crate) struct Task(TaskInner);

enum TaskInner {
    #[cfg(feature = "async-io")]
    AsyncExecutor(async_task::Task<()>),
    #[cfg(feature = "tokio")]
    Tokio(TokioTask),                    // abort-on-drop newtype over JoinHandle<()>
    External(Box<dyn ErasedTask>),
}
```

Spawning is `Runtime::spawn(&self, name, future) -> Task`, dispatching on the variant. There
is no public `Executor` type, no lifetime parameter, `tick`, `is_empty`, `run` or
`needs_internal_driver`, and no `start_internal_executor` in the builder: the `AsyncIo` runtime
owns its executor and thread. `build()` awaits its own setup steps like any other future, and the
runtime runs what it spawns. Blocking work goes through `Runtime::spawn_blocking`, dispatching to
`Tokio`'s pool, `blocking::unblock`, or, for an external runtime, whatever it implements — the
default std thread if it implements nothing. `Task` keeps its drop-cancels/`detach` semantics for
the crate's own use.

### Timers

`runtime::timeout(runtime, fut, duration)` races `fut` against `runtime.sleep(duration)`,
dispatching on the `Runtime` enum's variant like every other operation; no call site reaches
`tokio::time::sleep` or `async_io::Timer` directly. The duration, not a deadline, crosses the
trait: each backend's timer keeps its own clock, and Tokio's can be paused or advanced by a
test, so a deadline taken from `std::time::Instant` would already have passed there.
`Connection::call_method` passes `self.inner.runtime`.

### Driving

There is none. Every internal task is spawned on the runtime and the host's executor runs it;
`build()` awaits its own setup steps and spawns the socket reader and object-server dispatcher.
The `AsyncIo` executor thread starts on the first spawn, which happens during setup, and exits
once the executor is empty. `graceful_shutdown(self)` means "wait for `ConnectionInner` to be
destroyed": handlers, clones, streams and proxies keep their strong references, permanent
service loops stay weak, and nothing is force-closed. The socket reader finishes the
connection off in `Drop`, so it does not matter how its task ends — its own read error, a host
aborting or dropping the task, or the future never being polled: the connection reports closed,
pending calls fail and message streams end (a stream watches the closed event alongside its
receiver).

## I/O path

### Registered sockets

```rust
pub(crate) struct Registered<K> {
    registration: Registration,   // drops before `source`
    source: IoSource,
    runtime: Runtime,
    kind: K,
}
```

`K` selects the syscalls: `Unix` (recvmsg/sendmsg with SCM_RIGHTS via the existing
`fd_recvmsg`/`fd_sendmsg` helpers, peer credentials), `Tcp` and `Vsock` (recv/send, no FDs), and
`Pipe` (read/write, for a helper process's stdio). `ReadHalf` and `WriteHalf` are implemented on
`Arc<Registered<K>>`, mirroring `Arc<Async<T>>`'s impls, with `read_with`/`write_with` helpers
built on `poll_fn` + `poll_io`. `Builder::socket`/`authenticated_socket` and the public `Socket`
trait remain the route for sockets that are not file descriptors; nothing implements `Socket`
for `Async<T>` or a Tokio type.

### Connecting

```rust
pub(crate) async fn connect(
    runtime: &Runtime,
    domain: socket2::Domain,
    ty: socket2::Type,
    addr: &socket2::SockAddr,
) -> io::Result<IoSource>;
```

Transports build the socket with `socket2` (already in the lock through Tokio; added to `comms`)
as non-blocking and close-on-exec, then call `connect`. It either succeeds or pends with
`EINPROGRESS`; a pending connect registers the source, waits for writable readiness and then
checks `take_error()` (on unix `getpeername` too, since a runtime that tries the operation before
looking at readiness would otherwise take a half-connected socket for a connected one; on Windows
`getpeername` succeeds as soon as a connect is issued, so one readiness report is waited for and
`SO_ERROR` alone decides). A unix listener whose backlog is full turns a non-blocking connect
away with `EAGAIN` rather than queueing it, where a blocking `connect(2)` would wait in the
kernel for room; the helper waits on the runtime's timer instead and tries again with a fresh
socket, for as long as the caller keeps waiting, so the caller's own timeout or cancellation
bounds the wait as it bounds the handshake after it. This one helper serves unix, tcp and vsock
addresses alike (`SockAddr::vsock` on Linux). Peer credentials on the runtime path call
`SO_PEERCRED` / `getpeereid` / `SO_PEERPIDFD` directly; the supplementary-group lookup
(`getpwuid_r`, `getgrouplist`) goes through `spawn_blocking`.

On Windows, `AsyncIo` and an external runtime connect unix sockets over `socket2`'s `AF_UNIX`
support the same way; a connection on the built-in `Tokio` runtime falls back to a crate-private
`tokio::net::TcpStream` socket for `tcp:` and returns `Error::Unsupported` for unix sockets,
matching Tokio's own lack of `AsyncFd` on that platform.

### Ancillary operations

| Operation | Implementation |
| --- | --- |
| DNS for tcp hostnames | `spawn_blocking(\|\| (host, port).to_socket_addrs())` |
| nonce-tcp file | `spawn_blocking(\|\| std::fs::read(path))` |
| ibus/launchd `output()` | std process; stdout read to EOF via its registration, then reaped |
| unixexec | std process; pipes as `Registered<Pipe>`; reaped once both are closed |
| autolaunch (Windows) | `spawn_blocking(autolaunch_bus_address)`: a named mutex, unbounded wait |

Every process, on every runtime, is spawned with `std::process::Command`; its pipes are
registered like any other socket and its exit status is collected through `spawn_blocking`,
which always has an implementation. The wait starts when the transport is done with the helper,
not when the helper starts: `output()` reads to EOF first, and for `unixexec:` the read half's
drop starts it, with the reaper the two halves share running it as a fallback when the last of
them drops. Nothing awaits the wait; the hook's contract that the work runs whether or not its
future is kept is what leaves it with the pool. So a healthy connection occupies no blocking
worker; a helper that exited on its own is reaped at once, even while an idle `Connection` clone
still holds its input; and a connection that stops reading a helper still running parks a worker
until that helper sees its input close, which is when the last clone drops the other pipe.
`Connection::close()` closes the helper's standard input (the write half drops its pipe, and a later
write reports `NotConnected`), which is what tells a `unixexec:` helper to exit. `async-process` is
not part of the `async-io` feature and `process` is not part of the `tokio` feature: no backend
depends on either crate for subprocess handling.

## Feature and dependency model

`async-io` keeps every smol crate it already carries except `async-process`; `tokio` keeps
`tokio` without its `process` feature; `comms` gains `socket2`. There is no `compile_error!` for
`comms` without a backend: such a build's `Builder::session()` and friends fail with
`Error::Unsupported` unless `runtime(..)` was called; `utils::block_on` is
`futures_lite::future::block_on` (futures-lite is already a shared dependency with `std` on), so
`zbus::blocking` compiles and works when the host loop runs on another thread. The `vsock`
feature depends only on the `vsock` crate, not on `async-io`, and there is no `tokio-vsock`
feature.

- CI runs an external-only leg (`check`, `clippy` and `--tests` with `--no-default-features
  --features comms,proxy,service`), plus `cargo tree -e normal` asserting that none of the
  `async-io`-owned crates appear in that graph. Dev-dependencies (async-lock for the test
  runtime, tokio for the Tokio host test, `polling` for the reference host) are allowed; they
  never reach users.
- The Windows CI matrix adds a tokio-only leg (`--no-default-features --features
  tokio,proxy,service`) alongside the default and external-only legs.
- MSRV stays 1.87.

## Documentation

- Rustdoc on `zbus::runtime` explaining what a runtime supplies, the external-only build, and
  which transport/authentication combinations an external runtime supports; a Tokio host example
  showing zbus driven from Tokio's scheduler.
- Book: a "Runtimes" section in `connection.md` with a caller-supplied runtime example and a
  pointer to `zbus/tests/polling_host/host.rs`, the reference host; the FAQ's Tokio entry updated
  to match.
- `upgrading-to-6.md`: `Builder::runtime` replaces `internal_executor`; `Connection::executor`,
  `Executor` and `Task` are gone; `unix_stream`/`tcp_stream`/`vsock_stream` replace the
  async-io- and Tokio-typed constructors, taking an owned stream (`into_std()` for a Tokio
  socket, `into_inner()` for an `async_io::Async<T>`); `vsock` depends only on the `vsock` crate,
  not `async-io`, and there is no `tokio-vsock` feature; `unixexec` runs over a std process; a
  build with `comms` and neither backend compiles and returns `Error::Unsupported` without an
  explicit runtime.

## Testing strategy

The existing suite runs in every feature combination, unmodified in behaviour. Added coverage:

- A `cfg(test)` test runtime (`zbus/src/runtime/test_runtime.rs`, dev-dependencies only):
  `spawn` on std threads running `futures_lite::future::block_on`, `sleep` on a thread
  timer, `async-lock` locks, and the trait's default `spawn_blocking`; a p2p `socket::Channel`
  pair with one end on it covers method calls both ways, a served interface, property access and
  `graceful_shutdown` with retained clones.
- Erasure: a spawned task runs, cancel on drop, `detach`; a lock guard held across an
  `.await` in a `Send` future; a downcast that never fails, checked with a debug assertion.
- A no-backend `Builder::session().build()` resolves to `Error::Unsupported`.
- Socket wrapper tests: a partial write reported as success, FD passing over a unix socket, a
  readiness race (a peer that writes before the reader registers), timer cancellation (dropping a
  timed-out `call_method` future), and a pending connect that resolves on writable readiness or
  reports the socket's error.
- A session connection over an external runtime, in every feature configuration including the
  external-only build.
- DNS resolution and `nonce-tcp:` file reads running through `spawn_blocking`, proven with a test
  runtime whose hook panics on a literal address that should skip resolution entirely.
- A helper process (`unixexec`, and `ibus`/`launchd`'s `output()`) reaped through
  `spawn_blocking`, asserting a clean exit status, and a `unixexec:` helper's reap starting only
  once its pipes are closed, with the default hook's thread gone afterwards.
- A host that aborts the socket reader task: `Connection::closed()` resolves, a pending method
  call fails and a `MessageStream` ends.
- A method timeout under Tokio's paused clock expiring after its duration of Tokio time, not at
  once.
- The reference host releasing every task, registration and timer when it is dropped after its
  connections are gone, checked through a weak probe on its shared state.
- The `polling`-based reference host (`zbus/tests/polling_host/host.rs`) running a full
  connection lifecycle — session connect, an interface served, a method call under a timeout,
  `graceful_shutdown` — with the process's thread count unchanged before and after (Linux:
  `/proc/self/task`), in both the default and external-only configurations.
- The default `spawn_blocking` hook's thread exiting once its work is done, proven the same way.
- A `Tokio`-backed connection, built inside a runtime context, staying usable when polled from a
  plain thread outside one.

Required checks: `cargo test --all-features -- --skip fdpass_systemd`, `--no-default-features`,
`--no-default-features --features tokio,proxy,service --tests`, the external-only leg's
`--tests`, and the existing cross-platform `cargo check` targets.

## Risks

- **Trait surface.** Five traits, two of them with GATs for the lock guards, is more to
  implement than a reactor alone. Mitigated
  by the `polling`-based reference host in tree, under 200 lines, and by `AsyncIo` and `Tokio`
  being the same implementors the default path runs.
- **Erased lock cost** on the external path: two allocations per acquisition, not measured (no
  benchmark in tree exercises a connection). If it shows, the guard box can go by giving
  `ErasedMutex` a `poll_lock`/`unlock` pair in a later change without touching the public trait.
- **Default-path regressions** from routing the built-in runtime through the wrapper. Mitigated by
  the wrapper being the same `poll_readable`/`recvmsg` loop, and by the full suite running on the
  default features.
- **Windows unix-socket connect.** `uds_windows` has no non-blocking connect; `socket2` handles
  `AF_UNIX` on Windows, so the same connect helper applies. If it does not on some Windows version,
  the fallback is `spawn_blocking` on the runtime path, documented.
- **GAT-free `spawn_blocking` boxes its future.** Deliberate; see the API section.

## Follow-ups (out of scope)

- #1959: replacing the smol crates behind `AsyncIo`; the withdrawn #1963 branch is its seed.
- A thread-free `blocking` facade.
