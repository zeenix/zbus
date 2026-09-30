# Moving zbus's async locks into zruntime: Implementation Plan

> **For agentic workers:** this plan is executed task by task by subagents, each given the brief
> its task names. Steps use checkbox (`- [ ]`) syntax for tracking. The plan spans two
> repositories: zruntime (Tasks 1 to 5) and zbus (Tasks 6 to 8); zruntime's change lands first,
> because zbus's depends on it.

**Goal:** zbus's async `Mutex` and `RwLock` (`zbus/src/runtime/locks/`), built on zruntime's
`Event`, move into zruntime as public API: a `lock` module behind a new `lock` cargo feature. zbus
takes them from there. zruntime's own tests and benchmarks drop the locks they build by hand to
drive `Event` (`TestLock` in `src/tests/event.rs`, `Mutex` in `benches/event.rs`), and the
connection benchmark writes through zruntime's `Mutex`, as zbus's connection does, instead of
`futures_util::lock::Mutex`.

**Scope:** the two locks and their guards, with the semantics they have in zbus, made public, plus
the few conveniences a public lock type is expected to have (Decision 3). Not in scope: named
futures for `lock`, `read` and `write`, owned (`Arc`) guards, mapped guards, `downgrade` or
upgradable reads, fairness, a semaphore (see *Follow-ups*); and zbus's use of Tokio's locks on a
Tokio build, which stays (Decision 4).

**Tech Stack:** Rust 1.87 (MSRV), `std` only; criterion (through `codspeed-criterion-compat`) for
the benchmarks.

## Decisions

Each is the recommended choice; the plan is written against it, and a veto changes the task it
names.

1. **Module and feature:** the locks live at `zruntime::lock` (`Mutex`, `MutexGuard`, `RwLock`,
   `RwLockReadGuard`, `RwLockWriteGuard`), not re-exported at the crate root, as `broadcast`'s
   items are not. They are behind a non-default `lock` feature that implies `event`, as
   `broadcast` does; it needs no runtime and adds no dependency. zbus's `comms` turns it on.
   The consequence to veto, if any: once the benchmarks use the module (Decision 6), the
   documented `cargo bench --features helper` skips the `event` and `connection` targets, and
   the command becomes `cargo bench --features helper,lock`; making `lock` a default feature
   instead would keep the old command working. (Task 1, Task 6)
2. **Semantics unchanged:** the code moves as it is. `Mutex` is a flag taken with an `Acquire`
   compare-exchange and an `Event` notified once per release; `RwLock` is write-preferring, a
   waiting writer counting as waiting for as long as its future lives; neither poisons and
   neither is fair. The docs are rewritten for zruntime's users rather than for a zbus
   connection, and state what a user has to know: no poisoning, no fairness, write preference,
   that asking for a second read guard while holding one can deadlock, and that dropping a
   `lock`, `read` or `write` future before it completes is safe. (Task 1)
3. **API additions:** `try_lock`, `try_read` and `try_write` become public (they exist, private,
   for `Debug`); both locks gain `get_mut`, `into_inner`, `Default` and `From<T>`; the three
   guards gain `Debug`, delegating to the value (C-DEBUG). None of them needs `unsafe` of its own
   (`UnsafeCell::get_mut` and `UnsafeCell::into_inner`). `lock`, `read` and `write` stay
   `async fn`s, as Tokio's are: a named future type can replace them later without breaking a
   caller that awaits them. (Task 1)
4. **zbus keeps Tokio's locks on a Tokio build:** Tokio's locks are fair (FIFO) and zruntime's
   are not, so switching a Tokio build to zruntime's would change behavior, which a move should
   not. zruntime's `lock` module is built in every `comms` build from now on, so the docs'
   reason for the switch, that a Tokio build "carries no second lock implementation", goes: they
   state the fact alone, that a Tokio build uses Tokio's locks, with no reason that rests on how
   the code is compiled. (Task 6)
