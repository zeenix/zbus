//! Tests of a built-in runtime as a whole: the worker thread that starts on the first piece of
//! work handed to it, runs the scheduler and the reactor, and exits once nothing is left.

use std::{
    future::{pending, poll_fn},
    io::Write,
    mem::MaybeUninit,
    pin::pin,
    task::{Wake, Waker},
    thread,
    time::Instant,
};

use futures_lite::future::{block_on, poll_once};
use ntest::timeout;
use socket2::{SockRef, Socket};

use super::*;
use crate::runtime::{
    Interest,
    traits::{PollIo, Runtime, TaskHandle},
};

#[test]
#[timeout(15000)]
fn a_spawned_task_runs_to_completion() {
    let runtime = runtime();
    let ran = Arc::new(Mutex::new(false));
    let task = {
        let ran = ran.clone();
        runtime.spawn("a task that sets a flag", async move {
            *lock(&ran) = true;
        })
    };

    block_on(task).unwrap();

    assert!(*lock(&ran));
}

#[test]
#[timeout(15000)]
fn a_spawned_task_hands_its_output_back() {
    let runtime = runtime();
    let task = runtime.spawn("an answer", async { 42 });

    assert_eq!(block_on(task).unwrap(), 42);
}

#[test]
#[timeout(15000)]
fn the_worker_starts_on_the_first_spawn() {
    let runtime = runtime();
    let task = runtime.spawn("a task that names the thread it runs on", async {
        thread::current().name().map(str::to_owned)
    });

    let name = block_on(task).unwrap();

    assert_eq!(name.as_deref(), Some("zbus runtime"));
}

#[test]
#[timeout(15000)]
fn the_worker_exits_when_nothing_is_left() {
    let runtime = runtime();
    // A timer for the task to be, so that the deadline is armed by the worker's poll of it
    // rather than from this thread.
    let sleep = runtime.sleep(Duration::from_millis(50));
    let task = runtime.spawn("a task that sleeps", sleep);

    block_on(task).unwrap();

    assert!(worker_gone(&runtime));
}

#[test]
#[timeout(15000)]
fn cancelling_the_last_task_from_another_thread_lets_the_worker_exit() {
    let runtime = runtime();
    let task = runtime.spawn("a task that never finishes", pending::<()>());
    assert!(runtime.worker_running());

    drop(task);

    assert!(worker_gone(&runtime));
}

#[test]
#[timeout(15000)]
fn a_finished_detached_task_lets_the_worker_exit() {
    let runtime = runtime();
    // A timer to wait out first, so that the round the task ends in is one the worker reached
    // from a wait that its own timer broke: the wake channel is empty by then, and nobody is
    // told the task is over, since nobody holds a handle to it.
    let sleep = runtime.sleep(Duration::from_millis(50));

    runtime.spawn("a detached task", sleep).detach();

    // Which leaves the worker to work its own idleness out.
    assert!(worker_gone(&runtime));
}

#[test]
#[timeout(15000)]
fn a_registration_reports_readiness() {
    let runtime = runtime();
    let (source, mut peer) = pair();
    let registration = runtime.register_io_source(source.clone()).unwrap();
    // Written from another thread once the read below is waiting, so that the byte is one the
    // worker's wait reports rather than one the first attempt to read finds for itself.
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        peer.write_all(&[7]).unwrap();
    });

    let read = block_on(poll_fn(|cx| {
        let mut byte = [MaybeUninit::<u8>::uninit(); 1];

        registration.poll_io(cx, Interest::Readable, || {
            SockRef::from(&source).recv(&mut byte)
        })
    }));

    assert_eq!(read.unwrap(), 1);
    writer.join().unwrap();
}

