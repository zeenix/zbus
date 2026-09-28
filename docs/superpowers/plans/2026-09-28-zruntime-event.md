# Replacing event-listener with zruntime's Event: Implementation Plan

> **For agentic workers:** this plan is executed task by task by subagents, each given the brief
> its task names. Steps use checkbox (`- [ ]`) syntax for tracking. The plan spans two
> repositories: zruntime (Tasks 2 to 5) and zbus (Tasks 6 to 8); zruntime's change lands first,
> because zbus's depends on it.

**Goal:** zbus stops depending on `event-listener` directly, and stops exposing its types in its
public API. It builds instead on a new, runtime-agnostic `Event`/`EventListener` pair in
zruntime, written from scratch: safe Rust only, and no more complicated than event-listener's
own implementation.

**Scope, and what it does not reach:** `async-broadcast` 0.7, which zbus uses for its message
streams and pending method calls, depends on `event-listener` and `event-listener-strategy`
itself (`cargo tree -e normal -p zbus -i event-listener`). So after this plan `event-listener`
is still in every graph with the D-Bus API (`comms`) enabled; only zbus's direct dependency and
its public-API exposure go. Taking it out of the graph altogether is the next plan: porting
`async-broadcast` into zruntime as its broadcast channel, built on this plan's `Event` (see
*Follow-ups*). This plan's `Event` is designed to carry that port.

**Architecture:** zruntime gains `src/event.rs`: an `Event` that tasks listen to and that anyone
notifies, with an `EventListener` that is a `Future`. It needs no runtime, so it works under any
executor, and is always compiled. zbus's `comms` feature starts depending on zruntime (default
features off, so nothing of its runtime is enabled), and every use of `event_listener` in zbus —
its async locks, the connection's activity/closed/drop events, the object server's start-up
handshake, the proxy's property cache, `blocking_thread`, `MessageStream`, the tests and the
docs — moves to it. The two public functions that hand out listeners return zbus types that wrap
zruntime's, keeping zruntime out of zbus's public API as every other zruntime type already is.

**Tech Stack:** Rust 1.87 (MSRV), `std` only for the new type; criterion (through
`codspeed-criterion-compat`) for benchmarks; Miri for the new type's tests.

## Decisions

Each is the recommended choice; the plan is written against it, and a veto changes the task it
names.

1. **Scope** as above: the direct dependency and the public API, not the dependency graph.
2. **zruntime in zbus's graph:** `comms` gains `dep:zruntime`; the `zruntime` feature becomes
   `["comms", "zruntime/helper"]`, and `features = ["helper"]` leaves the workspace dependency.
   (Task 7.) At first zruntime got no feature to compile its runtime out, its whole crate
   building in ~0.5 s (debug). At the maintainer's direction (Task 12), zruntime puts its runtime
   and `Event` behind two default features, `runtime` and `event`, each usable without the
   other; `comms` enables `zruntime/event` alone, so a build without zbus's own backend builds
   none of zruntime's runtime; and zbus's backend feature is renamed from `zruntime` to
   `default-rt`, a name that stays right whichever runtime zbus defaults to.
3. **zbus's public API:** `Connection::monitor_activity` returns a new
   `zbus::connection::ActivityListener`, and `ResponseDispatchNotifier::new` returns a new
   `zbus::object_server::ResponseDispatchListener`; both wrap `zruntime::EventListener` and
   implement `Future<Output = ()>`. An opaque `impl Future` was ruled out: in edition 2024 the
   return type of `ResponseDispatchNotifier::<R>::new` has to capture `R` (`use<>` cannot leave a
   type parameter out), so it could not be `'static` for a non-`'static` `R`. Checked with a
   scratch crate. (Task 6.)
4. **Semantics:** event-listener's (spelled out in *Design* below), so that the port is
   behaviour-preserving. In particular, dropping an `Event` notifies nobody. One difference is
   deliberate: tasks are woken after the event's lock is let go of (S11), where event-listener
   wakes them under its lock, so a listener can complete before the notifying thread is done with
   the waker it is waking. zbus's blocking-work thread was the one place that relied on the
   other order (see *Report*).