5. **Event tests move with the lock:** the two tests driving `Event` through `TestLock` move to
   `src/tests/lock.rs` and drive `lock::Mutex` instead. The holder-count test merges with zbus's
   `a_mutex_is_shared_across_threads`. In the come-back test, the lock's event is private, so the
   waker comes back into the lock through its public API: its wake polls a fresh `lock()` once
   and drops whatever that gave (a listener, or a guard whose drop notifies), then wakes its
   task. It no longer adds a spurious notification per wake (`notify_additional(1)`); the
   single-threaded come-back tests in `event.rs` still cover `notify_additional` from a waker.
   `TestLock`, `ComingBack` and `ComeBackThenWake` go. (Task 2)
6. **Benchmarks keep their ids:** `event/mutex/4-threads` stays in `benches/event.rs` and times
   `lock::Mutex<()>`, so its CodSpeed history goes on; the `event` bench then requires `lock`.
   `benches/connection.rs` writes through `lock::Mutex`; its `connection/*` ids measure that
   plumbing change. CodSpeed's build adds `lock`. (Task 2)
7. **zbus's pin:** zbus's workspace points its zruntime dependency at the zeenix fork and the tip
   of this change there, with the FIXME to go back to `z-galaxy/zruntime` once it is merged, as
   it did for `Event`. (Task 6)

## Design: `zruntime::lock`

- `src/lock/mod.rs`: the module docs (what the locks are for — a guard held across an await,
  which `std::sync`'s cannot be, since its guards are `!Send` and it blocks the thread; built on
  `Event`; need no runtime, work under any executor, can be taken from any thread; the
  no-poisoning and no-fairness notes of Decision 2; a short example), `mod mutex; mod rwlock;`
  and the re-exports.
- `src/lock/mutex.rs` and `src/lock/rwlock.rs`: zbus's `mutex.rs` and `rwlock.rs`, `pub(crate)`
  made `pub`, with the additions of Decision 3 and public docs (with examples) on every public
  item. Their internal comments, `SAFETY` comments and `Send`/`Sync` bounds are kept word for
  word unless a reviewer finds them wrong. The value stays the last field of both locks, which
  `Arc<Lock<T>>` to `Arc<Lock<dyn Trait>>` coercion needs.
- Items ordered top-down per CONTRIBUTING.md; doc examples use `futures_lite::future::block_on`
  (a dev-dependency), so they run with `lock` alone.

Public items: `Mutex`, `MutexGuard`, `RwLock`, `RwLockReadGuard`, `RwLockWriteGuard`.

Tests, in `src/tests/lock.rs`, gated on the feature:

- zbus's eight, moved (`a_mutex_is_shared_across_threads` merged with `event.rs`'s holder-count
  test, keeping its `#[timeout]` and its smaller Miri sizes), and `event.rs`'s come-back test
  (Decision 5).
- New: `try_lock`, `try_read` and `try_write` against a held and a free lock (and `try_read`
  against a waiting writer); `get_mut`, `into_inner`, `Default` and `From`; the `Debug` output of
  a free lock, a held one and each guard; a `Mutex` coercing to an unsized value; readers and
  writers on many threads, the writers updating a pair in two steps and the readers checking
  that they never see it half-updated.
- Compile-time checks of the `Send`/`Sync` bounds, as doc tests in `src/tests/mod.rs` like the
  runtime's: `compile_fail` for `Mutex<Rc<_>>` being `Send` or `Sync`, `RwLock<Cell<_>>` being
  `Sync`, `MutexGuard<'_, Rc<_>>` being `Send`, `MutexGuard<'_, Cell<_>>` being `Sync`,
  `RwLockReadGuard<'_, Cell<_>>` being `Send`, and `RwLockWriteGuard<'_, Cell<_>>` being `Sync`;
  and one that compiles, showing each of them `Send` and `Sync` where the bounds hold, so the
  failures fail for the reason they were written for.

## Global Constraints

- Commits: gimoji prefix copied from the gimoji database (no package prefix in zruntime; `zb:` in
  zbus), atomic, author `Zeeshan Ali Khan <zeenix@gmail.com>` (check `git config` before each
  commit), an `Assisted-by:` trailer and no co-author lines or session links. This plan's commits
  end with a `Changelog: skip` trailer.
- 100 columns in code, comments and Markdown; no trailing whitespace; `cargo +nightly fmt`.
- No `#[allow(dead_code)]`, no `ignore`/`no_run` doc tests. `unsafe` only where zbus's locks have
  it, each block with its `SAFETY` comment.
