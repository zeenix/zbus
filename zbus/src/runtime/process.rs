//! The helper process behind a transport.
//!
//! `unixexec:` runs a program and speaks D-Bus over its standard input and output, while `ibus:`
//! and, on macOS, `launchd:` run one to ask it where the bus is. Either way the program is
//! started with [`std::process::Command`], its pipes are watched by the connection's runtime the
//! way its sockets are, and waiting for it to exit is blocking work that runtime is handed, unless
//! it has exited already.
//!
//! That wait starts when the connection lets go of the pipe it reads the program's output from,
//! which is the end of the transport whichever way round it came about: the program has closed
//! its output and exited, or the connection has stopped reading. A program that has exited by
//! then, as one that closed its output usually has, is collected on the spot, with no blocking
//! work at all. For one still running, the blocking work lasts until the program is gone, which
//! for a `unixexec:` program is once its input ends: when the last clone of the connection lets
//! go of the other pipe.

#[cfg(feature = "unixexec")]
use std::os::fd::BorrowedFd;
#[cfg(any(feature = "ibus", target_os = "macos"))]
use std::process::ExitStatus;
use std::{
    io,
    process::Stdio,
    sync::{Arc, Mutex, PoisonError},
};

#[cfg(feature = "unixexec")]
use super::io::RegisteredIo;
use super::{
    Runtime,
    io::{PipeOps, registered},
};
use crate::connection::socket::ReadHalf;
#[cfg(feature = "unixexec")]
use crate::{
    conn::AuthMechanism,
    connection::socket::{BoxedSplit, RecvmsgResult, Split, WriteHalf},
    fdo::ConnectionCredentials,
};

/// Runs `command` and hands back its standard input and output, watched by `runtime`.
///
/// The child keeps the caller's standard error. Whatever the child is handed to also takes on
/// waiting for it: the reaper that does that is in place before anything below can fail, so a
/// child this call gives up on is still waited for and leaves no zombie behind.
#[cfg(feature = "unixexec")]
pub(crate) fn spawn(runtime: &Runtime, mut command: std::process::Command) -> io::Result<Child> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    // Both pipes come off the child before the reaper takes it over.
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let reaper = Arc::new(Reaper::new(child, runtime));

    let stdin = registered(runtime, taken(stdin, "standard input")?, PipeOps).map(Arc::new)?;
    let stdout = registered(runtime, taken(stdout, "standard output")?, PipeOps).map(Arc::new)?;

    Ok(Child {
        stdin,
        stdout,
        reaper,
    })
}

/// Runs `command` to completion on `runtime` and hands back what it printed.
///
/// A program asked where a bus is has nothing to read and nothing to say beyond that address, so
/// its standard output is the only pipe here. Its standard input is the null device — the end of
/// its input is there from the start, and there is no pipe of its own to register — and so is its
/// standard error, whose contents nobody asking for an address has ever been shown.
///
/// A program that ends with a failing status is an error naming that status, so the bytes a
/// caller gets back are always those of a program that succeeded.
#[cfg(any(feature = "ibus", target_os = "macos"))]
pub(crate) async fn stdout(
    runtime: &Runtime,
    mut command: std::process::Command,
) -> io::Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    // The pipe comes off the child before the reaper takes it over, so a registration that fails
    // still leaves the child with something to wait for it.
    let pipe = child.stdout.take();
    let reaper = Reaper::new(child, runtime);
    let mut pipe = registered(runtime, taken(pipe, "standard output")?, PipeOps).map(Arc::new)?;

    let mut collected = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let (read, _) = ReadHalf::recvmsg(&mut pipe, &mut buffer).await?;
        if read == 0 {
            break;
        }
        collected.extend_from_slice(&buffer[..read]);
    }
    // Nothing more can come out of the pipe, so the program has exited or is about to. The
    // reaper is this call's alone, so the wait for it is one to await rather than to leave
    // behind.
    drop(pipe);
    let status = reaper.wait().await?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "the helper process ended with {status}"
        )));
    }

    Ok(collected)
}

/// A running helper process, with both of its pipes registered on a runtime.
#[cfg(feature = "unixexec")]
#[derive(Debug)]
pub(crate) struct Child {
    stdin: Arc<RegisteredIo<PipeOps>>,
    stdout: Arc<RegisteredIo<PipeOps>>,
    reaper: Arc<Reaper>,
}