5. **Implementation:** no `unsafe`, no atomics of its own: one `std::sync::Mutex` guards all of an
   event's state, and correctness follows from that mutex alone. No lock-free fast path in this
   plan (see *Follow-ups*).
6. **API surface:** only what zbus and zruntime's own tests use, plus `notify_additional`, which
   the counting `notify` needs as a counterpart to be generally usable. Names are
   event-listener's (`Event`, `EventListener`, `listen`, `notify`, `notify_additional`), so the
   port is an import change wherever the API is the same.
7. **Two suspected zbus bugs** found while surveying (see *Follow-ups*) are checked with scratch
   tests and reported, not fixed here.
8. **Delivery:** both repos develop on `claude/sleepy-cori-rlaid7`, based on `upstream/main`
   (z-galaxy), pushed to the `zeenix` forks. Until zruntime's change is merged upstream, zbus's
   workspace dependency points at the fork's commit, with a FIXME. No pull requests are opened
   unless asked for.

## Design: `zruntime::Event`

### API

```rust
pub struct Event { .. }          // Send + Sync + Debug + Default
impl Event {
    pub const fn new() -> Self;
    pub fn listen(&self) -> EventListener;
    pub fn notify(&self, n: usize) -> usize;
    pub fn notify_additional(&self, n: usize) -> usize;
}

pub struct EventListener { .. }  // Future<Output = ()> + Send + Sync + Unpin + 'static + Debug
```

Both are re-exported from the crate root, next to `Runtime`, and are available with any set of
features.

### Semantics

Normative: each item is covered by at least one test in `src/tests/event.rs`, named after the
behaviour.

- **S1, registration:** `listen` puts the listener in the event's queue before it returns. A
  notification sent after `listen` returns reaches the listener, whether or not it has been
  polled yet. This is what the listen-then-check pattern rests on: listen, check the condition,
  and only then wait.
- **S2, no permits:** a notification reaches only the listeners in the queue when it is sent;
  nothing is stored for a listener that comes later.
- **S3, order:** listeners are notified oldest first.
- **S4, `notify(n)`:** makes sure `n` listeners are notified, counting those notified already
  that have not yet resolved or been dropped; the rest of `n` goes to the oldest listeners not
  notified yet. `notify(usize::MAX)` notifies every listener in the queue. Returns how many
  listeners this call notified.
- **S5, `notify_additional(n)`:** notifies `n` more listeners not notified yet, whatever number
  is notified already. Returns how many it notified.
- **S6, resolution:** a notified listener resolves to `()` on its next poll and leaves the queue.
  The task that last polled it is woken when it is notified.
- **S7, passing a notification on:** a listener dropped while notified, before it resolved, passes
  its notification on, in the kind it was given: one from `notify` as `notify(1)` (so only where
  no other listener is still notified), one from `notify_additional` as `notify_additional(1)`.
  A listener dropped before being notified leaves the queue and changes nothing else.
- **S8, lifetime:** a listener keeps the event's shared state alive; the `Event` may be dropped
  first. Dropping the `Event` notifies nobody: a listener notified before still resolves, and
  one that was not can only be reached by a notification passed on (S7) by a listener dropped
  later.
- **S9, polled after resolving:** a listener polled again after it resolved is `Ready` again, at
  once; it never panics and never rejoins the queue.
- **S10, wakers:** only the waker of the latest poll is kept; one that `will_wake` the stored
  waker is not cloned again.
