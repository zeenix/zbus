# External Runtime Implementation Plan (single PR for #1960)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One PR (#1964, branch `runtime-abstraction`) delivers the whole of #1960: a
connection takes readiness, timers, tasks, locks and blocking work from one runtime, chosen by
cargo feature for the built-in backends or supplied through `Builder::runtime` by an external
host, and an external-runtime build depends on no crate the `async-io` feature owns.

**Architecture:** `zbus::runtime::traits::Runtime` is the only contract. Both built-in backends
(`AsyncIo`, `Tokio`) are crate-private implementors of it; a crate-private `Runtime` enum with a
zero-cost variant per compiled backend plus an erased `External` variant is what a connection
stores. One private registered-socket wrapper (`Registered<K>`) does every socket's I/O through
`IoRegistration::poll_io`, so transports, FD passing, authentication and peer credentials have one
implementation. Blocking work (DNS, nonce files, NSS group lookups, process reaping) goes through
the runtime's `spawn_blocking`, whose default implementation runs the work on a thread of its own
when the host has no pool; hosts with a pool override it. Processes (`unixexec`, `ibus`,
`launchd`) are spawned with std and their pipes registered like sockets.

**Tech stack:** Rust 1.87 (MSRV), `socket2` (new, under `comms`), `rustix`/`libc` (existing),
`async-io`/`async-executor`/`async-lock`/`blocking` behind `async-io`, `tokio` behind `tokio`;
dev-dependencies add `polling` for the reference host.