/// A socket of one protocol family is watched beside a wake channel of another.
///
/// Winsock takes every socket of one `select` call to come from a single service provider, and
/// a built-in runtime's wake channel is a loopback TCP connection while a `unix:path=` address
/// gives a connection an `AF_UNIX` socket. A wait therefore holds sockets of both families, and
/// this is where that mixture is asked for: an `AF_UNIX` socket registered on a runtime is
/// reported readable just as a socket of the wake channel's own family is.
#[cfg(windows)]
#[test]
#[timeout(15000)]
fn an_af_unix_socket_is_watched_beside_the_tcp_wake_socket() {
    use std::{env, fs, os::windows::io::AsSocket, process, time::SystemTime};

    use uds_windows::{UnixListener, UnixStream};

    let runtime = runtime();
    // Named after this process and this moment, so that two runs of the suite cannot meet over
    // one path.
    let since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let path = env::temp_dir().join(format!(
        "zbus-builtin-runtime-{}-{}.sock",
        process::id(),
        since_epoch.as_nanos()
    ));
    let listener = UnixListener::bind(&path).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    client.set_nonblocking(true).unwrap();
    server.set_nonblocking(true).unwrap();
    // The stream keeps the handle it was made with, so what the source is given is a duplicate
    // of it.
    let source = IoSource::from(client.as_socket().try_clone_to_owned().unwrap());
    let registration = runtime.register_io_source(source.clone()).unwrap();
    // Written from another thread once the read below is waiting, so that the byte is one the
    // worker's wait reports rather than one the first attempt to read finds for itself.
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        server.write_all(&[7]).unwrap();
    });

    let read = block_on(poll_fn(|cx| {
        let mut byte = [MaybeUninit::<u8>::uninit(); 1];

        registration.poll_io(cx, Interest::Readable, || {
            SockRef::from(&source).recv(&mut byte)
        })
    }));

    assert_eq!(read.unwrap(), 1);
    writer.join().unwrap();
    drop(listener);
    fs::remove_file(&path).unwrap();
}

#[test]
#[timeout(15000)]
fn sleep_resolves_once_the_duration_has_passed() {
    let runtime = runtime();
    let started = Instant::now();

    block_on(runtime.sleep(Duration::from_millis(50)));

    assert!(started.elapsed() >= Duration::from_millis(50));
}