- **S11, lock discipline:** no waker is woken, cloned-then-dropped or dropped while the event's
  lock is held (zruntime's rule, stated in `src/mode.rs`: never hold a lock across a wake or a
  waker's drop). So a waker that comes back into the same event from `wake` — listening,
  notifying, dropping a listener — does not deadlock. The flip side: a listener polled on
  another thread can see its notification and complete while the notifying thread still holds
  the waker it is about to wake, which `Event`'s docs say.
- **S12, auto traits:** `Event: Send + Sync`, `EventListener: Send + Sync + Unpin`, by auto traits
  alone (the crate has no `unsafe impl` and gets none here).
- **S13, memory:** `Event::new` allocates nothing; the shared state is allocated by the first
  `listen` or `notify`, as event-listener does. A listener costs no allocation of its own once the
  queue's storage has grown to the number of listeners alive at once.

### Structure

Guidance for the implementer, who may deviate with a reason given in the commit:

- `Event { list: OnceLock<Arc<Mutex<List>>> }`, initialised with `get_or_init` by both `listen`
  and `notify`, which keeps `new` `const` and gives both the same `List`. `notify` must not use
  `get` to skip a fresh event: without the lock, a `listen` racing it can be missed (a
  store-buffering race that only `SeqCst` fences on both sides close, which is the *Follow-ups*'
  fast path, not this plan).
- `List`: a slab — a `Vec` of slots, vacant ones chained into a free list — whose occupied slots
  are linked by index into a doubly-linked FIFO queue (`head`, `tail`), plus the index of the
  oldest listener not notified yet and the number notified. An entry holds `prev`, `next` and its
  state: waiting (with the waker of its latest poll, if any) or notified (with the kind of
  notification). Notified listeners are always a prefix of the queue, which is what lets the
  counting `notify` start at the first listener not notified and skip the rest.
- `EventListener { list: Arc<Mutex<List>>, key: Option<usize> }`; `key` is `None` once resolved.
- `notify` collects the wakers it takes while holding the lock and wakes them once it has let go
  of it: the first in an `Option<Waker>`, any others in a `Vec`, so that the common single-waker
  case allocates nothing.
- Poisoning is ignored (`PoisonError::into_inner`), as in `mode.rs`'s `Lock`: no code outside
  this module runs under the lock, so a panic cannot leave the list half-changed.

**Why it is race-free:** everything is decided under one mutex. A notifier changes its condition
before `notify` takes the lock; a waiter's `listen` lets go of the lock before the waiter checks
the condition. If `notify` takes the lock after `listen` let go of it, it finds the listener; if
before, its release of the lock happens before `listen`'s acquisition, so the waiter's check
sees the condition changed. No fence is needed, and none is emitted.

### Not in this plan

Tagged notifications, a blocking `wait`, stack-pinned listeners, `no_std`, a lock-free fast path,
and a `Local` flavour built from `Rc`/`RefCell`. zbus needs none of them.

## Design: the zbus side

- **Dependencies:** the root `Cargo.toml` loses `event-listener`;
  `[workspace.dependencies.zruntime]` keeps `version` and `default-features = false`, loses
  `features = ["helper"]`, and points `git` at `https://github.com/zeenix/zruntime` with the `rev`
  of zruntime's branch tip, with the FIXME extended to say it goes back to z-galaxy once that
  change is merged there. `zbus/Cargo.toml`:
  `comms` swaps `dep:event-listener` for `dep:zruntime`; `zruntime = ["comms", "zruntime/helper"]`;
  the `event-listener` dependency line goes. `Cargo.lock` is updated by cargo, not by hand.
- **Code:** every site in `grep -rn "event_listener" zbus/src zbus/tests` moves to
  `zruntime::{Event, EventListener}` with the same calls. `Mutex::new` and `RwLock::new` stay
  `const`. The comment on `Mutex::locked` about fences is rewritten for the mutex-only argument.
- **Public API:** the two newtypes of Decision 3, each a `#[must_use]`, `Debug` tuple struct with a
  `Future` impl that forwards to the inner listener, documented for a first-time reader.
- **Docs:** the doc example in `zbus/src/object_server/mod.rs`; `book/src/service.md` (its example
  also stops calling `wait()` inside `#[tokio::main]` and awaits instead); the statements that
  zbus's locks build on `event-listener` and that a build with neither backend depends on
  nothing zruntime owns, in `book/src/connection.md`, `book/src/upgrading-to-6.md`,
  `zbus/src/runtime/mod.rs`, `zbus/src/runtime/traits.rs`, `zbus/src/runtime/locks/mod.rs` and
  the header of `CI/forbidden-deps.sh`; and a new `###` section under *Other changes in 6.0* in
  `book/src/upgrading-to-6.md` for the two new listener types.

## Global Constraints

- **Code:** MSRV 1.87.0; `cargo +nightly fmt --all` clean; clippy clean with `-D warnings` in
  every configuration of the verification lists below. No `unsafe`, with one exception: tests
  may build a `RawWaker` vtable, the only way to observe a waker being cloned, with each
  `unsafe` block as small as it can be and a `// SAFETY:` comment on it. No `#[allow(...)]`, no
  `ignore`/`no_run` added to doc tests. 100 columns in every file. Items top-down (public API
  first, helpers below their callers, `#[cfg(test)] mod tests` last); `pub` before `pub(crate)`
  before private. Trait bounds in `where` clauses. Where names clash, import the module, never
  rename. Doc titles never start with "Get"/"Return".
- **Comments and docs:** sentences end with `.`; they explain the code to someone reading it for
  the first time, never its history ("now", "no longer", "previously", "used to", "instead of
  event-listener") and never an issue, a review, a benchmark run or this plan. Match the prose of
  the file being edited: zruntime's docs are full sentences that say what a thing is and why.
  User-facing docs do not describe internals.
- **Tests:** no `test_` prefix; every test that can hang has an `ntest` `#[timeout(15000)]`;
  thread-based tests assert outcomes, never timing tighter than the existing suites do.
- **Commits:** check `git config user.name`/`user.email` are `Zeeshan Ali Khan`/`zeenix@gmail.com`
  first. Subject: a curated gimoji emoji, copied from the gimoji database, never typed from memory
  (the variation selector matters), one space, then for zbus `zb: ` and for zruntime nothing,
  then an imperative title; at most 72 characters. Body wrapped at 72, says what and why, with
  measured numbers where there are some. Trailer, and the only one:
  `Assisted-by: Claude Opus 5.5 (claude-opus-5-5)` — never `Co-Authored-By`, never a session
  URL. Commits of this plan's own documents add `Changelog: skip`. Sign commits; commit unsigned
  only where signing fails.
- **Git:** atomic commits in both repos; review fixes are folded into the commit they fix
  (`git commit --fixup` + `git rebase --autosquash`), then force-pushed with
  `--force-with-lease`, to the feature branch only. Only the orchestrator pushes.
- **Subagents** never push, never open a PR, never touch `main`, and report the commands they ran
  with their results.

## Execution

| Task | Owner (model) | Runs alongside |
|------|---------------|----------------|
| 0. Environment and this plan | orchestrator | — |
| 1. Baseline test runs on the base commits | Sonnet | 2 |
| 2. `Event` in zruntime | Opus | 1, 9 |
| 3. Reviews of `Event`, then fixes | Fable, Sonnet (reviews); Opus (fixes) | — |
| 4. zruntime benchmarks, tests on `Event`, dev-dependency gone | Sonnet | — |
| 5. zruntime verification, then push | Haiku, orchestrator | — |
| 6. zbus listener newtypes | Sonnet | — |
| 7. zbus port | Sonnet | — |
| 8. zbus review and fixes | Opus (review), Sonnet (fixes) | — |
| 9. Suspected bugs, confirmed or not | Sonnet | 2 |
| 10. Benchmarks and binary size | Sonnet | nothing: the machine must be quiet |
| 11. Report | orchestrator | — |

Every brief states: the repo and branch, the files to touch, the Global Constraints, the exact
verification commands with the expected outcome, the commit subject and what its body must
cover, and what to report back. Reviewers get a fresh context and the diff, not the implementer's
reasoning. A review finding is fixed, or answered with why not; nothing is left silent.

---

### Task 0: Environment and this plan (orchestrator)

- [x] Toolchains: stable, nightly (`rustfmt`, `clippy`, `miri`, `rust-src`), `1.87.0`; targets
  `x86_64-pc-windows-gnu`, `x86_64-apple-darwin`, `x86_64-unknown-freebsd`,
  `x86_64-unknown-netbsd`, `aarch64-linux-android` for stable and 1.87.0.
- [x] Session bus: `CI/dbus-session.conf` instantiated as CI does into `/tmp/dbus-session.conf`
  and `/tmp/dbus-session-abstract.conf`; `dbus-run-session` works. No `ibus-daemon`, so
  `ibus_connection` is skipped along with CI's `fdpass_systemd`.
- [x] `upstream` remotes for z-galaxy in both repos; both branches start at `upstream/main`.
- [x] A helper that prints a gimoji emoji by code from the gimoji database, for commit subjects.
- [x] Commit this plan to zbus: `📝 Add the plan for replacing event-listener`, with
  `Changelog: skip`; push.

### Task 1: Baseline test runs (Sonnet, background)

Establishes which failures, if any, predate this work, so that none is later blamed on it.

- [ ] zruntime at `upstream/main`: the four `cargo --locked test` variants of
  `.github/workflows/rust.yml`.
- [ ] zbus at `upstream/main`, in a worktree inside the repo (excluded in `.git/info/exclude`):
  the `zruntime`, `tokio`, `external` and `wire` suites of `.github/workflows/rust.yml`, each
  under `dbus-run-session --config-file /tmp/dbus-session.conf`, skipping `fdpass_systemd` and
  `ibus_connection`.
- [ ] Report: each command, pass/fail, and every failing test with its first error lines.

### Task 2: `Event` in zruntime (Opus)

**Files:** create `src/event.rs`, `src/tests/event.rs`; modify `src/lib.rs` (module, re-export),
`src/tests/mod.rs` (`#[cfg(test)] mod event;`, and `compile_fail`-free doc-test checks of S12 if
the implementer prefers them to plain assertions), `README.md` (a short section on `Event`),
`AGENTS.md` (the architecture tree: `event.rs`).

- [ ] Implement the API and the semantics of *Design: `zruntime::Event`*. Module docs explain what
  an event is for, the listen-then-check pattern with a short example that compiles and runs
  (a doc test on `Event`), and what `notify` counts.
- [ ] Tests, one or more per S-item, driving listeners by hand with `Waker::noop()` and with a
  counting waker built on `std::task::Wake`; plus threaded stress tests, each under
  `#[timeout(15000)]` and each shrunk under `cfg(miri)`:
  - a mutex built like zbus's (`AtomicBool` + `Event`, `notify(1)` on release) taken 1000 times by
    each of 8 threads, counting to 8000;
  - listeners on many threads, one `notify(usize::MAX)`, all of them resolve;
  - waiters that drop themselves right after being notified never strand the rest (S7 under
    concurrency);
  - a waker that calls back into the same event from `wake` (S11).
- [ ] Verify: the zruntime list of *Verification*, plus
  `cargo +nightly miri test --lib tests::event` and a release loop
  `for i in $(seq 20); do cargo test --release --all-features --lib tests::event || break; done`.
- [ ] Commit: `✨ Add Event, a notification that tasks can wait for`. Body: what it is, that it
  needs no runtime, the semantics in brief, and that it is safe Rust with one mutex.

### Task 3: Reviews of `Event` (Fable, Sonnet; fixes by Opus)

Two reviewers, in parallel, fresh context, given the commit of Task 2:

- [ ] **Concurrency (Fable):** adversarial. Hunt for a lost wakeup between `listen` and the
  caller's check; a notification lost or duplicated when a listener drops while a `notify` runs on
  another thread; S7's pass-on when the dropped listener's kind differs from the next one's;
  waker re-entrancy and waker drops under the lock; the free-list and link bookkeeping (a slot
  reused while a stale key still points at it, `head`/`tail`/cursor fix-ups on removal);
  behaviour after the `Event` is dropped. Each finding comes with a failing test or a precise
  interleaving.
- [ ] **Conventions (Sonnet):** the Global Constraints; zruntime's prose, naming and top-down order
  (`CONTRIBUTING.md`); the docs are accurate against the code.
- [ ] Opus fixes every confirmed finding, adds the reviewer's failing test where there is one,
  folds the fixes into the Task 2 commit, and re-runs Task 2's verification. A finding not fixed
  gets a written reason in the report.

### Task 4: zruntime benchmarks and tests (Sonnet)

**Files:** create `benches/event.rs`; modify `Cargo.toml` (`[[bench]] name = "event"`,
`harness = false`, no `required-features`; `event-listener` dev-dependency removed in the second
commit), `Cargo.lock`, `src/tests/core.rs`, `src/tests/helper.rs`.

- [ ] Benchmarks, group `event`: `listen-drop`; `notify-none` (an initialised event, no listener);
  `notify-one` (listen, `notify(1)`, poll to `Ready`); `notify-all/100`; `mutex/4-threads` (the
  zbus-style mutex of Task 2, 4 threads × 1000 lock/unlock each, driven by
  `futures_lite::future::block_on`). Commit: `✅ Benchmark Event's listen and notify paths`.
- [ ] A comparison harness, **not committed**, that runs the same ids against
  `event_listener::Event` (still a dev-dependency at this point); kept outside the repo for
  Task 10.
- [ ] Port `src/tests/{core,helper}.rs` to `crate::Event`/`EventListener`; remove the
  dev-dependency; let cargo update `Cargo.lock` (one unlocked `cargo check`, then everything
  `--locked`). Commit: `➖ Test with zruntime's own Event instead of event-listener`.
- [ ] Verify each commit with the zruntime list below.

### Task 5: zruntime verification and push (Haiku, orchestrator)

- [ ] Haiku runs the whole zruntime list at the branch tip and reports each command's result.
- [ ] The orchestrator pushes `claude/sleepy-cori-rlaid7` to `zeenix/zruntime` and records the
  tip's SHA for Task 7.

### Task 6: zbus listener newtypes (Sonnet)

**Files:** create `zbus/src/connection/activity_listener.rs`; modify `zbus/src/connection/mod.rs`
(`monitor_activity`, the `pub use`), `zbus/src/object_server/dispatch_notifier.rs`
(`ResponseDispatchListener` below the notifier), `zbus/src/object_server/mod.rs` (`pub use`).

- [ ] The two types of Decision 3, still wrapping `event_listener::EventListener`, so this commit
  changes the API and nothing else. Their docs say what completes them and when: the next
  activity on the connection; the response being sent off (or the notifier being dropped, which is
  what happens today).
- [ ] Verify: zbus list, clippy part, plus `cargo --locked test -p zbus --doc` for the two items.
- [ ] Commit: `💥 zb: Hand out listeners of zbus's own types`. Body: that callers get a zbus type
  implementing `Future`, and why (a dependency's type in the public API ties zbus's semver to it).

### Task 7: zbus port (Sonnet)

**Files:** `Cargo.toml`, `Cargo.lock`, `zbus/Cargo.toml`, every file `grep -rln event_listener
zbus book CI` lists, and the docs of *Design: the zbus side*.

- [ ] Dependencies as designed, with the `rev` from Task 5.
- [ ] Port every site; `zbus/tests/*` use `zruntime::Event` too (zruntime is a normal dependency
  whenever `comms` is on, so no dev-dependency is added).
- [ ] Docs as designed. `grep -rn "event.listener\|event_listener" --include=*.rs --include=*.md
  --include=*.toml --include=*.sh .` finds nothing outside `docs/superpowers` and `Cargo.lock`
  (where `async-broadcast` still brings it).
- [ ] Verify: the whole zbus list below.
- [ ] Commit: `➖ zb: Build on zruntime's Event instead of event-listener`. Body: what moved, that
  `comms` now depends on zruntime with none of its runtime enabled, and that `event-listener`
  stays in the graph through `async-broadcast`.

### Task 8: zbus review (Opus; fixes by Sonnet)

- [ ] Fresh-context review of Tasks 6 and 7: behaviour preserved at every site (listener taken
  before the check it guards; `notify(1)` versus `notify(usize::MAX)` unchanged;
  `receive_property_changed`'s pre-notification; `graceful_shutdown` and `ResponseDispatchNotifier`
  relying on S8); feature gating in every CI configuration; the new public types' docs; every doc
  statement about dependencies true after the change.
- [ ] Fixes folded into the commit they belong to; zbus list re-run.

### Task 9: Suspected bugs (Sonnet, scratch only)

Nothing here is committed. For each, a test in a scratch worktree that fails if the bug is real:

- [ ] **Builder hang:** `Builder::build` awaits the object server task's `started_event`, and that
  task returns without notifying it when `add_match` fails or the connection is gone, leaving
  `build()` waiting forever.
- [ ] **Initial property value:** `Proxy::receive_property_changed` listens and then calls
  `notify(1)` to make the new stream yield the current value first; with another stream on the
  same property already waiting, `notify(1)` reaches that older listener instead, so the new
  stream does not yield the current value and the old one yields a spurious change.
- [ ] Report: confirmed or not, with the test's output.

### Task 10: Benchmarks and binary size (Sonnet, quiet machine)

- [ ] zruntime: the comparison harness of Task 4, `event_listener::Event` against `zruntime::Event`.
- [ ] zbus end to end: zbus's own connection benchmarks, taken from `zbus/benches/runtime.rs` at
  `07cd5ea^` (before they moved to zruntime) and adapted to the current API, in two worktrees —
  `upstream/main` and the branch — with nothing committed. Ids: `connection/build-and-drop`,
  `connection/graceful-shutdown`, `method-call/roundtrip`, `method-call/1MiB-body`,
  `signal/emit-receive`, on zruntime and on Tokio.
- [ ] Method: base and branch alternate, three runs each, `-- --noplot`; report the median of the
  `time:` point estimates and the spread; a difference is claimed only when it exceeds the spread.
- [ ] `CI/binary-size.sh upstream/main` on the branch. Growth is possible and expected to be
  small: both implementations are linked while `async-broadcast` keeps `event-listener`.

### Task 11: Report (orchestrator)

- [ ] Fill in *Report* below: commits and SHAs, verification results, benchmark and size tables,
  review findings and what became of them, Task 9's verdicts.

### Task 12: Feature gates in zruntime, `default-rt` in zbus (Sonnet; verification by Haiku)

Added after the report, at the maintainer's direction.

- [ ] zruntime, one commit on top, `🚩 Put the runtime and Event behind cargo features`:
  `default = ["runtime", "event", "tracing"]`; `runtime` turns on the optional `rustix` and
  `windows-sys` and gates the runtime's modules and public items; `event` gates `Event` and
  `EventListener`; `helper` implies `runtime`. Every combination builds and lints warning-free,
  the empty one included. `event.rs` keeps a lock helper of its own, since the runtime's may be
  compiled out. `Event`'s doc examples use none of the runtime (they poll by hand, or drive a
  future with `futures-lite`, a dev-dependency), so an `event`-only build runs them; the README
  is the crate's documentation only with `runtime`, whose examples it holds. Tests of the
  runtime that use `Event` are gated on `event`; the benchmarks list the features they use.
  CI's `--no-default-features` jobs become `--no-default-features --features runtime`, and
  `event`-only and empty builds are added. The README and `AGENTS.md` say what each feature is.
- [ ] zbus, the port commit: `comms` enables `zruntime/event`; the backend feature turns on
  `zruntime/runtime` and `zruntime/helper`; the `rev` moves to zruntime's new tip; the docs and
  the commit body say that a build without the backend builds none of zruntime's runtime.
- [ ] zbus, one commit after the port, `🚚 zb: Rename the zruntime feature to default-rt`: every
  `cfg`, the feature itself, the default list, docs.rs metadata, `required-features`, the
  fixtures' `zbus` features, CI and every doc that names the feature (not the crate or the
  runtime, which keep their name).
