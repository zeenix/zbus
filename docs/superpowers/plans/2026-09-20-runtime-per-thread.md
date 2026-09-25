# Runtime per Thread Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two threads each inside `zbus::block_on` run their connections in parallel, without
zbus starting a thread for either of them.

**Architecture:** The built-in runtime is looked up per thread instead of per process: the
`static SHARED` registry becomes a thread-local one, so the first `block_on` on a thread brings
that thread's runtime into being and the thread drives it, as now. A connection joins the
runtime whose seat the building thread is in (a task on a helper thread joins the helper's
runtime) and otherwise the building thread's own. Two consequences follow: the waker of a
`block_on`'s future can no longer look the runtime up from the waking thread, so the driving
thread tells the waker which runtime it is in; and "a thread inside `block_on` needs no helper"
holds only for that thread's own runtime, so a spawn or registration handed to another thread's
runtime from inside `block_on` starts a helper for it as any outside caller would.

**Tech Stack:** Rust, `std::thread_local!`, the existing driver/scheduler/reactor. No new
dependencies.

**Spec:** the maintainer's decision on PR #1975 (2026-09-20): "the caller launches the thread to
run each block_on and connection in separate threads and no threads are launched by block_on or
from inside it". The helper thread for work left alive after a `block_on` returns is unchanged
by this plan.

## Global Constraints

- MSRV 1.87.0; `cargo +nightly fmt --all`; `cargo clippy -p zbus --all-targets --features p2p
  -- -D warnings`; `cargo check --target x86_64-pc-windows-gnu -p zbus --all-targets
  --features p2p` must all pass.
- Doc comments and comments explain why, never what changed; no references to this plan, the PR
  or "before". 100 columns.
- Commits: gimoji prefix, package abbreviation, `Assisted-by: Claude Fable 5.1
  (claude-fable-5-1)` trailer and nothing else; commit with
  `git -c core.hooksPath=/dev/null commit --no-verify --no-gpg-sign`.
- Tests in `zbus/src/runtime/builtin/tests.rs` carry `#[timeout(15000)]` and names without a
  `test_` prefix. Run them with `cargo test -p zbus --features p2p --lib runtime::builtin`.
- No `unsafe` beyond what the driver already has.

---

### Task 1: The runtime a thread is in the seat of, and the one it owns

**Files:**
- Modify: `zbus/src/runtime/builtin/driver.rs` (thread-locals, `Driving::take`, `Seat::free`)
- Modify: `zbus/src/runtime/builtin/mod.rs` (`SHARED` → thread-local `OWN`, `Inner::current`,
  `Inner::is_own`, `Builtin::new`, `block_on`, module docs)
- Test: `zbus/src/runtime/builtin/tests.rs`

**Interfaces:**
- Produces: `driver::driven() -> Option<Arc<Inner>>` (the runtime whose seat the calling thread
  is in); `Inner::current() -> io::Result<Arc<Inner>>`; `Inner::is_own(&Arc<Inner>) -> bool`;
  the thread-local `OWN: Mutex<Weak<Inner>>` in `mod.rs`.
- Consumes: `Inner::shared_in(&Mutex<Weak<Inner>>)`, unchanged.

- [ ] **Step 1: Write the failing tests** (append to `tests.rs`)

```rust
#[test]
#[timeout(15000)]
fn each_thread_inside_block_on_has_a_runtime_of_its_own() {
    // `Builtin::new` on a thread that is in no seat goes to that thread's own runtime.
    let on_this_thread = super::block_on(async { Arc::as_ptr(Builtin::new().unwrap().inner()) });
    let on_another = thread::spawn(|| {
        super::block_on(async { Arc::as_ptr(Builtin::new().unwrap().inner()) })
    })
    .join()
    .unwrap();

    assert_ne!(on_this_thread, on_another);
    // And the same thread gets the same runtime back for as long as it is alive.
    let handle = Builtin::new().unwrap();
    assert_eq!(Arc::as_ptr(handle.inner()), on_this_thread);
}

#[test]
#[timeout(15000)]
fn a_connection_built_on_a_helper_thread_joins_the_runtime_the_helper_runs() {
    // A runtime in no registry, run by a helper because nobody is inside `block_on` on it.
    let runtime = Builtin::from_inner(Inner::new().unwrap());
    let expected = Arc::as_ptr(runtime.inner());

    let built_on = runtime.spawn("a task that builds a handle", async {
        Arc::as_ptr(Builtin::new().unwrap().inner())
    });

    assert_eq!(block_on(built_on).unwrap(), expected);
}
```