- Subagents edit only the files their task lists, do not commit and do not run `cargo fmt` (two
  of them may share a working tree); the orchestrator formats and commits.
- Push to the `zeenix` forks (`origin`) on `claude/jolly-goodall-ym5xsy`; no PRs unless asked.

## Execution

### Task 0: This plan (orchestrator)

- [ ] Commit this plan to zbus.
- [ ] Baseline: `cargo test --all-features` in zruntime; zbus's lock tests
      (`cargo test -p zbus --lib runtime::locks`).

zruntime gets two commits, each of which passes CI on its own:

1. the module: the feature, `src/lock/`, `lib.rs`, `src/tests/lock.rs` and `src/tests/mod.rs`,
   the docs' feature lists and the CI check (Tasks 1 and 3, run side by side: their files are
   disjoint). At this commit, `cargo bench --no-run --features helper,broadcast`, the set CI
   builds then, still builds.
2. the tests and benchmarks off their own locks: `src/tests/event.rs`, the two benchmarks, their
   `required-features`, `bench.yml` and `AGENTS.md`'s benchmark commands (Task 2, after the
   first commit).

### Task 1: The module and its tests (Sonnet)

- [ ] Feature `lock = ["event"]` in `Cargo.toml`, with a comment like the others;
      `#[cfg(feature = "lock")] pub mod lock;` in `lib.rs`.
- [ ] `src/lock/{mod,mutex,rwlock}.rs` per the design.
- [ ] `src/tests/lock.rs` with the moved and new tests (the come-back test comes in Task 2);
      `src/tests/mod.rs`: `mod lock`, the `Send`/`Sync` doc tests, and its header.
- [ ] `cargo check --no-default-features --features lock`, `cargo check --all-features`,
      `cargo test --all-features`, `cargo test --no-default-features --features lock`.

### Task 2: Tests and benchmarks off their own locks (Sonnet, after the first commit)

- [ ] `src/tests/event.rs`: the two `TestLock` tests move to `src/tests/lock.rs` (Decision 5);
      `TestLock`, `ComingBack`, `ComeBackThenWake` and any import left unused go; the header's
      "through a lock built on it" goes with them.
- [ ] `benches/event.rs`: `mutex/4-threads` on `lock::Mutex<()>`, the hand-built `Mutex` gone,
      the module docs updated; `benches/connection.rs`: `zruntime::lock` in place of
      `futures_util::lock`, keeping the `lock::Mutex` spelling (`std::sync::Mutex` is imported
      there as `Mutex`).
- [ ] `Cargo.toml`: `required-features` of `event` becomes `["lock"]`, of `connection`
      `["helper", "lock"]`; `.github/workflows/bench.yml` builds with `helper,broadcast,lock`;
      `AGENTS.md`'s benchmark commands say which features each target needs.
- [ ] `cargo bench --no-run --features helper,broadcast,lock`; `cargo test --all-features`.

### Task 3: Docs and CI (Sonnet, beside Task 1)

- [ ] `README.md`: `lock` in *Features*, with a link reference like `broadcast`'s.
- [ ] `src/event-only.md`: `lock` in *Features*, plain text (the build it is shown in may lack
      the module).
- [ ] `AGENTS.md`: features paragraph, the `--features lock` check among the commands,
      architecture tree, *Features* guideline, key files; not the benchmark commands (Task 2).
- [ ] `.github/workflows/rust.yml`: `cargo --locked check --no-default-features --features lock`
      beside the other feature-alone checks, and its step name and comment.

### Task 4: Review (Opus, and the advisor; fixes by Sonnet)

- [ ] The moved code against zbus's, line by line: no semantic drift, the `SAFETY` comments and
      `Send`/`Sync` bounds, the additions of Decision 3, docs (accuracy, no internals, style),
      item order, the tests' coverage and that each fails without what it checks.
- [ ] In particular: `value` is still the last field of both locks (zbus's object server coerces
      `Arc<RwLock<T>>` to `Arc<RwLock<dyn Interface>>`); `into_inner` moves out of `self.value`,
      which holds only while neither lock implements `Drop`; the public `try_write` takes a free
      lock ahead of writers counted as waiting, and its docs say so; `RwLock`'s `Debug` shows
      `<locked>` while a writer only waits; the come-back test polls its fresh `lock()` with
      `Waker::noop()` behind `pin!`, and its chain of wakes is bounded (a guard dropped inside a
      wake calls `notify(1)`, a no-op while the woken listener is still notified and unpolled).