- [ ] Verify: the whole zruntime list, with the `event`-only, `runtime`-only and empty builds;
  the whole zbus list with `default-rt` in place of `zruntime`; `cargo tree -e features` showing
  zruntime with `event` alone in the Tokio-only and external-only builds; `CI/binary-size.sh`.

## Verification

From Task 12 on, zbus's `zruntime` feature is named `default-rt`; the commands below use the name
it had before.

**zruntime** (per commit; from `.github/workflows/rust.yml`, `RUSTFLAGS=-D warnings`):

```sh
cargo +nightly fmt --all -- --check
for f in "" --no-default-features --all-features "--no-default-features --features helper"; do
    cargo --locked clippy --all-targets $f -- -D warnings
done
for t in x86_64-pc-windows-gnu x86_64-apple-darwin x86_64-unknown-freebsd \
    x86_64-unknown-netbsd aarch64-linux-android; do
    for f in "" --no-default-features --all-features; do
        cargo --locked clippy --all-targets --target $t $f -- -D warnings
        cargo +1.87.0 --locked check --target $t $f
    done
done
for f in "" --no-default-features --all-features; do cargo +1.87.0 --locked check $f; done
for f in "" --no-default-features --all-features "--no-default-features --features helper"; do
    cargo --locked test $f
done
RUSTDOCFLAGS="-D warnings" cargo --locked doc --all-features
RUSTDOCFLAGS="-D warnings" cargo --locked doc --no-default-features
```

