# Async sockets in zruntime: Implementation Plan

> **For agentic workers:** this plan is executed task by task by subagents, each given the brief
> its task names. Steps use checkbox (`- [ ]`) syntax for tracking. The plan spans two
> repositories: zruntime (Tasks 1 to 7) and zbus (Tasks 8 to 10); zruntime's change lands first,
> because zbus's depends on it.

**Goal:** zruntime gains ready-made async sockets, as smol has in `smol::net` (async-net): a
`net` module with `TcpListener`, `TcpStream` and `UdpSocket`, and `net::unix` with
`UnixListener`, `UnixStream` and `UnixDatagram`, each family behind a cargo feature of its own
(`tcp`, `udp`, `unix`). The sockets run on a zruntime `Runtime` of either flavour, connect without
blocking the thread, take socket addresses rather than host names, as async-io's do, and implement
`futures-io`'s `AsyncRead` and `AsyncWrite` where they are streams.

**What moves from zbus, and what does not.** zbus's socket layer (`zbus/src/runtime/io/`) is not
built on zruntime: it drives every socket through `traits::PollIo`, so the same code serves the
zruntime, Tokio and external runtimes a connection may run on. Types bound to zruntime's reactor
cannot replace it, and it stays. What in it is general is carried over into zruntime's private
implementation: the non-blocking connect (the predicate that tells a connect under way from one
made or failed, Winsock's zero-timeout `select` included, and the retry of a unix connect that a
full listen backlog turned away) and the `MSG_NOSIGNAL` send. One piece moves outright, as a change
of its own that the sockets do not use: `zbus/src/runtime/blocking_thread.rs`, the thread a piece of
blocking work runs on, which has nothing to do with any runtime. It becomes `zruntime::unblock`,
smol's `unblock` in zruntime, and zbus's default `spawn_blocking` takes it from there.

**Scope:** the six socket types and `unblock`; tests, docs and CI for each. Not in scope (see
*Follow-ups*): host-name lookup, `Clone` and owned split halves, `into_std`, vectored writes, unix
sockets on Windows, abstract-namespace and `*_addr` constructors, passing file descriptors and
peer credentials over a unix socket, constructors that find the runtime themselves, and a public
generic `Async<T>` wrapper.

**Tech Stack:** Rust 1.87 (MSRV); `socket2` (moves from a dev-dependency to an optional one),
`futures-io` (new, optional), `futures-core` (already optional), `rustix`'s `net` feature.

## Decisions

Each is the recommended choice; the plan is written against it, and a veto changes the task it
names.

1. **Features:** `unblock = []` (no runtime, no dependency, like `event`);
   `tcp = ["runtime", "dep:socket2", "dep:futures-io", "dep:futures-core"]`;
   `udp = ["runtime"]`;
   `unix = ["runtime", "dep:socket2", "dep:futures-io", "dep:futures-core", "rustix?/net"]`.
   None is a default. `net::unix` is `#[cfg(unix)]`, as smol's and Tokio's are; the `unix`
   feature builds nothing on Windows. (Tasks 1 to 4)
2. **Generic over the flavour:** every socket is `Type<M = Local>` where `M: Mode`, built on a
   `&Runtime<M>` its constructor takes first: `TcpStream::connect(&runtime, addr)`,
   `UnixListener::bind(&runtime, path)`. A `Local` socket is neither `Send` nor `Sync`; a `Shared`
   one is both. No constructor finds a runtime of its own. (Task 2)