#[test]
#[timeout(15000)]
fn a_sleep_polled_after_the_worker_retired_is_fired() {
    let runtime = runtime();
    let sleep = runtime.sleep(Duration::from_millis(20));
    // A timer that has never been polled has no deadline with the reactor, so there is nothing
    // for a worker to do and none of them runs.
    assert!(!runtime.worker_running());
    let started = Instant::now();

    block_on(sleep);

    assert!(started.elapsed() >= Duration::from_millis(20));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
#[timeout(15000)]
fn a_sleep_armed_from_another_thread_bounds_the_wait() {
    let runtime = runtime();
    let (source, _peer) = pair();
    // A source to watch and no deadline: the worker sits in a wait that nothing but a
    // notification brings to an end.
    let _registration = runtime.register_io_source(source).unwrap();
    let started = Instant::now();

    block_on(runtime.sleep(Duration::from_millis(20)));

    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
#[timeout(15000)]
fn a_dropped_sleep_lets_the_worker_exit() {
    let runtime = runtime();
    {
        let mut sleep = pin!(runtime.sleep(Duration::from_secs(10)));
        // The first poll is what hands the deadline to the reactor.
        assert!(block_on(poll_once(sleep.as_mut())).is_none());
        assert!(runtime.worker_running());
    }

    // The deadline went with the timer it belonged to, so the wait it bounded is over too.
    assert!(worker_gone(&runtime));
}

#[test]
#[timeout(15000)]
fn a_spawn_during_the_worker_exit_is_not_stranded() {
    let runtime = runtime();

    // No pause between the rounds, so that each spawn lands wherever the round before it left
    // the worker, the exit it is about to make included.
    for _ in 0..200 {
        let started = Instant::now();
        let task = runtime.spawn("a task that finishes at once", async {});

        block_on(task).unwrap();

        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

#[test]
#[timeout(15000)]
fn a_registration_without_tasks_keeps_the_worker_alive() {
    let runtime = runtime();
    let (source, _peer) = pair();

    let _registration = runtime.register_io_source(source).unwrap();

    assert!(runtime.worker_running());
    // Long enough that a worker taking no account of the registration would have retired.
    thread::sleep(Duration::from_millis(200));
    assert!(runtime.worker_running());
}

#[test]
#[timeout(15000)]
fn a_task_that_panics_leaves_the_worker_running() {
    let runtime = runtime();
    // Keeps the worker from retiring between the tasks below, so that the task after the panic
    // is run by the very worker the panic happened on.
    let _held = runtime.spawn("a task that never finishes", pending::<()>());
    let panicking = runtime.spawn("a task that panics", async {
        panic!("the task panicked on purpose");
    });

    assert!(block_on(panicking).is_err());

    // The task that never finishes rules retirement out, so a worker running here is the one the
    // panic happened on rather than one a later spawn started.
    assert!(runtime.worker_running());
    let next = runtime.spawn("the task after it", async { 7 });
    assert_eq!(block_on(next).unwrap(), 7);
}

#[test]
#[timeout(15000)]
fn a_panic_in_a_futures_drop_leaves_the_worker_running() {
    /// A task that is finished with on its first poll, and whose future panics as the worker
    /// drops it.
    struct PanicOnDrop;

    impl Future for PanicOnDrop {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
            Poll::Ready(())
        }
    }

    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("the future's drop panicked on purpose");
        }
    }

    let runtime = runtime();
    let _held = runtime.spawn("a task that never finishes", pending::<()>());
    let going = runtime.spawn("a task that panics as it goes", PanicOnDrop);

    assert!(block_on(going).is_ok());

    // The task that never finishes rules retirement out, so a worker running here is the one
    // that dropped the future above rather than one a later spawn started.
    assert!(runtime.worker_running());
    let next = runtime.spawn("the task after it", async { 7 });
    assert_eq!(block_on(next).unwrap(), 7);
}

#[test]
#[timeout(15000)]
fn a_cancelled_futures_drop_may_spawn_on_the_runtime() {
    /// Spawns a task of its own as it goes, through the runtime it holds.
    struct SpawnOnDrop {
        runtime: Builtin,
        ran: Arc<Mutex<bool>>,
    }

    impl Drop for SpawnOnDrop {
        fn drop(&mut self) {
            let ran = self.ran.clone();
            self.runtime
                .spawn("a task spawned from a drop", async move {
                    *lock(&ran) = true;
                })
                .detach();
        }
    }

    let runtime = runtime();
    let ran = Arc::new(Mutex::new(false));
    let polled = Arc::new(Mutex::new(false));
    let task = {
        let spawner = SpawnOnDrop {
            runtime: runtime.clone(),
            ran: ran.clone(),
        };
        let polled = polled.clone();
        runtime.spawn("a task that never finishes", async move {
            let _spawner = spawner;
            *lock(&polled) = true;
            pending::<()>().await;
        })
    };

    // Cancelled once the worker has taken the task up, so that the drop of its future is this
    // thread's to make. A cancellation racing that poll would leave the drop to the worker
    // instead, and the spawn the drop makes is served the same either way.
    assert!(within_a_second(|| *lock(&polled)));
    drop(task);

    assert!(within_a_second(|| *lock(&ran)));
}

#[test]
#[timeout(15000)]
fn a_worker_that_dies_is_replaced() {
    /// Panics where it is woken, which is on the worker that fires the timer it waits for.
    struct PanickingWaker;

    impl Wake for PanickingWaker {
        fn wake(self: Arc<Self>) {
            panic!("the waker panicked on purpose");
        }
    }

    let runtime = runtime();
    // A task that never finishes, so that retiring is out of the question and an unwind is the
    // one thing that can clear the running flag.
    let _held = runtime.spawn("a task that never finishes", pending::<()>());
    let waker = Waker::from(Arc::new(PanickingWaker));
    let mut sleep = pin!(runtime.sleep(Duration::from_millis(1)));
    let polled = sleep.as_mut().poll(&mut Context::from_waker(&waker));
    assert!(polled.is_pending());

    // The worker fires the timer, the wake panics, and the thread unwinds out of its loop.
    assert!(worker_gone(&runtime));

    let task = runtime.spawn("the task after it", async { 7 });
    assert_eq!(block_on(task).unwrap(), 7);
}

/// A runtime with nothing to do and no worker of its own until it is given something.
fn runtime() -> Builtin {
    Builtin::new().unwrap()
}

/// Whether `runtime`'s worker has gone within a second.
fn worker_gone(runtime: &Builtin) -> bool {
    within_a_second(|| !runtime.worker_running())
}

/// Whether `condition` holds within a second, looked at every 10 ms.
fn within_a_second(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// A connected pair: the source to register, and the far end to drive it from.
fn pair() -> (IoSource, Socket) {
    let (near, far) = connected();
    near.set_nonblocking(true).unwrap();
    far.set_nonblocking(true).unwrap();

    (IoSource::from_socket(near), far)
}

/// Two sockets connected to one another.
#[cfg(unix)]
fn connected() -> (Socket, Socket) {
    Socket::pair(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap()
}

/// Two sockets connected to one another.
///
/// Winsock has no socket pair, so this is a loopback connection which a listener of its own
/// accepts and then has no further use for.
#[cfg(windows)]
fn connected() -> (Socket, Socket) {
    use std::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let far = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let near = loop {
        let (accepted, _) = listener.accept().unwrap();
        // A loopback listener is reachable by anything else on the machine, so a connection that
        // is not the one made just above is turned away rather than taken for it.
        if accepted.peer_addr().unwrap() == far.local_addr().unwrap() {
            break accepted;
        }
    };

    // One-byte messages travel this pair, as they do the poller's wake pair, so neither end
    // holds a send back for the peer's acknowledgement of the one before it.
    near.set_nodelay(true).unwrap();
    far.set_nodelay(true).unwrap();

    (near.into(), far.into())
}