**zbus** (at the tip of Tasks 6, 7 and 8; from `.github/workflows/rust.yml`; `D=dbus-run-session
--config-file /tmp/dbus-session.conf --`; `SKIP="--skip fdpass_systemd --skip ibus_connection"`):

```sh
cargo +nightly fmt --all -- --check
cargo --locked clippy -- -D warnings   # and each --target of the zruntime list
# Every `cargo --locked clippy -p zbus --no-default-features ...` line of the clippy job,
# the fixtures lines included.
cargo +1.87.0 --locked check           # and the MSRV job's other lines
$D cargo --locked test --release --all-features -- $SKIP
$D cargo --locked test --release -p zbus \
    --features uuid,url,time,chrono,option-as-array,vsock,bus-impl -- $SKIP
$D cargo --locked test --release --doc -p zbus --no-default-features --features zruntime,proxy
$D cargo --locked test --release --doc -p zbus --no-default-features --features zruntime,service
$D cargo --locked test --release --tests -p zbus --no-default-features \
    --features tokio,proxy,service -- $SKIP
$D cargo --locked test --release --doc -p zbus --no-default-features --features tokio,service
$D cargo --locked test --release -p zbus --features tokio,p2p,vsock -- $SKIP
$D cargo --locked test --release -p zbus --no-default-features \
    --features proxy,service,object-manager,unixexec,ibus,tracing,p2p --tests
cargo --locked test --release -p zbus --no-default-features
dbus-run-session --config-file /tmp/dbus-session-abstract.conf -- \
    cargo --locked test --release -- basic_connection
CI/forbidden-deps.sh default
CI/forbidden-deps.sh tokio-only --no-default-features --features tokio,proxy,service
CI/forbidden-deps.sh external-only --no-default-features \
    --features proxy,service,object-manager,unixexec,ibus,tracing,p2p
for f in --all-features --no-default-features \
    "--no-default-features --features zruntime,proxy" \
    "--no-default-features --features zruntime,service"; do
    RUSTDOCFLAGS="-D warnings" cargo --locked doc -p zbus $f
done
```