3. **No `Clone`:** a `Registration` keeps one waker per interest, so two handles reading at once
   would lose each other's wake-ups. `AsyncRead` and `AsyncWrite` are implemented for `&Stream`
   as well as `Stream`, so one reader and one writer can share a stream, as std's `Read for
   &TcpStream` allows; the docs of each type say one reader and one writer at a time (likewise one
   `accept` at a time on a listener, one `recv*` and one `send*` on a datagram socket). (Task 2)
4. **Socket addresses only, as async-io has it:** a TCP or UDP address is an `impl
   Into<SocketAddr>`: a `SocketAddr`, a `SocketAddrV4` or `SocketAddrV6`, or an IP address and a
   port. No host name is looked up: std's lookup blocks the thread, which tokio and smol (async-net)
   move to a thread of their own, and which the maintainer chose to leave to the caller, who can
   run it through `unblock`. So `TcpListener::bind`, `UdpSocket::bind` and `UdpSocket::connect`,
   which do not block once no lookup is involved, are synchronous; `TcpStream::connect` and
   `UdpSocket::send_to` are async because the connect and the send wait for the socket. (Task 2)
5. **`unblock` is a thread per call,** and a change of its own, ahead of the sockets: zbus's
   `blocking_thread::run`, made public as
   `zruntime::unblock(work) -> Unblock<T>`, a named future (`Send` where `T` is). Semantics as
   in zbus: the work starts at once and runs to its end whether or not the future is polled or
   dropped; a panic in it is raised again where the future is polled; failing to start the thread
   panics. The thread is named `zruntime blocking work` (zbus's tests match the start of a name,
   which Linux truncates to fifteen bytes). (Task 1)
6. **Connecting never blocks:** a stream socket is made with `socket2`, non-blocking from the
   start, converted into the std type at once, registered, and waited on for writability, with
   zbus's `outcome` predicate (`SO_ERROR`, then `getpeername` on unix; a zero-timeout `select` on
   Windows) deciding made, failed or still under way. A unix connect that a full backlog turns
   away (`EAGAIN`) is tried again with a fresh socket every 20 ms on the runtime's timer, until it
   gets in or the caller drops the future, which is the wait a blocking `connect` makes in the
   kernel; a TCP one is not, since `EAGAIN` there is a shortage of local ports, which a blocking
   `connect` reports at once too. (Tasks 2, 4)
7. **Writes do not raise `SIGPIPE`:** TCP writes through std's `Write for &TcpStream`, which sends
   with `MSG_NOSIGNAL` (and std and socket2 set `SO_NOSIGPIPE` where that flag does not exist).
   Unix stream writes go through `rustix::net::send` with zbus's `SEND_FLAGS`, since std's `Write
   for &UnixStream` only gained the flag after 1.87. Vectored writes are left to the traits'
   defaults, which write the first buffer, because std's `writev` sends without the flag.
   (Tasks 2, 4)
8. **`poll_close` shuts the write half down,** as Tokio's `poll_shutdown` does, so that the peer
   reads the end of the stream; `poll_flush` has nothing to do. The shutdown is of the socket, so
   closing through one `&Stream` ends the stream for every handle on it. A `NotConnected` from
   the shutdown, which some BSDs report for a second one or for a peer that has gone, is a stream
   closed already and reads as success, so that `close` means the same on every platform.
   (Task 2)
9. **`incoming()`** on both listeners, an `Incoming<'_, M>` that implements `futures_core::Stream`,
   as smol's do. (Tasks 2, 4)
10. **zbus's pin:** zbus's workspace points zruntime at the zeenix fork and the tip of this
    change, with a FIXME to go back to `z-galaxy/zruntime` once it is merged, as it did for the
    locks. (Task 8)

## Design

### zruntime: `src/unblock.rs` (feature `unblock`)

zbus's `blocking_thread.rs` with `run` made `pub fn unblock`, `Blocking` renamed and made public as
`Unblock<T>` (with `Debug`), the `THREAD_NAME` changed, and public docs with an example. The
internal comments are kept unless a reviewer finds them wrong. Its four tests move to
`src/tests/unblock.rs` (`futures_lite::future::block_on` drives them; they need no runtime) and run
under Miri. Re-exported at the crate root: `zruntime::{unblock, Unblock}`.

### zruntime: `src/net/` (built where any of `tcp`, `udp`, or `unix` on unix, is)

- `mod.rs`: module docs (what is here, the feature of each family, the runtime argument, one
  reader and one writer, an example), the `mod`s and re-exports.
- `io.rs`: `pub(crate) struct Io<T, M>`: a `Registration<M>` and the socket in an `M::Ptr<T>`, of
  which the reactor holds a clone (through a new sealed hook, `Mode::source_ptr`, coercing
  `M::Ptr<T>` into `M::SourcePtr` for `T: AsSource + Send + Sync + 'static`). `Io::new(runtime,
  socket)` registers a socket that is non-blocking already; `get_ref`; `runtime()` (a handle on
  the runtime it is registered on, for `accept`); `poll_read_with`/`poll_write_with` and their
  `async` forms, which run an operation on `&T` under the registration's `poll_io`, retrying an
  `Interrupted` at once as zbus's `RegisteredIo::io` does.
- `connect.rs` (`tcp`, or `unix` on unix): `attempt`, `is_in_progress`, `outcome` and Windows'
  `progress`, from zbus's `connect.rs`, generic over the std type the socket becomes.
- `tcp.rs` (`tcp`): `TcpListener` (`bind`, `from_std`, `accept`, `incoming`, `local_addr`,
  `ttl`, `set_ttl`) and `TcpStream` (`connect`, `from_std`, `local_addr`, `peer_addr`, `peek`,
  `shutdown`, `nodelay`, `set_nodelay`, `ttl`, `set_ttl`; `AsyncRead`, `AsyncWrite`); `Incoming`.