**Spec:** `docs/superpowers/specs/2026-09-12-external-runtime-design.md` (branch
`external-runtime-spec`, PR #1961), amended by this plan as listed under "Spec amendments".

**Starting point:** branch `runtime-abstraction` at 631bfff6, five commits on `origin/main`
(ef30879c): traits; runtime value; `Builder::runtime`; CI leg; book. Tasks 1 and 6-7 amend those
commits with fixups; Tasks 2-5 add commits. Final history is nine commits (see Task 8).

## Global Constraints

- A build with `--no-default-features` and the features
  `blocking-api,proxy,service,object-manager,unixexec,ibus,tracing,p2p`
  ("EXT") lists none of async-io, async-executor, async-task, async-lock, async-process, blocking,
  polling, async-channel, async-signal, piper, tokio in `cargo tree -e normal`.
- Those crates are named only inside `#[cfg(feature = "async-io")]` (or `feature = "tokio"` for
  tokio) code, or in `cfg(test)` code over dev-dependencies.
- zbus writes no scheduler and no lock. It never drives, ticks or polls a runtime; every task it
  needs is spawned through `traits::Runtime::spawn`.
- The external path adds no zbus thread for readiness, timers or task progress at any point of
  a connection's life, including `build()` and teardown. The one documented exception is the
  default `traits::Runtime::spawn_blocking`, which runs a blocking operation (hostname lookup for
  `tcp:`, reading a `nonce-tcp:` file, the supplementary-group lookup for peer credentials,
  waiting on a helper process for `unixexec:`/`ibus:`/`launchd:`) on a std thread that exits with
  the operation; a host with a pool overrides it. Proof: the polling host test asserts an
  unchanged thread count over a lifecycle that needs none of those operations, and a second test
  shows the default hook's thread is gone once its work is done.
- Built-in backends are chosen by feature only: `async-io` alone → `AsyncIo`; `tokio` alone →
  `Tokio` when a Tokio runtime is current, else `Error::Unsupported`; both → `Tokio` when current,
  else `AsyncIo`; neither → `Builder::runtime` is required, else `Error::Unsupported`. `AsyncIo` and
  `Tokio` are crate-private; `Builder::runtime` is for external runtimes.
- Public API after the PR: `zbus::runtime::traits::{Runtime, IoRegistration, Task, Mutex, RwLock}`,
  `zbus::runtime::{IoSource, Interest, AsyncDrop}`, `connection::Builder::runtime` (+ blocking
  mirror), `Builder::{unix_stream, tcp_stream, vsock_stream}` taking owned std/`vsock` types
  (registered on the connection's runtime), `Builder::socket`/`authenticated_socket` for custom
  `Socket` impls (`Channel` and user types). Removed: `Builder::async_io_*` and
  `Builder::tokio_*` constructors, every `Socket`/`ReadHalf`/`WriteHalf` impl for `Async<T>` and
  the tokio/tokio-vsock types, the `tokio-vsock` feature, `internal_executor`,
  `Connection::executor`, `zbus::Executor`, `zbus::Task`. A tokio socket is passed after
  `into_std()`, an `Async<T>` after `into_inner()`.
- `poll_io` contract (issue text, verbatim in the trait docs): run the operation; return the
  first success (a partial write included) or any error other than `WouldBlock` immediately; on
  `WouldBlock` keep newer readiness events, arrange a wakeup and return `Pending`; bound retries;
  never spin, block the host thread or retain the callback. A pending connect waits for writable
  readiness before checking `SO_ERROR`. A registration is dropped before the source it watches.
- No `unsafe` beyond the existing FFI in `unix.rs`/`win32.rs` (each with a `// SAFETY:` comment),
  no `#[allow(...)]`, no atomics where a lock does, bounds in `where` clauses, pub before
  pub(crate) before private, usage before definition, no `test_` prefix, lines ≤ 100 chars, doc
  sentences end with a period and describe the code (no history, no future), no internal type
  names in user docs. Every hang-capable test has `#[ntest::timeout(15000)]`; no fixed-sleep
  assertions.
- Commits: gimoji prefix copied verbatim, `zb:`/`book:` scope, header ≤ 72 code points (♻️ counts
  two), body lines ≤ 74, trailers exactly `Assisted-by: Claude Fable 5.1 (claude-fable-5-1)` then
  `Claude-Session: https://claude.ai/code/session_01BZG5dKScVZhnycsSGwHUXZ`, no `Co-Authored-By`.
  Every git command that creates a commit runs with `-c core.hooksPath=/dev/null --no-verify
  --no-gpg-sign`. Fixups land with `git commit --fixup=<sha>` then
  `git -c sequence.editor=true rebase --autosquash origin/main`.

---

## Design deltas against the current branch

1. **Built-ins through the trait.** `AsyncIo` becomes `pub(crate)`. A new `pub(crate) struct Tokio
   { handle: tokio::runtime::Handle }` implements `traits::Runtime`: `register` →
   `tokio::io::unix::AsyncFd<IoSource>` (unix) / `io::ErrorKind::Unsupported` (windows);
   `sleep_until` → `tokio::time::sleep_until`; `spawn` → `handle.spawn` in the abort-on-drop
   `TokioTask`; locks → `tokio::sync`; `spawn_blocking` → `handle.spawn_blocking`. The enum becomes
   `Runtime { AsyncIo(AsyncIo), Tokio(Tokio), External(Arc<dyn ErasedRuntime>) }` and every arm
   calls the trait method on the concrete type. `Runtime::from_external` loses its downcast.
   `timeout.rs` goes through `sleep_until` for all variants. `Tokio` captures
   `Handle::current()` at build time, so a connection keeps working when later polled outside a
   runtime context (today it panics in `tokio::spawn`/`tokio::time::sleep`).
2. **One socket wrapper.** `runtime/io.rs`: `Registered<K>` = `IoSource` + a `Registration` enum
   (`AsyncIo(async_io::Registration)`, `Tokio(tokio_rt::Registration)`, `External(Box<dyn
   ErasedRegistration>)`) + a kind. `ReadHalf`/`WriteHalf` on `Arc<Registered<K>>`. Kinds:
   `Unix` (recvmsg/sendmsg with SCM_RIGHTS, peer credentials), `Tcp`, `Vsock`, `Pipe` (one fd,
   read or write). Every `Socket`/`ReadHalf`/`WriteHalf` impl for `Async<T>`, `tokio::net::*` and
   `tokio_vsock::*` goes; the `Socket` trait stays for sockets that are not file descriptors
   (`Channel`, user types). One crate-private `TokioTcp` over `tokio::net::TcpStream` remains under
   `#[cfg(all(windows, feature = "tokio"))]` for the Windows fallback in delta 3.
3. **Connecting on the runtime.** Transports build sockets with `socket2` (non-blocking,
   close-on-exec), call `connect`, and on `EINPROGRESS`/`WouldBlock` register and wait for
   writable readiness, then `take_error()`. `Transport::connect(self, address, runtime: &Runtime)`.
   Windows: unix sockets over `socket2` `AF_UNIX` on `AsyncIo`/`External`; on `Tokio` TCP falls back
   to `TokioTcp` and unix sockets stay `Unsupported`, as today. `select_runtime!`, `use_tokio`,
   `tcp_async_to_split`, `unix_stream_to_*` go.
4. **Blocking work through `spawn_blocking`.** The trait method loses its `Option`:
   `fn spawn_blocking<T>(&self, work: impl FnOnce() -> T + Send + 'static) -> Pin<Box<dyn
   Future<Output = T> + Send + 'static>>` with a default body that spawns a std thread named
   `zbus blocking work`, runs `work` there, and hands the result back through an
   `event_listener::Event` plus a `std::sync::Mutex<Option<T>>` (both already dependencies of
   `comms`); the future resolves once the result is stored and the thread exits. `AsyncIo`
   overrides it with `blocking::unblock`, `Tokio` with `Handle::spawn_blocking`. Its doc lists
   every zbus use verbatim: hostname lookup for `tcp:`, reading a `nonce-tcp:` file, the
   supplementary-group lookup behind peer credentials, waiting on the helper process of
   `unixexec:`, `ibus:` and `launchd:`. The enum's `spawn_blocking(&self, work, name) -> T`
   dispatches to the three variants; `close`/`shutdown(2)` run inline (non-blocking sockets);
   Windows credential lookups are inline syscalls.
5. **Processes with std.** `runtime/process.rs` spawns with `std::process::Command` (stdin/stdout
   piped, pipes set non-blocking, `CLOEXEC`), wraps the pipes as `Registered<Pipe>`, and reaps
   through `spawn_blocking(child.wait())` in a detached task spawned at the same time. `output()`
   for `ibus`/`launchd` reads stdout through the registration to EOF then awaits the reaper.
   `async-process` leaves the `async-io` feature list and `process` leaves the tokio feature list;
   `socket/command.rs` goes.
6. **Builder constructors.** `Builder::unix_stream(std::os::unix::net::UnixStream)` (unix) /
   `Builder::unix_stream(uds_windows::UnixStream)` (windows),
   `Builder::tcp_stream(std::net::TcpStream)`,
   `Builder::vsock_stream(vsock::VsockStream)` replace both the `async_io_*` and the `tokio_*`
   constructors and register the stream on the connection's runtime at build; the stream is set
   non-blocking there. The `vsock` feature no longer implies `async-io`; `tokio-vsock` is removed.
7. **Reference host.** `zbus/tests/polling_host.rs` (dev-dependency `polling`): a single-threaded
   host with a `polling::Poller`, a run loop that polls its spawned futures, `async-lock` locks
   (dev-dependency), timers via the poller's timeout, and the trait's default `spawn_blocking`.
   It connects to the session bus over a unix socket, serves an interface, calls a method with a
   timeout, and shuts down, asserting on Linux that `/proc/self/task` has the same entry count
   before and after. It runs in the external-only CI leg.

## Spec amendments (Task 7 writes them on `external-runtime-spec`)

- Non-goals: remove "Routing native Tokio through the trait"; Decision 1 becomes "one I/O path for
  every runtime, Tokio included"; Decision 5 becomes "one PR".
- Public API: `AsyncIo` is not public; `Builder::runtime` is for external runtimes; the
  `unix_stream`/`tcp_stream`/`vsock_stream` constructors replace both backend-typed families and
  the backend-typed `Socket` impls go; `tokio-vsock` goes; the Windows+Tokio TCP fallback keeps a
  crate-private `tokio::net::TcpStream` socket for that one case.
- Built-in runtime section: add `Tokio`; `register` semantics per platform.
- Ancillary table: one implementation per operation over `spawn_blocking`, which always works
  (default: a std thread per call); processes via std for every runtime; drop `async-process`.
  Goal "no zbus thread on the external path" gains the documented `spawn_blocking` exception.
- Testing strategy: polling host as the reference host; GLib example dropped (not in #1960).
- Follow-ups: keep #1959 and the `blocking` facade.

---

### Task 1: Built-in backends through the trait (fixups into commits 1-3 and 5)

**Files:**
- Modify: `zbus/src/runtime/{mod,traits,async_io,executor,locks,timeout,erased,tests}.rs`,
  `zbus/src/connection/builder.rs`, `zbus/src/blocking/connection/builder.rs`,
  `book/src/upgrading-to-6.md`, `zbus/README.md`
- Create: `zbus/src/runtime/tokio_rt.rs` (`#[cfg(feature = "tokio")]`),
  `zbus/src/runtime/tokio_lock.rs`
  (`traits::Mutex`/`RwLock` impls for `tokio::sync` types, `#[cfg(feature = "tokio")]`)

**Interfaces produced:**
```rust
// runtime/traits.rs (commit 1): spawn_blocking without Option, with the default body
fn spawn_blocking<T>(
    &self,
    work: impl FnOnce() -> T + Send + 'static,
) -> Pin<Box<dyn Future<Output = T> + Send + 'static>>
where
    T: Send + 'static,
{
    blocking_thread::run(work)   // private helper in runtime/blocking_thread.rs: std thread +
                                 // event_listener::Event + std::sync::Mutex<Option<T>>
}

// runtime/tokio_rt.rs
pub(crate) struct Tokio { handle: tokio::runtime::Handle }
impl Tokio {
    /// The runtime current on this thread, or `None` outside one.
    pub(crate) fn current() -> Option<Self>;
}
impl traits::Runtime for Tokio {
    type Registration = Registration;   // unix: AsyncFd<IoSource>; windows: a struct whose
                                        // `register` never constructs it (Unsupported)
    type Sleep = tokio::time::Sleep;
    type Task<T> = TokioTask<T>;        // moved here from executor.rs
    type Mutex<T> = tokio::sync::Mutex<T>;
    type RwLock<T> = tokio::sync::RwLock<T>;
    // unix: AsyncFd::new under handle.enter()
    fn register(&self, source: IoSource) -> io::Result<Registration>;
    fn sleep_until(&self, deadline: Instant) -> Self::Sleep; // sleep_until(deadline.into())
    fn spawn<T>(&self, fut) -> TokioTask<T> { TokioTask::new(self.handle.spawn(fut)) }
    fn mutex / rwlock -> tokio::sync::*::new(value)
    fn spawn_blocking<T>(&self, work) -> Pin<Box<dyn Future<Output = T> + Send + 'static>> {
        let task = self.handle.spawn_blocking(work);
        Box::pin(async move { task.await.expect("blocking work neither panics nor is cancelled") })
    }
}
```
`Registration::poll_io` (unix):
```rust
loop {
    let mut guard = match interest {
        Interest::Readable => ready!(self.0.poll_read_ready(cx))?,
        Interest::Writable => ready!(self.0.poll_write_ready(cx))?,
    };
    match operation() {
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => guard.clear_ready(),
        result => return Poll::Ready(result),
    }
}
```
`Runtime` enum: `AsyncIo(AsyncIo)`, `Tokio(Tokio)`, `External(Arc<dyn ErasedRuntime>)`;
`default_for_build`: `Tokio::current().map(Self::Tokio)` first when `tokio` is on, then `AsyncIo`,
then `Err(Error::Unsupported)`. `from_external` is `Self::External(Arc::new(runtime))` only.
`executor::TaskInner::Tokio(tokio_rt::TokioTask<T>)`; `locks::Mutex::Tokio(tokio::sync::Mutex<T>)`
constructed through `traits::Runtime::mutex(runtime, value)`. `timeout.rs`: one `sleep` over
`traits::Runtime::sleep_until` per built-in arm and the erased `sleep_until` for `External`; no
`tokio::time::sleep`/`Timer::after` calls remain. `ErasedRuntime` gains `spawn_blocking` (boxed
future of `Box<dyn Any + Send>`, downcast by the enum). The enum gains
`pub(crate) async fn spawn_blocking<T>(&self, work, name: &str) -> T`.

- [ ] Step 1: `traits.rs`: drop the `Option`, add `blocking_thread.rs` and the doc listing the
  uses; `AsyncIo::spawn_blocking` returns `Box::pin(blocking::unblock(work))`.
- [ ] Step 2: create `tokio_rt.rs` and `tokio_lock.rs`; move `TokioTask` there.
- [ ] Step 3: change the enum, `default_for_build`, `from_external`, `spawn`/`mutex`/`rwlock`,
  `spawn_blocking`, `timeout.rs`; make `AsyncIo` `pub(crate)` and remove
  `pub use async_io::AsyncIo`.
- [ ] Step 4: tests. Rename `an_explicit_runtime_beats_tokio_detection` to assert only
  `Runtime::External(_)` with `TestRuntime`. Add in `runtime/tests.rs`:
  `the_default_blocking_hook_runs_the_work_on_a_short_lived_thread` (a `cfg(test)` runtime
  keeping the trait default; `spawn_blocking(|| thread::current().name().map(String::from))`
  resolves to `Some("zbus blocking work")`; on linux `/proc/self/task` returns to the starting
  count, polled until true under the test timeout); `#[cfg(feature = "tokio")]`
  `a_tokio_connection_keeps_working_outside_the_runtime_context` (build a p2p `Channel` pair
  connection inside `rt.block_on`, then from a plain thread `futures_lite::future::block_on` a
  method call on it). Keep every other test.
- [ ] Step 5: docs: `runtime/mod.rs` and `traits.rs` module docs describe both built-ins as
  implementations; `Builder::runtime` doc drops the async-io override sentence; book/README lose
  the "force async-io inside Tokio" text.
- [ ] Step 6: verification: `cargo +nightly fmt --all`; clippy `-D warnings` on default,
  `--no-default-features --features async-io`, `tokio,p2p,proxy,service`, EXT, `--all-features`
  (all `--all-targets`); docs on all-features, tokio-only, EXT;
  `dbus-run-session -- cargo test -p zbus`
  default, `--no-default-features --features tokio,proxy,service --tests`, EXT `--lib`,
  `--all-features -- --skip fdpass_systemd`.
- [ ] Step 7: fixups: trait change into commit 1; enum/Tokio/timeout/docs into commit 2
  (`♻️ zb: Take a connection's tasks, locks and timers from one runtime`); `from_external`/test
  changes into commit 3; book/README into commit 5. Amend commit 2's body to mention the Tokio
  implementor and the captured handle.

### Task 2: The registered socket wrapper (new commit)

**Commit:** `✨ zb: Do every socket's I/O through the connection's runtime`

**Files:**
- Create: `zbus/src/runtime/io.rs` (`Registered<K>`, `Registration`, kinds, halves),
  `zbus/src/runtime/io/unix.rs` (`Unix` kind: recvmsg/sendmsg/credentials), `io/tcp.rs`,
  `io/vsock.rs`, `io/pipe.rs`, `io/connect.rs` (non-blocking connect helper), `io/tests.rs`
- Modify: `zbus/src/runtime/mod.rs` (`Runtime::register`), `zbus/src/runtime/erased.rs`
  (`ErasedRegistration`, `register` mirror), `zbus/src/runtime/io_source.rs` (`IoSource::new`
  un-gated; `From<OwnedFd>`/`From<OwnedSocket>`; `IoSource::from_socket(socket2::Socket)`),
  `zbus/Cargo.toml` (`socket2` under `comms`), `Cargo.toml` (workspace pin via `cargo add`)

**Interfaces produced:**
```rust
pub(crate) enum Registration {
    #[cfg(feature = "async-io")] AsyncIo(async_io::Registration),
    #[cfg(feature = "tokio")]    Tokio(tokio_rt::Registration),
    External(Box<dyn ErasedRegistration>),
}
impl Registration {
    pub(crate) fn poll_io<T>(&self, cx, interest, op: impl FnMut() -> io::Result<T>)
        -> Poll<io::Result<T>>;
    // poll_fn over poll_io
    pub(crate) async fn io<T>(&self, interest, op: impl FnMut() -> io::Result<T>) -> io::Result<T>;
}
impl Runtime {
    pub(crate) fn register(&self, source: IoSource) -> io::Result<Registration>;
}
pub(crate) struct Registered<K> {
    registration: Registration, source: IoSource, runtime: Runtime, kind: K,
}
// field order: registration is declared first, so it drops before the source
impl<K: Kind> Registered<K> {
    pub(crate) fn new(runtime: &Runtime, source: IoSource, kind: K) -> io::Result<Self>;
    pub(crate) async fn read_with<T>(&self, op) -> io::Result<T>;   // io(Readable, op)
    pub(crate) async fn write_with<T>(&self, op) -> io::Result<T>;  // io(Writable, op)
}
pub(crate) trait Kind: Send + Sync + 'static {
    fn recv(&self, source: &IoSource, buf: &mut [u8]) -> RecvmsgResult;
    fn send(&self, source: &IoSource, buf: &[u8], #[cfg(unix)] fds: &[BorrowedFd<'_>])
        -> io::Result<usize>;
    fn can_pass_unix_fd(&self) -> bool { false }
    fn auth_mechanism(&self) -> AuthMechanism { AuthMechanism::External }
    fn peer_credentials(&self, source: &IoSource, runtime: &Runtime)
        -> impl Future<Output = io::Result<ConnectionCredentials>> + Send;
    // socket2 shutdown(Both); no-op for pipes
    fn shutdown(&self, source: &IoSource) -> io::Result<()>;
}
pub(crate) struct Unix; pub(crate) struct Tcp; pub(crate) struct Vsock; pub(crate) struct Pipe;
impl<K: Kind> ReadHalf for Arc<Registered<K>> {
    // recvmsg = read_with(|| kind.recv(..)); can_pass_unix_fd; peer_credentials; auth_mechanism
}
impl<K: Kind> WriteHalf for Arc<Registered<K>> {
    // sendmsg = write_with(|| kind.send(..)); close = kind.shutdown(..);
    // send_zero_byte on freebsd/dragonfly for Unix
}
impl<K: Kind> Socket for Registered<K> { split = Arc twice }
// io/connect.rs
pub(crate) async fn connect(runtime: &Runtime, domain: socket2::Domain, ty: socket2::Type,
                            addr: &socket2::SockAddr) -> io::Result<IoSource>
```
`connect`: `Socket::new(domain, ty.nonblocking().cloexec(), None)`, `socket.connect(addr)`; on
`Ok` return; on `EINPROGRESS`/`WouldBlock` (`raw_os_error() == Some(libc::EINPROGRESS)` on unix,
`WSAEWOULDBLOCK` on windows) register the source, `io(Writable, || match socket.take_error()? {
Some(e) => Err(e), None => Ok(()) })`, then drop the temporary registration and return the
source; any other error returns. The `Unix` kind's `recv`/`send` are the existing
`fd_recvmsg`/`fd_sendmsg` moved from `socket/unix.rs`; `peer_credentials` is the existing
`get_unix_peer_creds_blocking` split in two: the syscalls run inline, the `getpwuid_r`/
`getgrouplist` lookup runs through `runtime.spawn_blocking(..)`. `Tcp` on windows looks up
credentials through the existing `win32::socket_addr_get_pid` inline; on unix `auth_mechanism`
is `Anonymous`. `Vsock`: recv/send, `Anonymous`. `Pipe`: `rustix::io::read`/`write`, `Anonymous`,
`shutdown` is a no-op.

- [ ] Step 1: `cargo add socket2` to the workspace and `zbus` (optional, under `comms`).
- [ ] Step 2: write `io_source.rs` constructors, `erased.rs` mirror, `Runtime::register`,
  `Registration`.
- [ ] Step 3: write `io.rs` and the kinds; nothing calls them yet except tests.
- [ ] Step 4: tests in `io/tests.rs`, each under every runtime variant the build has (a helper
  `fn runtimes() -> Vec<Runtime>` yielding `AsyncIo`, `Tokio` inside a runtime,
  `External(TestRuntime)`):
  - `a_partial_write_is_reported_as_success`: a unix socketpair with a 1 KiB send buffer
    (`SO_SNDBUF`), `sendmsg` of 64 KiB returns `Ok(n)` with `n < 65536`.
  - `file_descriptors_cross_a_unix_socket`: send a pipe fd with `sendmsg`; `recvmsg` returns it
    and reading through it yields the written bytes.
  - `readiness_written_before_registration_is_seen`: the peer writes before `Registered::new`;
    the first `recvmsg` returns the bytes without hanging.
  - `a_pending_connect_resolves_on_writable`: a listening unix socket with `listen(0)` plus one
    queued connect makes the second `connect` pend; accepting releases it.
  - `a_pending_connect_reports_the_socket_error`: connecting to a closed TCP port returns
    `ConnectionRefused` from `take_error`.
  - `a_registration_is_dropped_before_its_source`: a test-only `Kind` whose `Drop` records order
    into a shared log; assert the registration's drop precedes the source close.
- [ ] Step 5: verification as Task 1 step 6 plus the `cargo tree` EXT assertion (socket2 is
  allowed).
- [ ] Step 6: commit.

### Task 3: Transports and builder targets on the runtime (new commit)

**Commit:** `♻️ zb: Connect every transport on the connection's runtime`

**Files:**
- Modify: `zbus/src/address/transport/{mod,tcp,unix,vsock}.rs`, `zbus/src/address/mod.rs`
  (`Address::connect(self, runtime: &Runtime)`), `zbus/src/connection/builder.rs`,
  `zbus/src/blocking/connection/builder.rs`, `zbus/src/connection/socket/{mod,unix,tcp,vsock}.rs`
  (delete the `Async<T>`/tokio impls; `unix.rs`'s FFI helpers move to `runtime/io/unix.rs`),
  `zbus/src/runtime/mod.rs` (remove `select_runtime!`, `use_tokio`, the free `spawn_blocking`),
  `zbus/src/runtime/async_io.rs` (remove `Task::from_blocking`), `zbus/Cargo.toml`
  (`vsock = ["dep:vsock"]`, remove `tokio-vsock`), `zbus/tests/{e2e,builder_message_stream,
  builder_feature_additivity,connection_closed}.rs`, `zbus/src/connection/mod.rs` tests,
  `zbus/src/address/mod.rs` tests, `zbus/src/connection/handshake/mod.rs` tests
- Create: `zbus/src/connection/socket/tokio_tcp.rs` (`#[cfg(all(windows, feature = "tokio"))]`)

**Behaviour:**
- `Transport::connect(self, address: Address, runtime: &Runtime) -> Result<Stream>`; `Stream`
  variants keep `BoxedSplit`.
- Unix: `SockAddr::unix(path)` / `SockAddr::unix_abstract` (linux) → `connect(runtime, Domain::UNIX,
  Type::STREAM, &addr)` → `Registered::new(runtime, source, Unix)`. Windows: same over `AF_UNIX`
  for `AsyncIo`/`External`; for `Runtime::Tokio(_)` return `Error::Unsupported` (as today).
- Tcp: parse `host` as `IpAddr` first; a literal skips DNS; otherwise
  `runtime.spawn_blocking(move || (host, port).to_socket_addrs(), "resolve host")` filtered by
  family; try each address through `connect`. Nonce: `runtime.spawn_blocking(|| std::fs::read(path),
  "read nonce")` then `write_with` until written. `Runtime::Tokio(_)` on windows:
  `tokio::net::TcpStream::connect` into `TokioTcp` (nonce written with `AsyncWriteExt`).
- Vsock: `SockAddr::vsock(cid, port)`, `Domain::VSOCK`; `Registered<Vsock>`.
- Builder: `Target::{UnixStream(std UnixStream | uds_windows), TcpStream(std), VsockStream(vsock)}`
  → at `target_connect`, `set_nonblocking(true)`, `IoSource::from(OwnedFd::from(stream))`,
  `Registered::new(conn runtime, ..)`. Constructors `unix_stream`, `tcp_stream`, `vsock_stream`;
  remove `async_io_*` and `tokio_*`; keep `socket`/`authenticated_socket`. The blocking builder
  mirrors the renames.
- Delete `runtime::spawn_blocking` (free fn), `select_runtime!`, `use_tokio`, the `Socket`,
  `ReadHalf` and `WriteHalf` impls for `Async<T>`, `tokio::net::*` and `tokio_vsock::*`; grep
  proves no caller remains.

- [ ] Step 1-3: transports, builder, socket module deletions; `cargo check` on default, EXT,
  tokio-only, windows-gnu, apple-darwin targets after each.
- [ ] Step 4: tests. Existing suites (rewrite `async_io_unix_stream`/`tokio_unix_stream` →
  `unix_stream`, etc.; handshake tests build `UnixStream::pair()` and go through `Builder`-level
  helpers or `Registered::new`). New in `zbus/src/connection/mod.rs` tests:
  `a_session_connection_over_an_external_runtime` (`Builder::session().runtime(TestRuntime::new())`,
  `Peer.Ping` through the bus; runs in every configuration, including EXT under
  `dbus-run-session`); `a_tcp_host_name_resolves_through_spawn_blocking`
  (`tcp:host=localhost,port=<closed>` on the default-hook test runtime reaches
  `ConnectionRefused`, proving resolution ran); `a_tcp_literal_skips_resolution`
  (`tcp:host=127.0.0.1,port=<closed>` on a test runtime whose `spawn_blocking` panics reaches
  `ConnectionRefused` without panicking). `zbus/src/address/mod.rs`'s `connect_tcp`/
  `connect_nonce_tcp` tests run against `AsyncIo`, `Tokio` and `External`.
- [ ] Step 5: verification as before; EXT now runs `--tests` (integration tests included) under
  `dbus-run-session`.
- [ ] Step 6: commit.

### Task 4: Processes on the runtime (new commit)

**Commit:** `♻️ zb: Run transport helper processes on the connection's runtime`

**Files:**
- Modify: `zbus/src/runtime/process.rs` (rewrite over `std::process`),
  `zbus/src/address/transport/{unixexec,ibus,launchd,mod}.rs`, `zbus/src/connection/socket/mod.rs`
  (drop `command`), `zbus/Cargo.toml` (`async-io` feature loses `async-process`; tokio loses
  `process`), `zbus/tests/basic.rs` (ibus test), `zbus/tests/unixexec.rs`
- Delete: `zbus/src/connection/socket/command.rs`

**Interfaces produced:**
```rust
// runtime/process.rs,
// #[cfg(all(unix, any(feature = "unixexec", feature = "ibus", target_os = "macos")))]
pub(crate) struct Child {
    stdin: Option<Arc<Registered<Pipe>>>,
    stdout: Arc<Registered<Pipe>>,
    reaper: Task<io::Result<ExitStatus>>,
}
pub(crate) fn spawn(runtime: &Runtime, command: std::process::Command) -> Result<Child>;
    // stdin/stdout piped, stderr inherited; both pipes non-blocking; wrapped as Registered<Pipe>;
    // reaper = runtime.spawn(spawn_blocking(move || child.wait()), "reap <program>")
pub(crate) async fn output(runtime: &Runtime, command: std::process::Command) -> Result<Output>;
    // spawn, read stdout to EOF through read_with, await the reaper, build std::process::Output
impl Child { pub(crate) fn into_split(self) -> BoxedSplit }  // for unixexec; detaches the reaper
```
`unixexec`: `spawn` then `into_split`; the child is not killed on drop (as today); closing stdin
ends it and the reaper collects it. `ibus`/`launchd`: `output`.

- [ ] Step 1-2: rewrite, wire the transports, drop the crate features.
- [ ] Step 3: tests: `zbus/tests/unixexec.rs` unchanged in intent (runs when
  `systemd-stdio-bridge` exists) and additionally over the default-hook test runtime;
  `ibus_connection` in `basic.rs` unchanged; new `a_helper_process_is_reaped` in
  `runtime/process.rs` tests: run `true` through `output` under each runtime variant and assert
  `status.success()` and, on linux for `External`, that `/proc/self/task` returns to its starting
  count (polled under the test timeout).
- [ ] Step 4: verification incl. `cargo tree` EXT and `cargo tree -p zbus -e normal` on the
  default features no longer listing `async-process`.
- [ ] Step 5: commit.

### Task 5: The reference host and the thread-free proof (new commit)

**Commit:** `✅ zb: Prove a single-threaded host runs a connection without zbus threads`

**Files:**
- Create: `zbus/tests/polling_host.rs`, `zbus/tests/polling_host/host.rs` (the runtime)
- Modify: `zbus/Cargo.toml` (`[dev-dependencies] polling`), `zbus/src/runtime/traits.rs` docs
  (point at the host as the reference implementation)

**Host shape (≈200 lines, test code):** the `Runtime` must be `Send + Sync + 'static`, so the host
hands zbus a `Handle { poller: Arc<Poller>, queue: Arc<Mutex<VecDeque<Runnable>>>, timers:
Arc<Mutex<BTreeMap<(Instant, u64), Waker>>> }` with `std::sync::Mutex`; `spawn` builds a task
with `async_task::spawn` (dev-dependency) pushing runnables onto the queue and waking the poller
with `poller.notify()`; `register` adds the fd to the poller with `Event::none(key)` and `poll_io`
re-arms with `modify(readable/writable)`; `sleep_until` inserts a waker keyed by deadline; locks
are `async_lock` (dev-dependency); `spawn_blocking` keeps the trait default. The host's
`run(fut)` loop: drain runnables, poll `fut`, compute the next timer deadline,
`poller.wait(events, timeout)`, wake registrations and expired timers. The test: count threads
(`/proc/self/task` entries on linux), `run(async { session connection with the host; serve an
interface at a path; call it via a proxy with `method_timeout(1s)`; `graceful_shutdown` })`,
count threads again, assert equal. Also `a_timed_out_call_on_the_host_is_cancelled`: call a method
on a served interface that never replies, assert `Error::InputOutput(TimedOut)` within the
timeout and that the host's timer map is empty afterwards.

- [ ] Steps: write host, tests, run in default and EXT configurations.
- [ ] Commit.

### Task 6: CI (fixup into commit `👷 zb: Check the external-runtime-only build in CI`)

- External leg: `--tests` (not `--lib`) so `polling_host` and the session tests run; keep the
  `cargo tree` guard (file-based). `socket2` is allowed.
- `windows_test` matrix gains `tokio-only` (`--no-default-features --features tokio,proxy,service`).
- The `tokio` and `tokio-async-io` suites drop their `tokio-vsock` lines.
- Verify with `python3 -c "import yaml..."` and by running the external leg's commands locally.

### Task 7: Documentation and spec (fixup into commit `📝 book: ...` plus a spec commit)

- `book/src/connection.md`: new section "Runtimes": built-ins by feature; `Builder::runtime` with
  the requirement list (readiness, timers, spawning, locks) and the `spawn_blocking` default with
  the exact list of operations that use it; pointer to `zbus/tests/polling_host/host.rs` as the
  reference host. `faq.md`: update the Tokio entry. `upgrading-to-6.md`: constructor renames and
  the `into_std()`/`into_inner()` conversions, `vsock` no longer implies `async-io`, `tokio-vsock`
  gone, `unixexec` over std processes, removed API. `zbus/README.md`: runtime section rewritten to
  the final model. `zbus/src/runtime/{mod,traits}.rs`: docs describe the delivered behaviour, the
  `poll_io` contract verbatim from Global Constraints.
- Spec: apply "Spec amendments"; commit on `external-runtime-spec` with `Changelog: skip`;
  replace the plan file there with this document.

### Task 8: Final verification, history and PR

- History (nine commits): traits; runtime value (with Tokio); `Builder::runtime`; socket wrapper;
  transports; processes; reference host; CI; book. Every commit builds and passes clippy on
  default and EXT (`git rebase --exec` with the two clippy commands).
- Matrix: fmt check; `cargo --locked check` default/EXT/tokio-only/wire + windows-gnu,
  apple-darwin, freebsd for default and EXT; clippy on the five sets; docs on four sets; tests:
  default, `--all-features -- --skip fdpass_systemd`, tokio-only `--tests`, EXT `--tests`, wire;
  `cargo tree` guards for EXT and for default (`async-process` absent).
- Force-push `runtime-abstraction`; edit PR #1964's title to
  `✨ zb: Run a connection on any runtime, async-io and Tokio included` and rewrite its body:
  one paragraph per commit, the selection table, the external-only build's transports and the
  `spawn_blocking` rule, removed and renamed API, the thread-free proof.