#[cfg(feature = "unixexec")]
impl Child {
    /// The child's standard output and input, as the halves a connection reads and writes.
    ///
    /// [`Connection::close`] closes the helper's standard input, which is what tells the program
    /// to exit; a pipe has no shutdown of its own, so closing here is letting go of the
    /// descriptor. The wait for the program starts once the read half is gone, so a connection
    /// still reading what the program has left to say is never waited on behind its back.
    ///
    /// [`Connection::close`]: crate::Connection::close
    pub(crate) fn into_split(self) -> BoxedSplit {
        let Self {
            stdin,
            stdout,
            reaper,
        } = self;

        Split::new(
            Box::new(Stdout {
                pipe: stdout,
                reaper: reaper.clone(),
            }) as Box<dyn ReadHalf>,
            Box::new(Stdin {
                pipe: Some(stdin),
                reaper: Some(reaper),
            }) as Box<dyn WriteHalf>,
        )
    }
}

/// The standard output of a helper process, as the half a connection reads from.
#[cfg(feature = "unixexec")]
#[derive(Debug)]
struct Stdout {
    pipe: Arc<RegisteredIo<PipeOps>>,
    // Asked to start the wait as this half goes, so that a program that ended on its own is
    // waited for even while something else still holds the half it was written to.
    reaper: Arc<Reaper>,
}

#[cfg(feature = "unixexec")]
#[async_trait::async_trait]
impl ReadHalf for Stdout {
    async fn recvmsg(&mut self, buffer: &mut [u8]) -> RecvmsgResult {
        ReadHalf::recvmsg(&mut self.pipe, buffer).await
    }

    fn can_pass_unix_fd(&self) -> bool {
        ReadHalf::can_pass_unix_fd(&self.pipe)
    }

    async fn peer_credentials(&mut self) -> io::Result<ConnectionCredentials> {
        ReadHalf::peer_credentials(&mut self.pipe).await
    }

    fn auth_mechanism(&self) -> AuthMechanism {
        ReadHalf::auth_mechanism(&self.pipe)
    }
}

#[cfg(feature = "unixexec")]
impl Drop for Stdout {
    /// Starts the wait for the program, because the transport is over once its output is let go
    /// of.
    ///
    /// That is the end of it whichever way round it came about: the program closed its output
    /// and exited, or the connection stopped reading. The first is the case the write half
    /// cannot cover on its own — a program that exits of its own accord while a `Connection` or
    /// a `Proxy` is kept around idle would sit as a zombie for as long as that owner lives.
    ///
    /// In the second case the program is still running, and waiting for it parks a blocking
    /// worker until it notices that its input has ended, which only happens once every clone of
    /// the connection is gone and the other half with them.
    fn drop(&mut self) {
        self.reaper.reap();
    }
}

/// The standard input of a helper process, as the half a connection writes to.
///
/// A pipe has no shutdown of its own, so closing this half is letting go of the descriptor: that
/// is what the program at the other end sees as the end of its input, and what a `unixexec:`
/// program takes as the word to exit.
#[cfg(feature = "unixexec")]
#[derive(Debug)]
struct Stdin {
    pipe: Option<Arc<RegisteredIo<PipeOps>>>,
    // The fallback for a program whose read half never started the wait. Let go of along with
    // the pipe: a half that has been closed neither keeps the program's input open nor holds
    // that wait back.
    reaper: Option<Arc<Reaper>>,
}

#[cfg(feature = "unixexec")]
#[async_trait::async_trait]
impl WriteHalf for Stdin {
    async fn sendmsg(&mut self, buffer: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
        WriteHalf::sendmsg(self.pipe()?, buffer, fds).await
    }

    #[cfg(any(target_os = "freebsd", target_os = "dragonfly"))]
    async fn send_zero_byte(&mut self) -> io::Result<Option<usize>> {
        WriteHalf::send_zero_byte(self.pipe()?).await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.pipe.take();
        self.reaper.take();

        Ok(())
    }

    fn can_pass_unix_fd(&self) -> bool {
        self.pipe.as_ref().is_some_and(WriteHalf::can_pass_unix_fd)
    }

    async fn peer_credentials(&mut self) -> io::Result<ConnectionCredentials> {
        WriteHalf::peer_credentials(self.pipe()?).await
    }
}

#[cfg(feature = "unixexec")]
impl Stdin {
    /// The pipe, or the error for a half whose descriptor has been let go of.
    fn pipe(&mut self) -> io::Result<&mut Arc<RegisteredIo<PipeOps>>> {
        self.pipe.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "the standard input of the helper process is closed",
            )
        })
    }
}

