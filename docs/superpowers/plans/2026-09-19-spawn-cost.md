# Spawn Cost on the Built-in Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring `spawn/100-tasks` on the built-in runtime from 68 µs to the async-io level
(24 µs) by letting a thread that enters `zbus::block_on` take the seat from the helper thread,
and remove three measured wastes on the per-task path.

**Architecture:** The profile (`wt-profile/PROFILE.md`, taken at 8cc85f81) shows the runtime
level with async-io when the spawning thread holds the seat (24.1 µs) and 2.8× slower when a
helper holds it (68.4 µs): two threads fight over the one `Scheduler::state` mutex (26 % of
cycles in lock contention), the ready queue's cache lines bounce, and a burst costs ~4 pipe
writes. The fix that addresses the 3× is a hand-off: a `block_on` that finds the helper in the
seat asks for it, the helper frees the seat at the end of its round and parks (it stays up),
the `block_on` drives, and when it leaves with work behind the helper is unparked and takes the
seat back; a parked helper leaves once nothing is left. Three per-task wastes go too: a finished
handle's drop builds an `io::Error` nobody reads (3 allocations per task), the `live` map hashes
a dense counter with SipHash, and `Signal::wake_by_ref` takes the process-wide registry lock on
every wake.

**Tech Stack:** Rust 1.87, criterion. No new dependencies, no atomics, no `unsafe`.

**Spec:** the maintainer's statement of 2026-09-19 ("task spawning ... should perform much
better than before as there's no thread overhead involved") and `wt-profile/PROFILE.md`
(the measurements and the ranked list; items 1, 3, 6 and 8 are this plan; items 2, 4, 5, 7, 9,
10 are not).

## Global Constraints

- MSRV 1.87.0; `cargo +nightly fmt --all` clean; `cargo clippy -- -D warnings` clean on every
  commit for: `-p zbus --all-targets --features p2p`, `-p zbus --all-features --all-targets`,
  `-p zbus --no-default-features --features tokio,proxy,service,p2p --all-targets`, and
  `cargo check --target x86_64-pc-windows-gnu -p zbus --all-targets --features p2p`. Runtime
  tests: `cargo test -p zbus --features p2p --lib runtime::builtin`, plus the release loop
  `for i in 1 2 3 4 5 6 7 8 9 10; do cargo test -p zbus --release --features p2p --lib
  runtime::builtin::tests -- --test-threads=4 || break; done`; at the tip, the session-bus suite
  `dbus-run-session --config-file /tmp/dbus-session.conf -- cargo test -p zbus --all-features --
  --skip fdpass_systemd --skip ibus_connection`.
- No `#[allow(...)]`, no atomics (flags live under the mutex that guards the state they
  describe; thread-locals hold a `Cell`), no `unsafe`, no dependency changes.
- 100 columns; sentences in comments end with `.`; comments explain the code to a first-time
  reader — never "now"/"no longer"/"previously"/"yet"/"used to", never a reference to an issue,
  a review, a profile or a commit. Tests: no `test_` prefix; each on its own runtime through
  `runtime()`; no timing assertions tighter than the existing `within_a_second`.
- Commits: one logical change each; subject a gimoji emoji + ` zb: ` + imperative title, header
  ≤ 72 UTF-16 units (⚡️ counts 2, ✅ counts 1, ✨ counts 1), body ≤ 74 chars per line, body says
  why with the measured number; trailer exactly `Assisted-by: Claude Fable 5.1 (claude-fable-5-1)`;
  never Co-Authored-By, never a session URL, never another model's name. Commit with
  `/usr/bin/git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign`;
  `git config user.name`/`user.email` = `Zeeshan Ali Khan`/`zeenix@gmail.com`.
- Tooling: `git`, `grep`, `cargo` on `PATH` are shims; use `/usr/bin/git`, `/usr/bin/grep`,
  `~/.cargo/bin/cargo`; every cargo call wrapped in `flock /tmp/claude-1000/cargo.lock` with
  `CARGO_TARGET_DIR=/home/zeenix/checkout/z-galaxy/zbus/target`. Benchmarks: `cargo bench -p
  zbus --bench runtime --features p2p -- --noplot '<filter>'`, three runs, median of the
  `time:` point estimates; the machine is shared, so a change is only claimed when it exceeds
  the spread of the three runs.
- Branch `builtin-runtime-plan`, on top of its current tip (after the split onto
  `block-on-public`); push only to `zeenix` as `builtin-runtime` with `--force-with-lease`.