- `udp.rs` (`udp`): `UdpSocket` (`bind`, `from_std`, `local_addr`, `peer_addr`, `connect`,
  `send_to`, `recv_from`, `peek_from`, `send`, `recv`, `peek`, `broadcast`, `set_broadcast`,
  `ttl`, `set_ttl`, the multicast options and `join_`/`leave_multicast_v4`/`_v6`).
- `unix.rs` (`unix`, unix only): `UnixListener` (`bind`, `from_std`, `accept`, `incoming`,
  `local_addr`), `UnixStream` (`connect`, `pair`, `from_std`, `local_addr`, `peer_addr`,
  `shutdown`; `AsyncRead`, `AsyncWrite`), `UnixDatagram` (`bind`, `unbound`, `pair`, `from_std`,
  `connect`, `send_to`, `recv_from`, `send`, `recv`, `local_addr`, `peer_addr`, `shutdown`);
  `Incoming`.
- Every socket implements `Debug` and `AsFd`/`AsRawFd` (unix) or `AsSocket`/`AsRawSocket`
  (Windows). `from_std` sets the socket non-blocking itself, and so does `accept` (std's `accept`
  leaves an accepted socket blocking on Linux): `Io::new` takes a socket that is non-blocking
  already, and its docs say so, so that every caller has to see to it.
- `Io::runtime()` is a `pub(crate)` `Registration::runtime()`, a handle built from the core the
  registration holds, so a listener keeps no runtime of its own beside its registration.
- `Io::new` asks for progress (`M::ensure_progress`) once the source is in the reactor's map, as
  `Runtime<Shared>::register` does, with its comment.
- `net`'s module docs say that a runtime on Windows watches at most 1023 sockets (one `select`
  over a set of 1024, one place of which is the runtime's own), so a server there tops out at
  that many sockets, listener included.

Tests, in `src/tests/net/` (`mod.rs`, `tcp.rs`, `udp.rs`, `unix.rs`), each gated on its feature,
in both flavours wherever the test means the same in each: every public method at least once;
connect to a refused port reports the error (zbus's `RefusedPort` idea: a bound socket nobody
listens on); a pending connect resolves once accepted (a listener with a backlog of one, on Linux
and Android only: other platforms' backlogs admit more than they are asked for, so the connection
would not be left pending); a full unix backlog is retried until accepted; `poll_close` makes the
peer read the end; concurrent read and write on `&Stream`; a `Shared` socket is used from a task
on another thread; nothing assumes IPv6, which CI runners and containers may lack. Doc examples
use loopback TCP or a unix socket in a fresh directory, and need neither `helper` nor IPv6, since
Windows CI runs them all. Compile-time doc tests in `src/tests/mod.rs`: a `Local` socket is
neither `Send` nor `Sync`, a `Shared` one is both.

### zbus

`comms` gains `zruntime/unblock`. `zbus/src/runtime/blocking_thread.rs` goes; `traits.rs`'s
default `spawn_blocking` and `test_runtime.rs` call `Box::pin(zruntime::unblock(work))`; the tests
that match the thread's name (`runtime/tests.rs`, `process.rs`) match zruntime's. Docs that call
the thread zbus's own say where it comes from.

## Global Constraints

- Commits: gimoji prefix copied from the gimoji database (no package prefix in zruntime; `zb:` in
  zbus), atomic, author `Zeeshan Ali Khan <zeenix@gmail.com>` (check `git config` before each
  commit), an `Assisted-by:` trailer and no co-author lines or session links. This plan's own
  commits end with a `Changelog: skip` trailer.
- 100 columns in code, comments and Markdown; no trailing whitespace; `cargo +nightly fmt`.
- No `#[allow(dead_code)]`, no `ignore`/`no_run` doc tests. `unsafe` only where zbus's code has
  it (Windows' `select`), each block with its `SAFETY` comment.
- Items ordered top-down (CONTRIBUTING.md); `pub` before `pub(crate)` before private.
- MSRV 1.87 with edition 2024: no let-chains (`if let ... && ...`, stable from 1.88) or anything
  else newer; the host toolchain is newer and does not catch it, `cargo +1.87.0 check` does.
- CI builds with `--locked`: a commit that changes the dependencies carries `Cargo.lock` with it.
- Each type's docs list every operation of which one at a time may wait: read and `peek` (one
  reader), write (one writer), `accept` and `incoming` (one acceptor), `recv*` and `send*`.