Note: the first test calls `super::block_on`, the crate's own; that function is
`#[cfg(not(feature = "tokio"))]`, so gate the test the same way:
`#[cfg(not(feature = "tokio"))]` above `#[test]`.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p zbus --features p2p --lib runtime::builtin::tests::each_thread_inside_block_on_has_a_runtime_of_its_own runtime::builtin::tests::a_connection_built_on_a_helper_thread_joins_the_runtime_the_helper_runs`
Expected: the first fails on `assert_ne!` (one runtime per process today); the second passes
today for the wrong reason (the process runtime is the one the helper runs) and must still pass
after.

- [ ] **Step 3: In `driver.rs`, record which runtime a thread is in the seat of**

Add to the `thread_local!` block:

```rust
    /// The runtime this thread is in the seat of, for whatever is built on this thread while it
    /// is: a task run by a helper thread builds its connections on the runtime the helper runs.
    static DRIVING: RefCell<Weak<Inner>> = const { RefCell::new(Weak::new()) };
```

`RefCell` and `Weak` come from `std::cell` and `std::sync`; `Weak` is imported under the
`cfg(any(test, not(feature = "tokio")))` block today and is now needed unconditionally, so move
it to the unconditional `use std::{...}`.

Add, next to `on_driver_thread`:

```rust
/// The runtime the calling thread is in the seat of, if it is in one.
pub(super) fn driven() -> Option<Arc<Inner>> {
    DRIVING.with(|driving| driving.borrow().upgrade())
}
```

In `Driving::take`, where `DRIVER_REACTOR` is set, set this too:

```rust
        DRIVER_REACTOR.with(|reactor| reactor.set(Some(NonNull::from(&*inner.reactor))));
        DRIVING.with(|driving| *driving.borrow_mut() = Arc::downgrade(&inner));
```

In `Seat::free`, where `DRIVER_REACTOR` is cleared, clear this too:

```rust
        DRIVER_REACTOR.with(|reactor| reactor.set(None));
        DRIVING.with(|driving| *driving.borrow_mut() = Weak::new());
```

- [ ] **Step 4: In `mod.rs`, make the registry the thread's**

Replace the `static SHARED` and its doc with:

```rust
thread_local! {
    /// This thread's runtime, if one is alive: the one a `block_on` on this thread drives and a
    /// connection built here, on a thread in no seat, goes on.
    ///
    /// A `Weak`, so that the runtime and the two descriptors its reactor holds go once the last
    /// handle and any thread running it are gone, and the next handle brings a fresh one. A
    /// thread that ends with connections alive leaves them to the helper thread that took them
    /// over when its last `block_on` returned.
    static OWN: Mutex<Weak<Inner>> = const { Mutex::new(Weak::new()) };
}
```

Replace `Inner::shared` with:

```rust
    /// The runtime for what the calling thread builds: the one it is in the seat of, where it
    /// is in one, and its own otherwise, made here if none is alive.
    fn current() -> io::Result<Arc<Self>> {
        if let Some(inner) = driver::driven() {
            return Ok(inner);
        }
        OWN.with(|own| Self::shared_in(own))
    }

    /// Whether `inner` is the calling thread's own runtime, the one its `block_on` drives.
    pub(super) fn is_own(inner: &Arc<Self>) -> bool {
        OWN.with(|own| std::ptr::eq(lock(own).as_ptr(), Arc::as_ptr(inner)))
    }
