# Connecting and child processes through the runtime: Implementation Plan

> **For agentic workers:** this plan is executed task by task by subagents. Steps use checkbox
> (`- [ ]`) syntax for tracking. The plan spans two repositories: zruntime (Task 1) and zbus
> (Tasks 2 to 6); zruntime's change lands first, because zbus's zruntime backend depends on it.

**Goal:** zbus stops carrying its own copy of what its runtimes already do. Two pieces of
`zbus/src/runtime/` duplicate zruntime, and have a counterpart in Tokio as well:

* the non-blocking connect of `io/connect.rs`: the connect issued on a non-blocking socket, the
  wait for writability, the predicate that tells a connection under way from one made or failed
  (Winsock's zero-timeout `select` included), and the retry of a unix connect that a full listen
  backlog turned away. zruntime carried all of it over into `src/net/connect.rs`, behind its
  `TcpStream::connect` and `UnixStream::connect`; Tokio has `TcpStream::connect` and
  `UnixStream::connect_addr`.
* the `Reaper` of `process.rs`: a look at a helper process that may have exited, and otherwise a
  blocking `wait` on a worker, started as the connection lets go of the helper. zruntime's
  `process` module waits for a child through a pidfd on Linux and a kqueue on Apple's platforms
  and the BSDs, and collects one that is let go of while it runs (`reap_on_drop`); Tokio's
  `process` module has a reaper of its own.

`traits::Runtime` gains the operations behind them, and each built-in backend implements them with
its runtime crate's own API. An external runtime implements them too, with what it has of its own:
the methods are required (decision 1). Beside them, the Tokio backend watches a socket of zbus's
on Windows through a stream of its own rather than owning the stream (decision 10), so that no
special case is left for it.

**Scope:** the trait methods, their erased mirrors, the zruntime and Tokio backends, the callers in
the transports and in `process.rs`, the Tokio backend's registration on Windows, the example
external runtime in `zbus/tests/polling_runtime`, the docs that list what reaches
`spawn_blocking`, and three additions to zruntime. Not in scope: the readiness path (`PollIo`,
`SocketOps`, `RegisteredIo`), which serves every runtime alike and stays; and the peer-credential
lookups, which zbus alone does.

## Decisions

1. **No zbus cargo feature gates a trait method, and each new method is required.** A method behind
   one of zbus's features is a trap for an implementation in another crate, which cannot test zbus's
   features: with the feature on, an `impl` that leaves the method out fails to build (E0046), and
   with it off, one that has it fails as well (E0407). Whichever crate in the graph turns the
   feature on decides, which is the feature-additivity break that
   `tests/builder_feature_additivity.rs` guards against elsewhere. The only `cfg` on a method is the
   platform's: one that takes a unix type is `#[cfg(unix)]`, which an implementation can mirror.
   None of the methods has a default, so that zbus carries no fallback code: every runtime
   implements the three methods, with what it has of its own, as the example runtime does on a
   single thread, or else with a blocking connect and a wait on a thread for blocking work. The
   backends implement each method wherever the trait has it, whatever zbus's features: where zbus's
   features leave a backend nothing to do it with, as in a Tokio build with no transport that runs
   a program, where zbus does not turn on Tokio's `process` feature, the method reports
   `Unsupported`, and zbus never calls it there.
2. **`connect_tcp(&self, SocketAddr)` and `connect_unix(&self, &Path)` resolve to an `IoSource`**:
   a connected socket in non-blocking mode, which the connection registers through
   `register_io_source` as it registers every other socket. The readiness path stays one, and the
   socket is registered twice, once by the runtime for the connect and once by zbus for the
   traffic, as `io/connect.rs` registers it today. On Linux and Android, a path that starts with a
   zero byte names a socket in the abstract namespace by the rest of it, as Tokio's
   `UnixStream::connect` takes it. Both return `impl Future<Output = io::Result<IoSource>> + Send`.
3. **A runtime with no non-blocking connect connects on blocking work**: a blocking `connect(2)`
   on a thread for blocking work, such as `spawn_blocking`'s, with the socket switched to
   non-blocking mode once connected. `io/connect.rs` loses the readiness wait, the predicate, the
   Windows `select` and the backlog retry, and keeps a blocking connect through socket2, which the
   zruntime backend's unix connect on Windows (where zruntime has no unix socket) and the test
   runtimes use. The cost falls on a runtime that connects this way: a connect occupies a thread
   while the kernel works on it, and dropping the future does not stop that work, only discards
   the socket it makes. A unix connect to a listener whose backlog is full waits in the kernel
   rather than in a retry loop on the timer.
4. **`#[cfg(unix)] spawn_process(&self, Command, stdin: Stdio, stdout: Stdio, stderr: Stdio)`
   returns `io::Result<Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + 'static>>>`**,
   the future of the exit status of the process it spawned. zbus awaits it to its end, on a task
   of the runtime where nothing else does (decision 11); a future dropped before it resolves leaves
   the process to the runtime, which may collect it once it exits, as zruntime does, when it next
   gets round to it, as Tokio does, or not at all, and never kills it. No associated type: a
   wait written as an `async` block has no type to name, and would be boxed anyway. The streams
   are passed beside the command rather than set on it, because zruntime's
   `Command::from(std::process::Command)` cannot see what was set on the std command and gives a
   stream nothing was set through its own builder the default of whichever of `spawn`, `status`
   and `output` runs it.
5. **zbus makes the pipes to a helper process itself**, with `std::io::pipe` (Rust 1.87; zbus's
   MSRV is 1.89), and hands the child's ends over as `Stdio`. Its own ends stay `IoSource`s driven
   through `PipeOps`, so a pipe is read and written on every runtime as a socket is, and the runtime
   only spawns and waits.
6. **`spawn_process` has no default either.** A runtime with no way to wait for a child without a
   thread waits for it with a blocking `wait` on a thread for blocking work, after a look at it,
   which collects a process that has exited already. Or it looks at the child at an interval on its
   timer, as the example runtime does (Task 5). The future outlives the borrow of `self`, so a wait
   it starts later goes through something it owns, a handle of the runtime's that it keeps or a
   pool it reaches without one, which the trait's docs say. The test runtimes wait on a thread,
   spawning with std and waiting on `zruntime::unblock`.
7. **zruntime gains `TcpStream::into_std`, `UnixStream::into_std` and `UnixStream::connect_addr`**,
   each as std and Tokio have it: the first two hand the socket back, unregistered and still
   non-blocking, and the third connects to a `std::os::unix::net::SocketAddr`, which is what makes
   an abstract-namespace name reachable on Linux and Android. Each is general, and zbus's zruntime
   backend needs all three.
8. **zbus's features.** `default-rt` turns on zruntime's `tcp`, `unix` and `process`; `unixexec` and
   `ibus` turn on `tokio?/process`, and so does a macOS-only dependency entry for Tokio, since
   `launchd:` runs a program on macOS whatever the features. zruntime's `process` is on under
   `default-rt` whether or not a transport runs a program, as Cargo cannot turn a feature on only
   where two others are, and the backend spawns through it on every unix build, as the trait has the
   method there whatever the features. In a build with no transport that runs a program, where zbus
   does not turn on Tokio's `process`, the Tokio backend's `spawn_process` reports `Unsupported`.
   Tokio's `fs` and `io-util` features go: nothing in the library reaches them.
   The Tokio zbus requires is the newest, 1.53, which is where `UnixStream::connect_addr` arrived.
9. **The zruntime pin.** zruntime's additions land on its `main` first (Task 1), and zbus moves to
    that `main` in a commit of its own, which builds unchanged, before the work that uses them.
10. **Tokio watches a socket of zbus's on Windows through its own `TcpStream`.** Tokio has no
    `AsyncFd` there, so the backend's registration is a `TcpStream` made from a duplicate of the
    connection's socket handle: readiness comes from `poll_read_ready` and `poll_write_ready`, and
    the operation runs under `try_io`, which runs it only while Tokio holds readiness and clears
    that readiness on `WouldBlock`, which also has mio arm its poll of the socket again, whichever
    handle the operation used. With that, the TCP stream that Tokio owned on Windows, and the
    special cases around it in the transports and the builder, go. A unix-domain socket stays
    `Unsupported` on Tokio there, as mio has none; the backend tells one by its address. The
    Windows CI job's Tokio suite, which reaches the daemon over nonce-tcp, exercises the path.
11. **zbus awaits a helper process's exit on a task of the runtime.** The future `spawn_process`
    hands back is awaited from the moment the program is spawned, on a detached task for a
    `unixexec:` program and in the call itself for an `ibus:` or `launchd:` one, so the program is
    collected as soon as it exits and nothing has to start the wait when a pipe is let go of. A
    runtime cannot be relied on to collect a child whose wait is dropped: Tokio does so only when
    its driver next returns from waiting for events, which an idle multi-threaded runtime may never
    do. So the trait's contract does not promise that a dropped future collects the process
    (decision 4), the backends return their crate's wait as it is, and no implementation hands a
    dropped child over to anything. A process is never killed.

## Tasks

### Task 1: zruntime additions (zruntime repository)

- [ ] `TcpStream::into_std(self) -> std::net::TcpStream`, through `Async::into_inner`; the docs say
      the socket is no longer watched and is still non-blocking, and that on a shared runtime the
      call may wait for a moment, as `into_inner` does.
- [ ] `UnixStream::into_std(self) -> std::os::unix::net::UnixStream`, likewise.
- [ ] `UnixStream::connect_addr(runtime, &std::os::unix::net::SocketAddr)`: a pathname address
      goes to `SockAddr::unix` as `connect`'s does, an abstract name on Linux and Android as a
      leading zero byte, and an unnamed address fails with `InvalidInput`. Same backlog behaviour
      as `connect`.
- [ ] Tests for each (an abstract-name round trip on Linux), `CLAUDE.md` and README where they
      enumerate the API, the feature-alone `cargo check`s, clippy, nightly fmt.

### Task 2: the zruntime pin (zbus)

- [ ] Point `[workspace.dependencies.zruntime]` at zruntime's current `main` (1becd68), which
      holds Task 1's additions; it builds as it is.

### Task 3: Tokio on Windows (zbus)

- [ ] The Tokio backend registers a socket on Windows through a `TcpStream` made from a duplicate
      of the handle (decision 10), and the stream Tokio owned there, `TokioTcp`, goes with the
      Tokio-only connect of the `tcp:` transport and the matches on the runtime in the builder and
      the `tcp:` and `unix:` transports. The `unix:` transport maps an `Unsupported` registration
      to `Error::Unsupported`. The sweep that left Tokio out on Windows in the socket tests goes,
      its test running under Tokio there too.
- [ ] Tokio's `fs` and `io-util` features go (decision 8).

### Task 4: `connect_tcp` and `connect_unix` (zbus)

- [ ] The methods on `traits::Runtime`, required (decision 1), their mirrors on `ErasedRuntime`
      and the forwarding impl for the boxed runtime, which is how a handed-in runtime's connects
      are reached, the dispatch on the crate's `Runtime` enum, and the calls in the `tcp:` and
      `unix:` transports. `runtime/io/connect.rs` keeps only a blocking connect, which retries an
      interrupted `connect(2)` and takes `EISCONN` on the retry as the success it is, as std's
      connect does, and is built only where something calls it. A shared helper,
      `io::unix_socket_address`, turns a unix path into a std `SocketAddr` for both backends, a
      leading zero byte naming an abstract socket on Linux and Android.
- [ ] zruntime: `connect_tcp` through `zruntime::net::TcpStream::connect` and `into_std`;
      `connect_unix` on unix through `UnixStream::connect_addr` and `into_std`, and on Windows,
      where zruntime has no unix socket, a blocking connect on `spawn_blocking`. `default-rt` turns
      on zruntime's `tcp` and `unix` features (decision 8), which the comment in
      `CI/forbidden-deps.sh` lists.
- [ ] Tokio: `connect_tcp` through Tokio's `connect` and `into_std`, and `connect_unix` on unix
      through `connect_addr` and `into_std`, `Unsupported` on Windows. The handle is entered for
      every poll of a connect, so that the socket is Tokio's own whichever thread polls it. mio
      takes only `EINPROGRESS` as a connect under way, so a listener whose backlog is full fails
      Tokio's unix connect with `WouldBlock` on Linux and Android, where zruntime's waits: the
      backend tries again every 20 milliseconds on Tokio's timer.
- [ ] `zbus/tests/polling_runtime` implements both on its own poller, without a thread: its
      `lifecycle` test counts the process's threads, and a blocking connect on a thread would start
      one. That takes `socket2` and `libc` (for `EINPROGRESS`, whose `io::ErrorKind` is unstable)
      as dev-dependencies of zbus.
- [ ] The test runtimes connect on their own blocking work, which keeps the blocking-call counts of
      the connection tests. The connect tests of `runtime/io/tests.rs` test the methods on every
      runtime; a runtime with connects of its own is checked to be the one a connection connects
      with, and the Tokio backend's connects are polled from a thread with no Tokio runtime
      current.

### Task 5: `spawn_process` (zbus)

- [ ] The method on `traits::Runtime`, required (decision 1), its mirror on `ErasedRuntime` and
      the dispatch on the `Runtime` enum. `process.rs` makes its pipes with `std::io::pipe`
      (decision 5), spawns through the runtime, and awaits the exit future on a task of the
      runtime (decision 11); the `Reaper` goes.
- [ ] zruntime: `spawn_process` on every unix build through `zruntime::process::Command`, its
      streams set through its builder. `default-rt` turns on zruntime's `process` feature
      (decision 8), which the comment in `CI/forbidden-deps.sh` lists, with `signal-hook-registry`,
      which Tokio's brings in.
- [ ] Tokio: `spawn_process` through `tokio::process::Command` where zbus turns on Tokio's
      `process` feature, and `Unsupported` on the rest of unix. The handle is entered for the spawn
      and for every poll of the wait: Tokio registers a pidfd or a `SIGCHLD` handler at the spawn,
      and registers the pidfd again in a poll of the wait that finds it readable too early, each on
      the runtime that is current.
- [ ] `zbus/tests/polling_runtime` implements `spawn_process` on its own timer, for a runtime with
      nothing to tell it a child exited. The test runtimes spawn a std child and wait for it on
      `zruntime::unblock` (decision 6).
- [ ] The tests of `process.rs` read the process table, getting the process id from the program
      itself, in place of counting the blocking calls of a `TestRuntime`; under every runtime, a
      helper process is collected while the connection holds both halves of its pipes.

### Task 6: docs (zbus)

- [ ] The `traits` module docs and `spawn_blocking`'s list of what reaches it; the book's
      `connection.md` and `upgrading-to-6.md` paragraphs on supplying a runtime, and the README.

## Follow-ups

- `the_default_blocking_hook_runs_the_work_on_a_short_lived_thread` waits for zruntime's pool to
  shrink back, which its idle threads do only after a timeout; its name and its last check no
  longer say what the default hook does.