/// The wait for a helper process, shared by everything that still has a pipe to it.
///
/// The half a connection reads from is what starts that wait, and the last holder to let go is
/// the fallback for a child that never reached one.
#[derive(Debug)]
struct Reaper {
    child: Mutex<Option<std::process::Child>>,
    runtime: Runtime,
}

impl Reaper {
    /// A reaper for `child`, which waits for it on `runtime` when the time comes.
    fn new(child: std::process::Child, runtime: &Runtime) -> Self {
        Self {
            child: Mutex::new(Some(child)),
            runtime: runtime.clone(),
        }
    }

    /// How the child ended.
    ///
    /// A child that has exited already is collected without blocking work. One that is still
    /// running is waited for through the runtime's blocking hook.
    ///
    /// The outcome only reaches a caller that holds the reaper on its own; anywhere else the
    /// wait is the one [`Drop`] starts, with nobody left to hand an outcome to.
    #[cfg(any(feature = "ibus", target_os = "macos"))]
    async fn wait(&self) -> io::Result<ExitStatus> {
        let Some(mut child) = self.take() else {
            return Err(io::Error::other(
                "the helper process is already being waited for",
            ));
        };

        // The output has ended by now, and the program has often exited with it, in which case a
        // look at it is all the wait takes. An error from the look is the one a wait would give.
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }

