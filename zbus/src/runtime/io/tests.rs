//! Tests for the socket every connection's I/O goes through.
//!
//! Each one runs under every runtime this build can make, since the wrapper's whole purpose is
//! to behave the same on all of them.

use std::{
    future::Future,
    io::{Read, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    sync::{Arc, Mutex},
};

use ntest::timeout;

use super::{RegisteredIo, UnixOps};
use crate::{
    connection::socket::{ReadHalf, WriteHalf},
    runtime::{IoSource, Runtime, test_runtime::TestRuntime},
};

#[test]
#[timeout(15000)]
fn a_partial_write_is_reported_as_success() {
    under_every_runtime(|runtime| async move {
        let (left, right) = UnixStream::pair().unwrap();
        // A send buffer far smaller than the write below is what makes the kernel take only part
        // of it.
        rustix::net::sockopt::set_socket_send_buffer_size(&left, 1024).unwrap();
        let mut socket = half(&runtime, left, UnixOps);

        let written = socket.sendmsg(&[b'z'; 64 * 1024], &[]).await.unwrap();

        assert!(written > 0, "the write reported no progress at all");
        assert!(written < 64 * 1024, "the whole write fitted after all");
        drop(right);
    });
}

#[test]
#[timeout(15000)]
fn file_descriptors_cross_a_unix_socket() {
    under_every_runtime(|runtime| async move {
        let (left, right) = UnixStream::pair().unwrap();
        let mut sender = half(&runtime, left, UnixOps);
        let mut receiver = half(&runtime, right, UnixOps);

        let (read_end, mut write_end) = std::io::pipe().unwrap();
        write_end.write_all(b"through the pipe").unwrap();
        drop(write_end);
        let read_end = OwnedFd::from(read_end);

        let written = sender
            .sendmsg(b"!", &[std::os::fd::AsFd::as_fd(&read_end)])
            .await
            .unwrap();
        assert_eq!(written, 1);

        let mut byte = [0; 1];
        let (read, mut fds) = receiver.recvmsg(&mut byte).await.unwrap();
        assert_eq!(read, 1);
        assert_eq!(&byte, b"!");
        assert_eq!(fds.len(), 1, "the descriptor did not come with the byte");

        let mut through = String::new();
        std::fs::File::from(fds.remove(0))
            .read_to_string(&mut through)
            .unwrap();
        assert_eq!(through, "through the pipe");
    });
}

#[test]
#[timeout(15000)]
fn readiness_written_before_registration_is_seen() {
    under_every_runtime(|runtime| async move {
        let (left, mut right) = UnixStream::pair().unwrap();
        // The bytes are there before the runtime is ever told about the socket, so a
        // registration has to find them whether it tries the read first or waits for a readiness
        // that has already happened.
        right.write_all(b"early").unwrap();
        let mut socket = half(&runtime, left, UnixOps);

        let mut buffer = [0; 5];
        let (read, _fds) = socket.recvmsg(&mut buffer).await.unwrap();

        assert_eq!(&buffer[..read], b"early");
    });
}

#[test]
#[timeout(15000)]
fn a_registration_is_dropped_before_its_source() {
    let (left, peer) = UnixStream::pair().unwrap();
    peer.set_nonblocking(true).unwrap();
    let runtime = Watching {
        inner: TestRuntime::new(),
        peer: Arc::new(peer),
        log: Arc::default(),
    };
    let log = runtime.log.clone();
    let socket = half(&Runtime::from_external(runtime), left, UnixOps);

    drop(socket);

    assert_eq!(
        log.lock().unwrap().as_slice(),
        ["the source is still open"],
        "the source was closed before the registration let go of it",
    );
}

/// `stream`, registered on `runtime` as one half of a socket with `ops`.
fn half<S, O>(runtime: &Runtime, stream: S, ops: O) -> Arc<RegisteredIo<O>>
where
    S: Into<OwnedFd>,
    O: super::SocketOps,
{
    Arc::new(super::registered(runtime, stream, ops).unwrap())
}

/// Runs `body` once under every runtime this build can make.
///
/// Everything but Tokio is driven by `futures_lite`; a Tokio connection is polled inside the
/// runtime it belongs to, which is where a Tokio user would poll it.
fn under_every_runtime<Body, Fut>(body: Body)
where
    Body: Fn(Runtime) -> Fut,
    Fut: Future<Output = ()>,
{
    #[cfg(feature = "async-io")]
    futures_lite::future::block_on(body(Runtime::AsyncIo(crate::runtime::AsyncIo::new())));

    #[cfg(feature = "tokio")]
    {
        let tokio = tokio::runtime::Runtime::new().unwrap();
        tokio.block_on(async {
            let runtime = crate::runtime::Tokio::current().expect("a Tokio runtime is current");

            body(Runtime::Tokio(runtime)).await
        });
    }

    futures_lite::future::block_on(body(Runtime::from_external(TestRuntime::new())));
}

/// A runtime whose registrations keep no source of their own.
///
/// That is what lets a test see when the source a [`RegisteredIo`] holds is released: the peer of
/// the socket pair only reads end-of-file once the last owner of the descriptor has let go.
#[derive(Clone, Debug)]
struct Watching {
    inner: TestRuntime,
    peer: Arc<UnixStream>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl crate::runtime::traits::Runtime for Watching {
    type RegisteredIoSource = WatchingRegistration;
    type Sleep = <TestRuntime as crate::runtime::traits::Runtime>::Sleep;
    type Task<T>
        = <TestRuntime as crate::runtime::traits::Runtime>::Task<T>
    where
        T: Send + 'static;

    fn register_io_source(&self, source: IoSource) -> std::io::Result<WatchingRegistration> {
        drop(source);

        Ok(WatchingRegistration {
            peer: self.peer.clone(),
            log: self.log.clone(),
        })
    }

    fn sleep(&self, duration: std::time::Duration) -> Self::Sleep {
        crate::runtime::traits::Runtime::sleep(&self.inner, duration)
    }

    fn spawn<T>(
        &self,
        name: &str,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Self::Task<T>
    where
        T: Send + 'static,
    {
        crate::runtime::traits::Runtime::spawn(&self.inner, name, future)
    }
}

/// A registration that records, as it goes, whether the source it was given is still open.
#[derive(Debug)]
struct WatchingRegistration {
    peer: Arc<UnixStream>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl crate::runtime::traits::PollIo for WatchingRegistration {
    fn poll_io<T>(
        &self,
        _cx: &mut std::task::Context<'_>,
        _interest: crate::runtime::Interest,
        mut operation: impl FnMut() -> std::io::Result<T>,
    ) -> std::task::Poll<std::io::Result<T>> {
        std::task::Poll::Ready(operation())
    }
}

impl Drop for WatchingRegistration {
    fn drop(&mut self) {
        // A closed source makes the peer of the socket pair read end-of-file; while it is open
        // the non-blocking read finds nothing to return instead.
        let closed = matches!((&*self.peer).read(&mut [0; 1]), Ok(0));

        self.log.lock().unwrap().push(if closed {
            "the source is closed"
        } else {
            "the source is still open"
        });
    }
}

/// A peer's supplementary groups are looked up through the runtime, and only where zbus makes
/// that lookup.
///
/// The peer of a socket pair is this process itself, so what the lookup finds is of no interest
/// here; what is, is whether the runtime was handed any blocking work to find it.
#[cfg(unix)]
#[test]
#[timeout(15000)]
fn a_group_lookup_reaches_the_runtime_only_where_there_is_one() {
    let counting = TestRuntime::new();
    let (left, _peer) = UnixStream::pair().unwrap();
    let mut socket = half(&Runtime::from_external(counting.clone()), left, UnixOps);

    futures_lite::future::block_on(ReadHalf::peer_credentials(&mut socket)).unwrap();

    // zbus looks supplementary groups up on Linux and Android only, and one call is what that
    // takes.
    let expected = usize::from(cfg!(any(target_os = "android", target_os = "linux")));
    assert_eq!(counting.blocking_calls(), expected);
}