CI's `semver-checks` job is not run locally.

## Follow-ups

Not part of this plan; listed so that none is lost.

- **`async-broadcast` into zruntime:** the next plan ports it (it is maintained by zbus's
  maintainer) into zruntime as a broadcast channel built on `Event`, and moves zbus onto it, which
  is what takes `event-listener` and `event-listener-strategy` out of zbus's graph. What it needs
  from `Event` is `listen`, `notify(1)`, `notify(usize::MAX)` and listeners held in its send and
  receive futures, all of which this plan provides; its `*_blocking` methods, which wait through
  `event-listener-strategy`, need a blocking wait on a listener in addition, which that plan adds
  (or leaves those methods out). zbus's `Stream` use of the receiver needs either a
  `futures-core` dependency in zruntime or an inherent `poll_recv` that zbus wraps.
- **Task 9's bugs**, where confirmed: each wants a failing test and a fix of its own.
- **A lock-free fast path** for `notify` on an event nobody listens to (an atomic count of
  waiting listeners, or a `get` on the `OnceLock`), which needs `SeqCst` fences in both `listen`
  and `notify` and a loom test. Worth it only if Task 10 shows the lock on that path costing
  something measurable end to end.
- **zbus's async locks in zruntime**, next to `Event`, if other users want them.
- **Merging:** zruntime's change first; then zbus's `rev` goes back to a z-galaxy commit.

## Report

To be filled in by Task 11.