```

`Builtin::new` calls `Inner::current()` instead of `Inner::shared()`; its doc becomes "A handle
on the runtime for what this thread builds, brought into being here if none is alive."

`block_on` in `mod.rs` becomes (the `Resolve` type changes in Task 2; until then keep `Arc::new`):

```rust
    driver::block_on(Arc::new(|| OWN.with(|own| lock(own).upgrade())), future)
```

Rewrite the module doc's second paragraph (`There is one runtime in a process ...`) as:

```
//! A thread that runs `block_on` has a runtime of its own, shared by every connection built on
//! that thread and alive for as long as any of them or a thread running it is: one channel for
//! breaking a wait from another thread — a pipe on unix and a socket pair on Windows, two
//! descriptors either way — and no thread until there is work with nobody to run it. Two
//! threads inside `block_on` run their connections side by side; a connection used from a
//! thread other than the one that built it is driven by the latter, and each wake of the
//! former's future crosses between the two.
```

and in the third paragraph replace "The thread in the seat is the one inside" with "The thread
in a runtime's seat is the one inside".

- [ ] **Step 5: Run the two tests and the whole runtime module**

Run: `cargo test -p zbus --features p2p --lib runtime::builtin`
Expected: all pass. Note that `every_handle_in_a_registry_shares_one_runtime` and
`a_shared_runtime_goes_with_its_last_handle` keep using `shared_in` with a registry of their
own and are unchanged.

- [ ] **Step 6: Commit** (message below, verbatim; body lines at most 74 columns)

```
♻️ zb: Give each thread inside block_on a runtime of its own

One built-in runtime per process meant that two threads inside
`block_on` shared one seat: the first to take it ran both threads'
connections, and the other was polled through a cross-thread unpark
per wake of its future, plus a wake of the reactor's socket. A client
and a service in one process, each on a thread of its own, paid 70 %
over a unix socket pair for that, where async-io ran them in parallel.

The registry is now the thread's, so the first `block_on` on a thread
brings that thread's runtime into being and the thread drives it, and
two such threads run side by side with no thread of zbus's own between
them. What a thread builds goes on the runtime it is in the seat of,
so a task on a helper thread builds on the runtime the helper runs, and
on the thread's own runtime otherwise.