- The docs' feature lists name what each feature pulls in, as they do for `rustix` and
  `windows-sys`: `tcp` and `unix` bring `socket2`, `futures-io` and `futures-core`.
- Subagents edit only the files their task lists, do not commit and do not run `cargo fmt` (two
  of them may share a working tree); the orchestrator formats and commits.
- Push to the `zeenix` forks (`origin`) on `ccr-b8770982-7qin80`; no PRs unless asked.

## Execution

zruntime gets four commits, each of which passes CI on its own: `unblock`; the `net` module with
TCP; UDP; unix.

### Task 0: This plan (orchestrator)

- [ ] Commit this plan to zbus.
- [ ] Baseline: `cargo test --all-features` in zruntime.

### Task 1: `unblock` (Sonnet)

- [ ] Feature, `src/unblock.rs`, `lib.rs` re-exports, `src/tests/unblock.rs`, its `mod` line.
- [ ] `cargo check --no-default-features --features unblock`; `cargo test --all-features`.

### Task 2: The `net` foundation and TCP (orchestrator writes the foundation; Sonnet the rest)

- [ ] Orchestrator: `Mode::source_ptr`, `net/{mod,io,connect}.rs`, the `tcp` feature and its
      dependencies.
- [ ] Sonnet: `net/tcp.rs` and `src/tests/net/{mod,tcp}.rs`, and the `Send`/`Sync` doc tests.

### Task 3: UDP (Sonnet, beside Task 4)

- [ ] `net/udp.rs`, `src/tests/net/udp.rs`. The orchestrator adds the lines both tasks share
      (`Cargo.toml`'s features, the `mod`s and re-exports) beforehand, and splits them between
      the two commits.

### Task 4: Unix (Sonnet, beside Task 3)

- [ ] `net/unix.rs`, `src/tests/net/unix.rs`, as Task 3.

### Task 5: Docs and CI (Sonnet)

- [ ] `README.md` (features, a short *Sockets* section), `src/event-only.md` (features, plain
      text), `AGENTS.md` (features paragraph, commands, architecture tree, guidelines, key
      files), `.github/workflows/rust.yml` (`unblock`, `tcp`, `udp`, `unix` each alone; Miri
      gains `unblock`), docs.rs metadata. Each line goes into the commit of the feature it
      documents.

### Task 6: Review (Opus, and the advisor; fixes by Sonnet)

- [ ] The ported code against zbus's, line by line; soundness of the `Mode` hook and the drop
      order of `Io`; lost wake-ups; docs (accuracy, no internals, style); item order; that each
      test fails without what it checks.

### Task 7: zruntime verification and push (orchestrator)

- [ ] nightly fmt; clippy (`--all-targets --all-features`, `-D warnings`) on the host and the
      five other targets; each feature alone; `cargo test --all-features`; docs with `-D
      warnings`; 1.87.0; Miri for `unblock`. Each commit checked on its own. Push.

### Task 8: zbus port (Sonnet)

- [ ] Pin (Decision 10), `cargo update -p zruntime`; `comms` gains `zruntime/unblock`;
      `blocking_thread.rs` goes; its callers and the name-matching tests; docs. The commit
      message says the threads blocking work runs on are now named `zruntime blocking work`,
      which shows in a debugger.

### Task 9: zbus verification (Sonnet, background)

- [ ] The suites of zbus's CI under `dbus-run-session`, clippy, nightly fmt, docs,
      `CI/forbidden-deps.sh`; failures compared with the base's.

### Task 10: Review and report (Opus, the advisor, orchestrator)

- [ ] Review; fixes; append a *Report* section here, which names `unblock` as the one public
      addition beyond `net`, made for the move from zbus; commit; push.

## Follow-ups

- Host names, and connecting to the first of several addresses: a `ToSocketAddrs` of zruntime's
  own that looks a name up through `unblock`, as tokio and smol do theirs on threads of their own.
- `Clone`, or owned split halves (`into_split`), for a reader and a writer in separate tasks.
- `into_std`, which has to take the socket back from a reactor that may hold a clone of it.
- Vectored writes with `MSG_NOSIGNAL` (`sendmsg`).
- Unix sockets on Windows, through `socket2`'s `AF_UNIX` support, as zbus has through
  `uds_windows`.
- Abstract-namespace addresses and `bind_addr`/`connect_addr`.
- File descriptors and peer credentials over a unix socket (zbus has both, runtime-agnostic).
- Constructors that take `SharedRuntime::current()` (the `helper` feature).
- A public generic `Async<T>` over any non-blocking source.