### Task 5: zruntime verification and push (orchestrator)

- [ ] nightly fmt; clippy (`--all-targets --all-features`, `-D warnings`); `runtime`, `event`,
      `broadcast` and `lock` each checked alone; `cargo test --all-features`;
      `cargo test --no-default-features --features lock`; docs with `-D warnings`, with all
      features and with `lock` alone; the benchmark build; the other targets and 1.87.0 where
      their toolchains can be installed. Each commit is checked on its own.
- [ ] The holder-count, come-back and readers-and-writers tests, 50 runs in a row.
- [ ] Miri (`rustup +nightly component add miri`, then `cargo +nightly miri test
      --no-default-features --features lock`), which is what checks the moved `unsafe`; if it
      cannot be installed here, the report says so.
- [ ] Commit (the module, then the tests and benchmarks), push.

### Task 6: zbus port (Sonnet)

- [ ] Workspace `Cargo.toml`: zruntime from `https://github.com/zeenix/zruntime` at Task 5's tip,
      the FIXME saying the source goes back to `z-galaxy/zruntime` once merged there;
      `cargo update -p zruntime`, so that `Cargo.lock` follows.
- [ ] `zbus/Cargo.toml`: `comms` gains `zruntime/lock`; the comment on the zruntime dependency.
- [ ] `zbus/src/runtime/locks/` becomes `zbus/src/runtime/locks.rs`, re-exporting zruntime's
      locks or, on a Tokio build, Tokio's, as today, with the same `service` gate on the
      readers-writer lock; its tests are gone with the code.
- [ ] Docs saying the locks are zbus's own: `zbus/src/runtime/mod.rs`, `zbus/README.md`,
      `book/src/connection.md`, `book/src/upgrading-to-6.md` (Decision 4's wording), and the
      header of `CI/forbidden-deps.sh`, which also lists the zruntime features a tree has.
      `book/src/connection.md` also says zbus builds zruntime "with its `event` feature alone",
      untrue since `broadcast`: it names `event`, `broadcast` and `lock`.
- [ ] No zruntime lock type appears in a public zbus signature (the object server's guards sit in
      `pub(super)` fields of public types).

### Task 7: zbus verification (Sonnet, background)

- [ ] Under `dbus-run-session`, the suites of zbus's CI (`.github/workflows/rust.yml`), with the
      commands and feature sets it gives them: all features; `zruntime` (the `default-rt`
      suite); `tokio`; `tokio-zruntime`; external, `comms` with neither `default-rt` nor
      `tokio`, the build in which `comms` turning on `zruntime/lock` is what gives zbus its
      locks; and wire. `CI/forbidden-deps.sh` for the Tokio-only and external-only builds.
      Failures compared with the base's (the vsock, helper-process, unixexec and
      `fdpass_systemd` tests fail in this container regardless).
- [ ] `cargo check -p zbus --no-default-features --features comms` with `-D warnings`: without
      `service`, a readers-writer lock re-export left ungated would warn as unused.
- [ ] Clippy as CI runs it; nightly fmt; docs.

### Task 8: Review and report (Opus, the advisor, orchestrator)

- [ ] Review the zbus diff and the change as a whole; fixes; append a *Report* section here;
      commit; push.

## Follow-ups

- Named `Unpin` futures for `lock`, `read` and `write`, for a caller that keeps the wait in a
  struct field (an `AsyncWrite` behind a lock, say).
- Owned guards (`lock_arc` and friends), mapped guards, `RwLockWriteGuard::downgrade`.
- Starvation: a woken waiter that loses the race to a newcomer queues again at the back;
  async-lock bounds this with a starvation mode.
- The poison-tolerant `lock` helper has three copies in zruntime (`event.rs`, `broadcast.rs`,
  `lock/rwlock.rs`).
- zbus's Tokio switch can go once zruntime's locks are fair.
- zbus's zruntime source goes back to `z-galaxy/zruntime` once this change is merged there.
