# Moving async-broadcast into zruntime: Implementation Plan

> **For agentic workers:** this plan is executed task by task by subagents, each given the brief
> its task names. Steps use checkbox (`- [ ]`) syntax for tracking. The plan spans two
> repositories: zruntime (Tasks 1 to 4) and zbus (Tasks 5 to 7); zruntime's change lands first,
> because zbus's depends on it.

**Goal:** `event-listener`, `event-listener-strategy` and `async-broadcast` leave zbus's
dependency graph. async-broadcast 0.7.2 moves into zruntime as its `broadcast` module, built on
zruntime's own `Event`, behind a new `broadcast` cargo feature that zbus's `comms` feature
enables. async-broadcast is maintained by zbus's maintainer, so this is a move, not a fork.

**Scope:** the channel and its async API, nearly as async-broadcast has it. Two API breaks land
with the move, since it is a new crate's API anyway:

- [async-broadcast#79](https://github.com/smol-rs/async-broadcast/issues/79): capacities are
  `NonZeroUsize`, turning the capacity-zero panic into a type error.
- [async-broadcast#75](https://github.com/smol-rs/async-broadcast/issues/75): there is one
  `broadcast` and one `recv`, each returning a plain, `Unpin` future. The issue asks to swap the
  names of the boxed (`recv`) and unboxed (`recv_direct`) variants; the boxed ones exist only
  because async-broadcast's futures are `!Unpin` (an `event-listener` listener and a
  `PhantomPinned`). zruntime's `EventListener` is `Unpin`, so the futures are too, and the split
  has no reason left to exist.

Not in scope: a blocking API (`*_blocking`), `event-listener-strategy`'s strategy machinery, and
anything done to async-broadcast upstream (see *Follow-ups*).

**Tech Stack:** Rust 1.87 (MSRV), `std` plus `futures-core` (for `Stream`) in zruntime;
criterion (through `codspeed-criterion-compat`) for the benchmark.

## Decisions

Each is the recommended choice; the plan is written against it, and a veto changes the task it
names.

1. **Module, not root:** the channel lives at `zruntime::broadcast`, not re-exported at the
   crate root, where `Sender`, `Receiver`, `Send`, `Recv` and the error types would crowd the
   runtime's items. (Task 1)
2. **Constructor name:** `zruntime::broadcast::channel(cap)`, since `broadcast::broadcast`
   stutters. (Task 1, Task 5)
3. **Capacities (#79):** `channel` and the three `set_capacity` methods take `NonZeroUsize`
   (`set_capacity(0)` today leaves a channel whose every send waits forever); the `capacity`
   getters return `NonZeroUsize` too, for symmetry. (Task 1)
4. **Futures (#75):** `Sender::broadcast` returns `Send<'_, T>` and `Receiver::recv` returns
   `Recv<'_, T>`, both `Unpin`; `broadcast_direct`, `recv_direct`, the `*_blocking` methods,
   `pin-project-lite` and `PhantomPinned` go. `Receiver::poll_recv` takes `&mut self` and a
   `Context`, as an `Unpin` type's poll method can. A compile-time check that both futures and
   `Receiver` are `Unpin` is part of the tests. (Task 1)
5. **Stream:** `Receiver` keeps its `Stream` and `FusedStream` impls, through an optional
   `futures-core` dependency that the `broadcast` feature turns on. (Task 1)
6. **Locking:** the channel's mutex is taken poison-tolerantly (`unwrap_or_else(|e|
   e.into_inner())`), as `event.rs` does: the one piece of user code that runs under it is
   `T::clone` in `try_recv`, and a panic there leaves the channel's state consistent (the
   position and counts are updated before the clone). (Task 1)
7. **Notifying under the lock:** the port keeps notifying its events while holding the channel's
   mutex, as async-broadcast does. `Event::notify` wakes wakers after letting go of its own lock,
   but still under the channel's; a waker that polls synchronously and re-enters the channel
   would deadlock, as it would with async-broadcast today. The module's internal docs say so;
   moving the notifications out of the lock is a follow-up, not part of the move. (Task 1)
8. **License notice:** async-broadcast is `MIT OR Apache-2.0`, its MIT notice reading
   `Copyright (c) 2020 Yoshua Wuyts`; zruntime is MIT. `src/broadcast.rs` opens with a comment
   saying where the code comes from and carrying that notice. (Task 1)
9. **zbus's queue sizes:** `Connection::set_max_queued(usize)` and the builder's
   `max_queued(usize)` stay as they are (changing them is a separate zbus API decision, though
   6.0 is not out yet); zbus converts at the boundary and a zero panics there with a clear
   message, as it panics inside `async_broadcast::broadcast` today. (Task 5)

## Design: `zruntime::broadcast`

A port of async-broadcast 0.7.2's `src/lib.rs`, keeping its structure (one `Inner<T>` behind an
`Arc<Mutex<_>>`, a `VecDeque<(T, usize)>` of messages with per-message waiter counts, `head_pos`,
`send_ops` and `recv_ops` events) and its semantics (overflow mode, `await_active`, inactive
receivers, closing when all senders or all receivers are gone). What changes:

- `event_listener::{Event, EventListener}` → `crate::{Event, EventListener}`. The port relies on
  `listen`, `notify(1)` (counting: it notifies only if no listener is notified already) and
  `notify(usize::MAX)`, and on a notified listener that is dropped passing its notification on.
  `event.rs`'s docs state all four; Task 3 checks the port against them.
- `Send`/`Recv` are plain structs (sender/receiver reference, `Option<EventListener>`, the
  message) implementing `Future` directly; the `easy_wrapper!`/`EventListenerFuture` layer goes.
- API per the decisions above; all other methods and their docs are kept, doc examples rewritten
  against the new names and `futures_lite::future::block_on`.
- Items ordered top-down per CONTRIBUTING.md. Inside the module, `struct Send` shadows the
  marker trait: a `Send` bound there is spelled `std::marker::Send`, as async-broadcast does.

Public items: `channel`, `Sender`, `Receiver`, `InactiveReceiver`, `Send`, `Recv`, `SendError`,
`TrySendError`, `RecvError`, `TryRecvError`.

## Global Constraints

- Commits: gimoji prefix copied from the gimoji database (no package prefix in zruntime; `zb:` in
  zbus), atomic, author `Zeeshan Ali Khan <zeenix@gmail.com>` (check `git config` before each
  commit), an `Assisted-by:` trailer and no co-author lines or session links. This plan's commit
  ends with a `Changelog: skip` trailer.
- 100 columns in code, comments and Markdown; no trailing whitespace; `cargo +nightly fmt`.
- No `#[allow(dead_code)]`, no `ignore`/`no_run` doc tests.
- Push to the `zeenix` forks (`origin`) on `claude/modest-euler-w8gk5w`; no PRs unless asked.

## Execution

### Task 0: This plan (orchestrator)

- [ ] Commit this plan to zbus.
- [ ] Baseline: `cargo tree -i event-listener --all-features -e normal` in zbus lists
      `async-broadcast` and `event-listener-strategy` as its users.

### Task 1: The port (Sonnet)

- [ ] `cargo add futures-core --optional --no-default-features`; feature
      `broadcast = ["event", "dep:futures-core"]` with a comment like the others.
- [ ] `src/broadcast.rs` per the design; `#[cfg(feature = "broadcast")] pub mod broadcast;` in
      `lib.rs`.
- [ ] `cargo check --no-default-features --features broadcast`, `cargo check --all-features`,
      `cargo test --all-features --doc`.

### Task 2: Tests, benchmark, CI and docs (Sonnet)

- [ ] async-broadcast's `tests/test.rs` → `src/tests/broadcast.rs`, gated on the feature;
      `easy-parallel` → `std::thread::scope`; plus the `Unpin` check and capacity tests.
- [ ] `benches/broadcast_bench.rs` → `benches/broadcast.rs` on codspeed criterion,
      `required-features = ["broadcast"]`; the codspeed CI build gains the feature.
- [ ] CI: `cargo check --no-default-features --features broadcast` beside the other
      feature-alone checks.
- [ ] `CLAUDE.md` (features paragraph, architecture tree, guidelines) and `README.md`.

### Task 3: Review (Opus; fixes by Sonnet)

- [ ] Review the port against async-broadcast line by line for semantic drift, the `Event`
      semantics above, the two issues, docs and item order.

### Task 4: zruntime verification and push (orchestrator)

- [ ] fmt, clippy (all targets, all features), the feature-alone checks, tests, docs, the
      cross-target checks for which toolchains are installed; commit and push.

### Task 5: zbus port (Sonnet)

- [ ] Workspace `Cargo.toml`: drop `async-broadcast`; bump the zruntime `rev` to Task 4's commit.
- [ ] `zbus/Cargo.toml`: `comms` drops `dep:async-broadcast` and gains `zruntime/broadcast`.
- [ ] Port `connection/{mod,builder,pending_method_calls,socket_reader}.rs`,
      `connection/socket/channel.rs`, `message_stream.rs` and `tests/issue/issue_173.rs`.
- [ ] Confirm no zruntime broadcast type appears in a public zbus signature.

### Task 6: zbus verification and review (Sonnet, background; review by Opus)

- [ ] Under `dbus-run-session`: `cargo test --all-features`, `cargo test -p zbus
      --no-default-features`, the tokio feature set; clippy; `cargo +nightly fmt --all`.
- [ ] `cargo tree -i event-listener --all-features -e normal` finds nothing; same for
      `event-listener-strategy` and `async-broadcast`.

### Task 7: Report (orchestrator)

- [ ] Append a *Report* section here: commits, deviations, verification, reviews.

## Follow-ups

- async-broadcast upstream: a final release pointing users to `zruntime::broadcast`, or
  deprecation, once zruntime 0.1.0 is on crates.io.
- Notifying the channel's events after letting go of its mutex (Decision 7).
- zbus's `max_queued` taking `NonZeroUsize` before 6.0 (Decision 9).