        self.runtime.spawn_blocking(move || child.wait()).await
    }

    /// Starts the wait for the child, unless something has taken it on already.
    ///
    /// A child that has exited already is collected right here, with no blocking work. One that
    /// is still running is handed to the blocking hook, and nobody is left to hear how it ended,
    /// so the future the hook hands back is dropped on the spot: the hook's contract is that the
    /// work runs to completion regardless, which is what leaves the wait with the runtime's pool
    /// and nothing else, in particular no task holding a runtime that a helper outliving its
    /// connection could keep alive.
    ///
    /// For a child that is still running, a runtime that is already shutting down is the one case
    /// this cannot cover. Tokio's blocking pool takes work after its runtime has gone and never
    /// runs it, so a helper let go of by then is left for this process's own exit to hand over to
    /// the init process. And a hook that cannot start a thread for the wait panics here, as it
    /// would wherever else it was asked from.
    fn reap(&self) {
        let Some(mut child) = self.take() else {
            return;
        };

        // A look at a child that has exited already collects it. An error from the look means
        // there is nothing for a wait to collect either, so only a child still running is left to
        // the blocking hook.
        if matches!(child.try_wait(), Ok(None)) {
            drop(self.runtime.spawn_blocking(move || child.wait()));
        }
    }

    /// The child, for whichever of the wait, the reap and the drop gets there first.
    fn take(&self) -> Option<std::process::Child> {
        self.child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl Drop for Reaper {
    /// Waits for a child nobody asked about, so that it leaves no zombie behind.
    ///
    /// A transport starts the wait as its read half goes, so this is the fallback for a child
    /// that never reached one: a connection attempt that gave up between spawning the program
    /// and registering its pipes leaves one of those.
    fn drop(&mut self) {
        self.reap();
    }
}

/// `pipe`, or the error for a child process that turned out to have no such pipe.
fn taken<P>(pipe: Option<P>, which: &str) -> io::Result<P> {
    pipe.ok_or_else(|| io::Error::other(format!("the child process has no {which}")))
}

// Every test here runs a program and reads what it printed, which is what `stdout` is.
#[cfg(all(test, any(feature = "ibus", target_os = "macos")))]
mod tests {
    use std::process::Command;

    use ntest::timeout;

    use super::*;
    use crate::runtime::test_runtime::under_every_runtime;

    /// A helper process is waited for, so it leaves no zombie behind.
    #[test]
    #[timeout(15000)]
    fn a_helper_process_is_reaped() {
        under_every_runtime(|runtime| async move {
            let printed = stdout(&runtime, Command::new("true")).await.unwrap();

            assert!(printed.is_empty(), "got {printed:?}");
        });
    }

    /// Waiting for a helper process that is still running on a runtime that keeps no threads for
    /// blocking work leaves none behind either.
    ///
    /// A program that had exited already is collected on the spot, with no thread started at all,
    /// so this one closes its output at once and then runs on for a moment. The read ends with the
    /// program still there to be waited for, and the wait runs on a thread of the default hook's
    /// own. Should this thread be held up for longer than the program runs, it finds the program
    /// gone and has no thread to check, which only makes the test pass.
    #[cfg(target_os = "linux")]
    #[test]
    #[timeout(15000)]
    fn reaping_a_helper_process_leaves_no_thread_behind() {
        use crate::runtime::{test_runtime::DefaultBlocking, tests::blocking_threads};

        let runtime = Runtime::new(DefaultBlocking::new());
        // Other tests are free to run blocking work of their own alongside this one, so the
        // count this one has to come back to is the one it started from.
        let before = blocking_threads();

        let mut command = Command::new("sh");
        command.args(["-c", "exec >&-; sleep 0.2"]);
        let printed = futures_lite::future::block_on(stdout(&runtime, command)).unwrap();

        assert!(printed.is_empty(), "got {printed:?}");
        while blocking_threads() > before {
            std::thread::yield_now();
        }
    }

    /// What a helper process writes to its standard output is read to the end of it.
    #[test]
    #[timeout(15000)]
    fn stdout_is_collected() {
        under_every_runtime(|runtime| async move {
            let mut command = Command::new("echo");
            command.arg("hello");

            let printed = stdout(&runtime, command).await.unwrap();

            assert_eq!(printed, b"hello\n");
        });
    }

    /// A helper process asked for its output gets no standard input of its own to wait for.
    ///
    /// `cat` reads its input to the end before the `echo` after it runs, so the program only
    /// finishes at all if its input has an end: the null device has one from the start, an open
    /// pipe has none.
    #[test]
    #[timeout(15000)]
    fn a_helper_process_asked_for_its_output_reads_no_input() {
        under_every_runtime(|runtime| async move {
            let mut command = Command::new("sh");
            command.args(["-c", "cat; echo done"]);

            let printed = stdout(&runtime, command).await.unwrap();

            assert_eq!(printed, b"done\n");
        });
    }

    /// A helper process that ends badly is an error, rather than empty output.
    #[test]
    #[timeout(15000)]
    fn a_failing_helper_process_is_an_error() {
        under_every_runtime(|runtime| async move {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 3"]);

            let failed = stdout(&runtime, command).await.unwrap_err();

            assert!(
                failed.to_string().contains("exit status: 3"),
                "got `{failed}`",
            );
        });
    }
}

// The halves below are what a `unixexec:` connection is handed, so they are only built, and only
// tested, with that transport.
#[cfg(all(test, feature = "unixexec"))]
mod split_tests {
    use std::process::Command;

    use ntest::timeout;

    use super::*;
    use crate::runtime::test_runtime::TestRuntime;

    /// The wait for a helper process starts when the half a connection reads is let go of, and
    /// not when the half it writes to is closed.
    ///
    /// `cat` stands in for a `unixexec:` helper: it writes back what it is given and only exits
    /// once its input ends. Its input is closed here while what it wrote back is still to be
    /// read. The program then exits and stays in the process table as a zombie, which shows that
    /// closing the write half collected nothing, and dropping the read half collects it on the
    /// spot, with no blocking work.
    #[cfg(target_os = "linux")]
    #[test]
    #[timeout(15000)]
    fn the_wait_for_a_helper_process_starts_when_its_read_half_goes() {
        // The runtime counts the blocking work it is handed, and the wait for a helper that is
        // still running is the only such work a pipe can lead to.
        let counting = TestRuntime::new();
        let runtime = Runtime::new(counting.clone());

        futures_lite::future::block_on(async {
            let child = spawn(&runtime, Command::new("cat")).unwrap();
            let pid = pid_of(&child);
            let (mut stdout, mut stdin) = child.into_split().take();
            assert_eq!(counting.blocking_calls(), 0);

            WriteHalf::sendmsg(&mut stdin, b"hello", &[]).await.unwrap();
            WriteHalf::close(&mut stdin).await.unwrap();
            // The program is on its way out, but what it wrote back is still to be read, so
            // nothing is waiting for it yet.
            assert_eq!(counting.blocking_calls(), 0);

            let mut echoed = Vec::new();
            let mut buffer = [0; 64];
            loop {
                let (read, _) = ReadHalf::recvmsg(&mut stdout, &mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                echoed.extend_from_slice(&buffer[..read]);
            }
            assert_eq!(echoed, b"hello");
            // A program that has exited and is not waited for stays in the table as a zombie, so
            // seeing it become one shows that closing the write half collected nothing.
            wait_until_zombie(pid);

            drop(stdout);
            // The drop itself collected it, without handing the runtime anything.
            assert!(!is_in_the_process_table(pid));
            assert_eq!(counting.blocking_calls(), 0);
        });
    }

    /// A helper process that ends on its own is waited for as soon as the connection has stopped
    /// reading it, rather than once whoever holds the other half has finished with it.
    ///
    /// `true` stands in for a helper that exits without being told to, and the write half stays
    /// for the rest of the test the way a `Connection` that is kept around holds on to it. The
    /// program has exited by the time the read half goes, so that is where it is collected, on the
    /// spot and with no blocking work.
    #[cfg(target_os = "linux")]
    #[test]
    #[timeout(15000)]
    fn a_helper_process_that_ended_is_waited_for_once_the_read_half_is_gone() {
        // The runtime counts the blocking work it is handed, and the wait for a helper that is
        // still running is the only such work a pipe can lead to.
        let counting = TestRuntime::new();
        let runtime = Runtime::new(counting.clone());

        futures_lite::future::block_on(async {
            let child = spawn(&runtime, Command::new("true")).unwrap();
            let pid = pid_of(&child);
            let (mut stdout, stdin) = child.into_split().take();

            let mut buffer = [0; 64];
            while ReadHalf::recvmsg(&mut stdout, &mut buffer).await.unwrap().0 != 0 {}
            // The program has exited, and nothing has collected it yet.
            wait_until_zombie(pid);
            assert_eq!(counting.blocking_calls(), 0);

            drop(stdout);
            assert!(!is_in_the_process_table(pid));
            assert_eq!(counting.blocking_calls(), 0);

            // The child has been collected, so the half that outlives that asks for no second
            // wait.
            drop(stdin);
            assert_eq!(counting.blocking_calls(), 0);
        });
    }

    /// A helper process that is still running when the connection stops reading it is waited for
    /// on blocking work, which collects it once it has exited.
    ///
    /// `cat` blocks reading its input and the write half holds that open, so the program cannot
    /// have exited when the read half goes: there is nothing to collect on the spot, and the wait
    /// is handed to the runtime. Closing the write half is what lets the program go, and the
    /// entry in the process table is gone once that wait has run.
    #[cfg(target_os = "linux")]
    #[test]
    #[timeout(15000)]
    fn a_helper_process_still_running_is_waited_for_on_blocking_work() {
        // The runtime counts the blocking work it is handed, and the wait for a helper that is
        // still running is the only such work a pipe can lead to.
        let counting = TestRuntime::new();
        let runtime = Runtime::new(counting.clone());

        futures_lite::future::block_on(async {
            let child = spawn(&runtime, Command::new("cat")).unwrap();
            let pid = pid_of(&child);
            let (stdout, mut stdin) = child.into_split().take();

            drop(stdout);
            assert_eq!(counting.blocking_calls(), 1);
            assert!(is_in_the_process_table(pid));

            WriteHalf::close(&mut stdin).await.unwrap();
            // The wait is the runtime's to run in its own time, and the entry stays until it has.
            while is_in_the_process_table(pid) {
                std::thread::yield_now();
            }
            // The reaper went with the write half, and asked for no second wait.
            assert_eq!(counting.blocking_calls(), 1);
        });
    }

    /// The helper process itself is gone once both halves are, rather than left behind.
    ///
    /// A program that announces its own process id and then becomes `cat` is one this test can
    /// watch from the outside: the entry under `/proc` outlives a child that was told to exit
    /// but never waited for, so its disappearance is the wait having run.
    #[cfg(target_os = "linux")]
    #[test]
    #[timeout(15000)]
    fn a_helper_process_leaves_the_process_table() {
        use crate::runtime::test_runtime::DefaultBlocking;

        // The default hook is what a runtime that overrides nothing waits through.
        let runtime = Runtime::new(DefaultBlocking::new());

        let pid = futures_lite::future::block_on(async {
            let mut command = Command::new("sh");
            command.args(["-c", "echo $$; exec cat"]);
            let (mut stdout, mut stdin) = spawn(&runtime, command).unwrap().into_split().take();

            let mut announced = Vec::new();
            let mut buffer = [0; 64];
            while !announced.contains(&b'\n') {
                let (read, _) = ReadHalf::recvmsg(&mut stdout, &mut buffer).await.unwrap();
                assert_ne!(read, 0, "the helper ended before it said what it was");
                announced.extend_from_slice(&buffer[..read]);
            }
            let pid: u32 = String::from_utf8_lossy(&announced).trim().parse().unwrap();

            WriteHalf::close(&mut stdin).await.unwrap();
            while ReadHalf::recvmsg(&mut stdout, &mut buffer).await.unwrap().0 != 0 {}

            pid
        });

        // The wait is the runtime's to run in its own time, and the entry stays until it has.
        while is_in_the_process_table(pid) {
            std::thread::yield_now();
        }
    }

    /// A helper process that ends on its own leaves no zombie behind either.
    ///
    /// The program announces its own process id before it exits, which is what lets this test
    /// watch it from the outside: the entry under `/proc` outlives a child that exited but was
    /// never waited for, so its disappearance is the wait having run.
    #[cfg(target_os = "linux")]
    #[test]
    #[timeout(15000)]
    fn a_helper_process_that_ended_leaves_no_zombie_behind() {
        use crate::runtime::{test_runtime::DefaultBlocking, tests::blocking_threads};

        // The default hook is what a runtime that overrides nothing waits through.
        let runtime = Runtime::new(DefaultBlocking::new());
        // Other tests are free to run blocking work of their own alongside this one, so the
        // count this one has to come back to is the one it started from.
        let before = blocking_threads();

        let (pid, stdin) = futures_lite::future::block_on(async {
            let mut command = Command::new("sh");
            command.args(["-c", "echo $$"]);
            let (mut stdout, stdin) = spawn(&runtime, command).unwrap().into_split().take();

            let mut announced = Vec::new();
            let mut buffer = [0; 64];
            loop {
                let (read, _) = ReadHalf::recvmsg(&mut stdout, &mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                announced.extend_from_slice(&buffer[..read]);
            }
            // Only the read half goes; the other one is handed back to be held for the rest of
            // the test, so nothing but this drop can have asked for the wait below.
            drop(stdout);

            let pid = String::from_utf8_lossy(&announced)
                .trim()
                .parse::<u32>()
                .unwrap();

            (pid, stdin)
        });

        // A program that has exited by now was collected by the drop, and the entry is gone. One
        // that was still on its way out is the runtime's to wait for in its own time, and the
        // entry stays until it has.
        while is_in_the_process_table(pid) {
            std::thread::yield_now();
        }
        // If the wait ran on a thread, it is the hook's own, so it is gone once the wait is done.
        while blocking_threads() > before {
            std::thread::yield_now();
        }

        drop(stdin);
    }

    /// A half that has been closed has no descriptor left to write to.
    #[test]
    #[timeout(15000)]
    fn a_write_to_a_closed_half_fails() {
        let runtime = Runtime::new(TestRuntime::new());

        futures_lite::future::block_on(async {
            let (_stdout, mut stdin) = spawn(&runtime, Command::new("cat"))
                .unwrap()
                .into_split()
                .take();

            WriteHalf::close(&mut stdin).await.unwrap();
            let refused = WriteHalf::sendmsg(&mut stdin, b"hello", &[])
                .await
                .unwrap_err();

            assert_eq!(refused.kind(), io::ErrorKind::NotConnected);
        });
    }

    /// The process id of the program `child` runs.
    ///
    /// Splitting the child into its halves uses it up, so a test asks for the id before that. The
    /// reaper has the process until something waits for it, which nothing has done by then.
    #[cfg(target_os = "linux")]
    fn pid_of(child: &Child) -> u32 {
        child
            .reaper
            .child
            .lock()
            .unwrap()
            .as_ref()
            .expect("nothing has waited for the helper yet")
            .id()
    }

    /// Waits until the process `pid` has exited and nothing has collected it.
    ///
    /// A process that has gone but is not waited for stays in the process table as a zombie, the
    /// state `Z`. Its entry vanishing before it was seen as one means that something did wait.
    #[cfg(target_os = "linux")]
    fn wait_until_zombie(pid: u32) {
        loop {
            match state_of(pid) {
                Some('Z') => return,
                Some(_) => std::thread::yield_now(),
                None => panic!("something collected the helper before its read half went"),
            }
        }
    }

    /// The state of the process `pid` in the process table, or `None` once its entry is gone.
    ///
    /// The state is the letter that follows the program's name in `/proc/<pid>/stat`. The name is
    /// in parentheses and may hold spaces and parentheses of its own, so the last closing one is
    /// where it ends.
    #[cfg(target_os = "linux")]
    fn state_of(pid: u32) -> Option<char> {
        let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
        let name_end = stat.iter().rposition(|&byte| byte == b')')?;
        let state = stat[name_end + 1..].trim_ascii_start().first()?;

        Some(char::from(*state))
    }

    /// Whether the process table has an entry for `pid`, a zombie's included.
    #[cfg(target_os = "linux")]
    fn is_in_the_process_table(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}