## Verification per commit

The clippy/check list and the runtime tests above, per commit; the release loop and the
session-bus suite at the tip of Task 2 and of Task 3.

---

### Task 1: The benchmark that shows the seat path, and three per-task wastes

**Files:**
- Modify: `zbus/benches/runtime.rs` (a new group before every other)
- Modify: `zbus/src/runtime/builtin/scheduler.rs` (`JoinHandle::drop`; `State::live`'s hasher)
- Modify: `zbus/src/runtime/builtin/driver.rs` (`Signal`)
- Test: `zbus/src/runtime/builtin/tests.rs` (one new test), the runtime tests, the benches.

**Interfaces:**
- Produces: bench id `spawn/100-tasks-inside`; `IdHasher` (private to `scheduler.rs`);
  `Signal { runtime: Mutex<Weak<Inner>> }` in place of calling `resolve` on every wake.

Four commits, in this order.

- [ ] **Step 1: The benchmark (✅)**

`wt-profile/zbus/benches/runtime.rs` holds the uncommitted version of this change (a group
placed before `connection`); reproduce it on the branch, do not copy the worktree's file. In
`fn runtime`, before `let mut group = c.benchmark_group("connection");`, add a group whose
doc says what the existing `spawn/100-tasks` measures (a helper thread holds the seat, the
bench thread parks and every spawn and every awaited handle crosses threads) and what this one
measures (the pair is built inside the same `block_on`, so the bench thread is in the seat and
no thread of zbus's own exists):

```rust
        // `spawn/100-tasks` below spawns from a thread that parks behind the helper thread the
        // connection pair left in the seat; here the pair is built inside the same `block_on`,
        // so the spawning thread is the one in the seat and no thread of zbus's own exists.
        let mut group = c.benchmark_group("spawn");
        group.throughput(Throughput::Elements(100));
        group.bench_function("100-tasks-inside", |b| {
            b.iter(|| {
                zbus::block_on(async {
                    let (_server, client) = pair().await;
                    let tasks: Vec<_> = (0..100u32)
                        .map(|i| client.spawn("bench", async move { i }))
                        .collect();
                    for task in tasks {
                        black_box(task.await.unwrap());
                    }
                })
            });
        });
        group.finish();
```

Criterion allows the same group name twice in one run only if the ids differ; if it objects,
name the group `spawn-inside` and the id `100-tasks`. Run it once (`-- 'spawn/'`), record both
ids' numbers in the report. Commit: `✅ zb: Benchmark a spawn burst from the thread in the seat`,
body: the other spawn id measures a caller parked behind the helper; this one measures the
thread running the runtime itself.

- [ ] **Step 2: A finished handle's drop builds no error (⚡️)**

`JoinHandle::drop` in `scheduler.rs` takes the future out of the cell and then, whatever the
stage was, calls `self.cell.joint.fail(io::Error::other("the task was cancelled"))` and the
scheduler's `notify` hook. For a task that has finished (`Stage::Done`) there is nothing to
cancel, no waiter that could see the error, and nothing the runtime learns from the notify.
Change the match so that the `Stage::Done` arm returns from `drop` at once:

```rust
                // Finished, or cancelled by an earlier drop: nothing to take back, nobody to
                // tell.
                Stage::Done => return,
```

(placed inside the `match` that runs under the cell lock; the guard drops on return). Keep the
`Idle` and `Running` arms and everything after the match as they are. Test, in `tests.rs`:

```rust
#[test]
#[timeout(15000)]
fn dropping_a_finished_handle_fails_nothing() {
    let runtime = runtime();
    let task = runtime.spawn("a task that finishes at once", async { 1 });
    let output = drive(&runtime, async { task.await });

    assert_eq!(output.unwrap(), 1);
}
```

is not enough on its own (the handle is consumed by `await`); what the change removes is
observable only as work, so the test that guards it is the existing
`a_spawned_task_hands_its_output_back` plus this one, which drops a finished handle without
awaiting it and checks the runtime is untouched:

```rust
#[test]
#[timeout(15000)]
fn a_finished_task_whose_handle_is_dropped_leaves_nothing_behind() {
    let runtime = runtime();
    let task = runtime.spawn("a task that finishes at once", async {});
    // Run the task to completion on this thread, keeping the handle alive past it.
    drive(&runtime, async {
        std::future::poll_fn(|cx| {
            cx.waker().wake_by_ref();
            std::task::Poll::<()>::Ready(())
        })
        .await
    });
    assert!(within_a_second(|| !runtime.inner().is_busy()));

    drop(task);

    assert!(!runtime.inner().is_busy());
    assert!(!runtime.helper_running());
}
```

(`Inner::is_busy` is `pub(super)`; `Builtin::inner` is `#[cfg(test)]`.) Measure `spawn/` (both
ids, three runs). Commit: `⚡️ zb: Drop a finished task's handle without a cancellation error`,
body with the measured µs.

- [ ] **Step 3: Task ids hash as themselves (⚡️)**

In `scheduler.rs`, add `hash::{BuildHasherDefault, Hasher}` to the `std` imports and:

```rust
/// Hashes a task id as itself.
///
/// Ids come from a counter, so they are spread as evenly as a hash could spread them, and the
/// default hasher's defence against chosen keys buys nothing for keys no caller chooses.
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 << 8) | u64::from(*byte);
        }
    }

    fn write_u64(&mut self, id: u64) {
        self.0 = id;
    }
}
```

and `live: HashMap<u64, Arc<TaskCell>, BuildHasherDefault<IdHasher>>` (constructed with
`HashMap::default()`). Existing tests cover the map. Measure. Commit: `⚡️ zb: Hash task ids
as themselves in the live map`.

- [ ] **Step 4: A `block_on`'s waker keeps the runtime it found (⚡️)**

In `driver.rs`, `Signal` gains `runtime: Mutex<Weak<Inner>>` (starts `Weak::new()`), and
`wake_by_ref` becomes:

```rust
    fn wake_by_ref(self: &Arc<Self>) {
        *lock(&self.woken) = true;
        self.thread.unpark();
        // The runtime is looked up through the registry once, then kept: a wake is on the hot
        // path of every reply, and the registry's lock is the whole process's.
        let inner = {
            let mut runtime = lock(&self.runtime);
            match runtime.upgrade() {
                Some(inner) => Some(inner),
                None => {
                    let inner = (self.resolve)();
                    if let Some(inner) = &inner {
                        *runtime = Arc::downgrade(inner);
                    }

                    inner
                }
            }
        };
        if let Some(inner) = inner {
            inner.reactor.notify();
        }
    }
```

Measure. Commit: `⚡️ zb: Look the runtime up once per block_on waker, not per wake`.

- [ ] **Step 5: Verify**

Per-commit clippy/check and runtime tests for each of the four; report the four `spawn/`
medians (before, after 2, after 3, after 4).

---

### Task 2: A `block_on` takes the seat from the helper

**Files:**
- Modify: `zbus/src/runtime/builtin/driver.rs` (`Seat`, `Driving::take`, `helper`,
  `Driving::drop`, `hand_over`, `ensure_helper`, module doc)
- Modify: `zbus/src/runtime/builtin/mod.rs` (module doc paragraph on the helper)
- Test: `zbus/src/runtime/builtin/tests.rs` (three new tests; existing ones adjusted where
  "helper running" now includes "helper parked")

**Interfaces:**
- Consumes: Task 1's `Signal` (unchanged here).
- Produces: `Seat { holder, waiting, helper: Helper }` where
  `enum Helper { Down, Driving, Parked(Thread) }` replaces `helper: bool`;
  `Seat::helper_running()` is `!matches!(self.helper, Helper::Down)`.

**Protocol** (the design; the code follows it exactly):

1. A `block_on` whose `Driving::take` finds the helper in the seat records itself in `waiting`
   (as today) and calls `inner.reactor.notify()`, so the helper's wait ends.
2. The helper, after each batch and before each wait, asks `Driving::yield_if_wanted`: under
   the seat lock, if `waiting` is non-empty, it frees the seat (`Seat::free`, which unparks the
   waiters), marks itself `Helper::Parked(thread::current())`, and returns `true`; the helper
   then `thread::park()`s and, when unparked, tries `take` again. `yield_if_wanted` runs before
   `leave_if_idle`? No: `leave_if_idle` first (an idle runtime needs no helper at all), then
   `yield_if_wanted`, then the wait.
3. `Driving::take` for the helper no longer clears the flag on failure: a helper that finds the
   seat taken marks itself `Parked` under the lock and returns `None`; `helper()` parks and
   retries. (The ABA guard `left` stays.)
4. `Seat::free` unparks a `Parked` helper as well as the waiters, every time. So: a `block_on`
   leaving with work behind unparks the helper, which takes the seat back; a `block_on` leaving
   with nothing behind also unparks it, and the helper then takes the seat, finds nothing
   (`leave_if_idle`) and exits with `Helper::Down`.
5. `hand_over` and `ensure_helper` start a thread only when `helper == Helper::Down`.
6. `leave_if_idle` sets `Helper::Down` when the leaver is the helper (as today for the flag).

The pattern this serves: a program calling `zbus::block_on` once per operation with a live
connection pays, per call, one notify, one park/unpark of the helper and two seat changes, and
in exchange runs the operation on its own thread with no contention — the profile's 68 µs
against 24 µs.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
#[timeout(15000)]
fn a_block_on_takes_the_seat_from_the_helper() {
    let runtime = runtime();
    // Spawned from outside `block_on`, so a helper starts and holds the seat.
    let _held = runtime.spawn("a task that never finishes", pending::<()>());
    assert!(within_a_second(|| runtime.helper_running()));
    let task = runtime.spawn("a task that names its thread", async { thread::current().id() });

    let ran_on = drive(&runtime, task).unwrap();

    assert_eq!(ran_on, thread::current().id());
}

#[test]
#[timeout(15000)]
fn the_helper_takes_the_seat_back_when_block_on_leaves() {
    let runtime = runtime();
    let _held = runtime.spawn("a task that never finishes", pending::<()>());
    assert!(within_a_second(|| runtime.helper_running()));
    drive(&runtime, async {});

    // Awaited from outside `block_on`: only the helper can run it.
    let task = runtime.spawn("a task that names its thread", async {
        thread::current().name().map(str::to_owned)
    });
    let name = block_on(task).unwrap();

    assert_eq!(name.as_deref(), Some("zbus runtime"));
}

#[test]
#[timeout(15000)]
fn a_parked_helper_leaves_once_nothing_is_left() {
    let runtime = runtime();
    let held = runtime.spawn("a task that never finishes", pending::<()>());
    assert!(within_a_second(|| runtime.helper_running()));
    // Takes the seat from the helper, which parks.
    drive(&runtime, async {});
    assert!(runtime.helper_running());

    drop(held);

    assert!(helper_gone(&runtime));
}
```

(`block_on` in the tests module is `futures_lite::future::block_on`, the foreign one; `drive`
is the runtime's.) Run: `cargo test -p zbus --features p2p --lib runtime::builtin` — the first
fails (`ran_on` is the helper's thread), the third may hang without the change; that is the
failure.

- [ ] **Step 2: Implement the protocol**

`Seat`:

```rust
    /// The helper thread: down, driving, or parked while a `block_on` has the seat.
    helper: Helper,
```

```rust
/// Where the helper thread is.
enum Helper {
    /// No helper thread.
    Down,
    /// In the seat, or on its way to take it.
    Driving,
    /// Up, with a `block_on` in the seat; unparked when the seat is freed.
    Parked(Thread),
}
```

`Seat::free`:

```rust
    fn free(&mut self) {
        self.holder = Holder::Nobody;
        DRIVER_REACTOR.with(|reactor| reactor.set(None));
        for (_, thread) in self.waiting.drain() {
            thread.unpark();
        }
        // A parked helper takes the seat back where work is left, and leaves where none is;
        // either way it has to look.
        if let Helper::Parked(thread) = &self.helper {
            thread.unpark();
        }
    }
```

`Driving::take`, the taken branch:

```rust
        if seat.holder != Holder::Nobody {
            match who {
                Holder::Helper => seat.helper = Helper::Parked(thread::current()),
                _ => {
                    let thread = thread::current();
                    seat.waiting.insert(thread.id(), thread);
                    // The helper looks for waiters between a batch and a wait, so its wait
                    // is ended here; a `block_on` in the seat frees it when its future is
                    // done and needs no telling.
                    if seat.holder == Holder::Helper {
                        drop(seat);
                        inner.reactor.notify();
                    }
                }
            }

            return None;
        }
        seat.holder = who;
        if who == Holder::Helper {
            seat.helper = Helper::Driving;
        }
```

(`notify` from a thread not in the seat writes the wake byte; the reactor's `wake_pending`
dedups it.) `Driving::yield_if_wanted`:

```rust
    /// Frees the seat for a waiting `block_on` and parks the helper; what the helper asks
    /// before each wait. `true` where it did, and the caller parks.
    fn yield_if_wanted(&self) -> bool {
        let mut seat = lock(&self.inner.seat);
        if seat.waiting.is_empty() {
            return false;
        }
        seat.helper = Helper::Parked(thread::current());
        seat.free();
        self.left.set(true);

        true
    }
```

`helper`:

```rust
fn helper(inner: Arc<Inner>) {
    let mut failed_waits = 0u32;
    loop {
        let Some(driving) = Driving::take(inner.clone(), Holder::Helper) else {
            // A `block_on` has the seat; it frees it when its future is done, and the parked
            // helper is unparked then.
            thread::park();
            continue;
        };
        loop {
            driving.run_batch();
            if driving.leave_if_idle() {
                return;
            }
            if driving.yield_if_wanted() {
                thread::park();
                break;
            }
            driving.wait(false, &mut failed_waits);
        }
    }
}
```

`leave_if_idle` and `Driving::drop`: where they set `seat.helper = false` for the helper, set
`Helper::Down`; where `hand_over`/`ensure_helper` test `!seat.helper`, test
`matches!(seat.helper, Helper::Down)`; `Seat::helper_running` is `!matches!(self.helper,
Helper::Down)`. `Driving::drop` on the helper thread while unwinding (a panic in a waker)
must set `Helper::Down` as today, so the next spawn starts a fresh helper. A `Driving` whose
`left` is set (after `yield_if_wanted`) does nothing in `drop`, as after `leave_if_idle`.

Module docs: `driver.rs`'s header and `mod.rs`'s helper paragraph gain one sentence each: a
`block_on` that arrives while the helper is in the seat is given it, the helper parking until
the `block_on` leaves, so a program calling `block_on` once per operation runs each on its own
thread.

- [ ] **Step 3: Run the tests, then the whole set**

The three new tests pass; every existing runtime test passes (adjust any that asserted the
helper had *gone* right after a `block_on` took the seat — with the protocol it is parked, and
gone only once nothing is left; say which in the report). Release loop ten times. Session-bus
suite. Windows check.

- [ ] **Step 4: Measure**

`spawn/` (both ids), `method-call/roundtrip`, `block_on/roundtrip-inside`,
`signal/emit-receive`, `connection/graceful-shutdown`, three runs each, medians, before (the
Task 1 tip) and after. Expected: `spawn/100-tasks` near `spawn/100-tasks-inside`;
`method-call/roundtrip` (one call per `block_on`, connection alive) now pays two seat changes
and a notify per call, and should land near `block_on/roundtrip-inside` (17 µs) rather than its
23 µs; nothing else moves. A result outside that shape is reported as is.

- [ ] **Step 5: Commit**

`⚡️ zb: Give a block_on the seat while the helper thread waits` with a body saying why (a
thread inside `block_on` runs the runtime with no contention; a helper holding the seat against
it costs a burst of 100 spawns three times the cycles) and the two measured numbers.

---

### Task 3: Numbers into the PR

- [ ] **Step 1:** Update the PR body's benchmark table (the session scratchpad's latest
  `pr-body-*.md` is the current text; `gh pr view 1975 --json body -q .body` is the source of
  truth): rows for `spawn/100-tasks`, `spawn/100-tasks-inside`, `method-call/roundtrip` and any
  other id that moved, and the "How to read it" paragraph's sentence on spawn/shutdown replaced
  by the new story. Apply with `gh pr edit 1975 --body-file`.
- [ ] **Step 2:** `/usr/bin/git push --force-with-lease zeenix builtin-runtime-plan:builtin-runtime`
  and `gh pr checks 1975 --watch`.

## Self-review

- Coverage: PROFILE.md items 1 (Task 1 Step 2), 3 (Step 3), 8 (Step 4), 6 (Task 2); the bench
  that shows the seat path (Task 1 Step 1). Items 2 (`&'static str` names: a public trait
  signature), 4 (single allocation per task: a rewrite of the cell), 5 (skipping the zero-
  timeout wait: needs a generation counter and fairness proof), 7 (lock-free queue: atomics),
  9, 10 (micro) are out of scope.
- Names: `Helper::{Down, Driving, Parked}`, `Seat::helper_running`, `Driving::yield_if_wanted`,
  `Driving::take`'s notify, `Seat::free`'s unpark — consistent between the protocol and the
  code.
- Placeholders: none; the one open choice (criterion group naming) has both outcomes given.