Assisted-by: Claude Fable 5.1 (claude-fable-5-1)
```

### Task 2: Wakes that reach the right runtime, and helpers for the runtimes of other threads

**Files:**
- Modify: `zbus/src/runtime/builtin/driver.rs` (`Resolve`, `block_on`, `Leaving`, `drive`,
  `Signal`, `ensure_helper`)
- Modify: `zbus/src/runtime/builtin/mod.rs` (`block_on`)
- Test: `zbus/src/runtime/builtin/tests.rs` (`drive`, `resolve_on_the_second_ask`, new test)

**Interfaces:**
- Produces: `driver::Resolve<'a> = &'a dyn Fn() -> Option<Arc<Inner>>`.
- Consumes: `Inner::is_own` from Task 1.

- [ ] **Step 1: Write the failing test** (append to `tests.rs`)

```rust
#[test]
#[timeout(15000)]
fn a_spawn_from_inside_another_threads_block_on_starts_a_helper() {
    // A runtime in no registry, so that it is nobody's own; the thread below is inside
    // `block_on` with nothing of its own to drive.
    let runtime = Builtin::from_inner(Inner::new().unwrap());
    let ran = Arc::new(Mutex::new(false));
    let task = {
        let ran = ran.clone();
        driver::block_on(&|| None, async {
            runtime.spawn("a task of another thread's runtime", async move {
                *lock(&ran) = true;
            })
        })
    };

    // Nobody but a helper can run it: the thread that spawned it drives no runtime of this one.
    block_on(task).unwrap();
    assert!(*lock(&ran));
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `cargo test -p zbus --features p2p --lib runtime::builtin::tests::a_spawn_from_inside_another_threads_block_on_starts_a_helper`
Expected: does not compile yet (`&|| None` is not a `Resolve`); after Step 3's type change and
before Step 5, it hangs until the timeout: `ensure_helper` sees the thread inside `block_on` and
starts no helper.

- [ ] **Step 3: The `Resolve` a `block_on` borrows**

In `driver.rs`:

```rust
/// The runtime a `block_on` is to drive, looked up afresh at each turn of its loop, because the
/// future it polls may be the very thing that brings the runtime into being. Asked on the
/// calling thread alone, so a lookup in that thread's own registry is what it is.
///
/// This and what follows it are what a `block_on` is made of, which a Tokio build has no use
/// for: there `zbus::block_on` is Tokio's own, and only a helper ever takes the seat.
#[cfg(any(test, not(feature = "tokio")))]
pub(super) type Resolve<'a> = &'a dyn Fn() -> Option<Arc<Inner>>;
```

`block_on(resolve: Resolve<'_>, future: F)`; `Leaving<'a> { resolve: Resolve<'a>, ... }`
with `resolve` stored as-is and called as `(self.resolve)()`; `drive(resolve: Resolve<'_>,
...)`. Drop the `Arc` wrapping in `Leaving { resolve: &resolve, .. }` and `drive(&resolve, ..)`
accordingly (`resolve` is already a reference).

In `mod.rs`: `driver::block_on(&|| OWN.with(|own| lock(own).upgrade()), future)`.

In `tests.rs`: `drive` becomes

```rust
fn drive<F>(runtime: &Builtin, future: F) -> F::Output
where
    F: Future,
{
    let inner = runtime.inner().clone();

    driver::block_on(&move || Some(inner.clone()), future)
}
```

and `resolve_on_the_second_ask` returns a boxed closure the caller borrows: change its
signature to `-> Box<dyn Fn() -> Option<Arc<Inner>>>`, its body's `Arc::new(move || {` to
`Box::new(move || {`, and its two call sites (lines near 600 and 622) to
`driver::block_on(&*resolve, ...)`.

- [ ] **Step 4: The waker is told its runtime rather than looking it up**

Replace `Signal`'s `resolve` field and its doc:

```rust
/// The waker of the future a `block_on` polls.
#[cfg(any(test, not(feature = "tokio")))]
struct Signal {
    thread: Thread,
    /// Whether the future has been woken since it was last polled.
    woken: Mutex<bool>,
    /// The runtime the thread is in the seat of, set by that thread as it takes the seat: a wake
    /// may come from any thread, and only the driving thread knows which runtime's wait it is in.
    runtime: Mutex<Weak<Inner>>,
}
```

and `wake_by_ref`:

```rust
    /// Marks the future woken and rouses its thread, wherever that thread is: parked, or in the
    /// seat inside the reactor's wait, which is what the notification ends. A wake from the
    /// thread itself writes no notification, because that thread looks at the flag before its
    /// next wait; a wake of a thread in no seat needs none, because that thread is parked.
    fn wake_by_ref(self: &Arc<Self>) {
        *lock(&self.woken) = true;
        self.thread.unpark();
        if let Some(inner) = lock(&self.runtime).upgrade() {
            inner.reactor.notify();
        }
    }
```

In `drive`, build the `Signal` without `resolve`, and tell it the runtime whenever the seat is
taken. Replace the two `*driving = take_seat();` lines with a call to a closure that also
records the runtime:

```rust
    let take_seat = |driving: &mut Option<Driving>| {
        *driving = resolve().and_then(|inner| Driving::take(inner, Holder::BlockOn));
        // Set before the round that follows, so that a wake arriving during its wait finds the
        // runtime whose wait to break.
        if let Some(driving) = driving {
            *lock(&signal.runtime) = Arc::downgrade(&driving.inner);
        }
    };
```

called as `take_seat(driving)` in both places (the `if driving.is_none()` guards stay).

- [ ] **Step 5: A helper for a runtime that is not the caller's own**

In `ensure_helper`, replace the first `if` with:

```rust
    // A thread inside `block_on` on its own runtime asks for no helper: it takes the seat on the
    // next turn of its loop and runs the work itself. Work handed to another thread's runtime
    // gets a helper as from any thread outside `block_on`, because this thread will never sit in
    // that seat.
    if IN_BLOCK_ON.with(Cell::get) > 0 && Inner::is_own(inner) {
        return;
    }
```

Update the doc of `Leaving::drop` ("A thread inside `block_on` asks for no helper, on the
promise ...") to read "A thread inside `block_on` asks for no helper for its own runtime, on the
promise ...".

- [ ] **Step 6: Run the runtime module's tests, fmt, clippy and the Windows check**

Run: `cargo test -p zbus --features p2p --lib runtime::builtin`
Expected: all pass, the new one included.
Run: `cargo +nightly fmt --all && cargo clippy -p zbus --all-targets --features p2p -- -D warnings && cargo check --target x86_64-pc-windows-gnu -p zbus --all-targets --features p2p`
Expected: clean.

- [ ] **Step 7: Squash into Task 1's commit**

`git commit --fixup=<Task 1 sha>` then `GIT_SEQUENCE_EDITOR=: git -c core.hooksPath=/dev/null rebase -i --autosquash <Task 1 sha>^`. One commit carries the whole change: the per-thread registry is not correct without this task.

### Task 3: Documentation

**Files:**
- Modify: `zbus/src/runtime/mod.rs:1-6`, `zbus/src/runtime/traits.rs:1-8`, `zbus/src/utils.rs`
  (the `builtin-runtime` arm of `block_on`), `book/src/connection.md` ("Built-in backends"),
  `book/src/upgrading-to-6.md` (the `builtin-runtime` paragraph), `book/src/blocking.md` (intro).

- [ ] **Step 1: Say "per thread" wherever the docs say "per process"**

`runtime/mod.rs` and `traits.rs`, same sentence in both: replace "one runtime per process, run
by the thread inside [`block_on`](crate::block_on) and by a helper thread only where no thread
is inside it" with "one runtime per thread that runs [`block_on`](crate::block_on), driven by
that thread, and by a helper thread only for work left with no thread inside `block_on`".

`utils.rs`, the `builtin-runtime` arm: replace "the calling thread also runs the tasks, sockets
and timers of every connection on zbus's built-in runtime, so such a program stays a single
thread" with "the calling thread also runs the tasks, sockets and timers of every connection
built on it, so such a program stays a single thread, and two threads that each call this run
their connections side by side".

`connection.md`, "Built-in backends": replace "one for the whole process and depending on no
runtime crate at all" with "one per thread that runs `zbus::block_on`, depending on no runtime
crate at all"; replace "that thread runs every built-in connection's tasks, sockets and timers"
with "that thread runs the tasks, sockets and timers of every built-in connection built on it";
replace "The one runtime serves every built-in connection in the process, so a task of one
connection that runs long delays the others' I/O until it yields." with "Two threads inside
`zbus::block_on` run their connections side by side. A thread's runtime serves every connection
built on that thread, so a task of one connection that runs long delays the others' I/O until it
yields, and a connection used from another thread is still driven by the thread that built it."

`upgrading-to-6.md`: replace "one runtime per process, driven by the thread inside
`zbus::block_on`" with "one runtime per thread that runs `zbus::block_on`, driven by that
thread".

`blocking.md` intro: replace "so the whole program is a single thread" with "so the whole
program is a single thread, and a program that runs `block_on` on several threads runs their
connections side by side".

- [ ] **Step 2: Build the docs and the book**

Run: `cargo doc -p zbus --no-deps && (cd book && mdbook build)`
Expected: clean. Check every changed line is at most 100 columns.

- [ ] **Step 3: Squash into the same commit** as Tasks 1 and 2, the same way.

### Task 4: Measure

Not a code task. After the commit, the two `1000-concurrent-p2p` ids are measured on the three
trees with the session script in the scratchpad (`bench-session-c.sh` pattern), and the PR body
is updated by the controller.
