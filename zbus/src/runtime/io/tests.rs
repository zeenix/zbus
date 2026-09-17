//! Tests for the socket every connection's I/O goes through.
//!
//! Anything here that needs a runtime is run under every one this build can make, since the
//! wrapper's whole purpose is to behave the same on all of them.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
    sync::{Arc, Mutex},
};

use ntest::timeout;

use socket2::{Domain, SockAddr, Socket, Type};

use super::connect::{connect, outcome};
#[cfg(unix)]
use super::{RegisteredIo, UnixOps};
#[cfg(unix)]
use crate::connection::socket::{ReadHalf, WriteHalf};
use crate::runtime::{IoSource, test_runtime::under_every_watching_runtime};
#[cfg(unix)]
use crate::runtime::{
    Runtime,
    test_runtime::{TestRuntime, under_every_runtime},
};

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

/// A connection still being made reads as a would-block.
///
/// The socket here has had no connection asked for at all, which on unix is what one the kernel
/// is still working on looks like to the predicate: no peer to name. Reporting that as a
/// would-block is what lets the wait for a connection be a readiness wait like any other. Winsock
/// is asked a question that only a connect it has taken over answers, so its side of the
/// predicate is covered by the connect tests under both polling orders instead.
#[cfg(unix)]
#[test]
fn a_socket_with_no_peer_reads_as_would_block() {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();

    let error = outcome(&IoSource::from_socket(socket)).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
}

/// A connection that has been made reads as made, on every platform.
#[test]
#[timeout(15000)]
fn a_socket_with_a_peer_reads_as_connected() {
    let listener = listening_socket();
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
    socket.connect(&listener.local_addr().unwrap()).unwrap();

    outcome(&IoSource::from_socket(socket)).unwrap();
}

/// A connection the kernel has not finished making is waited for, not reported.
///
/// Only unix can be made to hold a TCP connection this way: a listener whose backlog is full
/// leaves a connection pending there, while Winsock refuses it outright instead. On unix the
/// predicate's reading of a socket that has yet to reach a peer, which is what a connection under
/// way looks like, is also covered by [`a_socket_with_no_peer_reads_as_would_block`]; on Windows
/// the connect sweeps under both polling orders are what cover it.
#[cfg(unix)]
#[test]
#[timeout(15000)]
fn a_pending_connect_resolves_on_writable() {
    under_every_runtime(|runtime| async move {
        let listener = listening_socket();
        let address = listener.local_addr().unwrap();
        // The queue holds this one, so the kernel has nowhere to put the next connection until
        // the listener takes this one out.
        let _queued = connect(&runtime, Domain::IPV4, Type::STREAM, &address)
            .await
            .unwrap();

        let mut pending = std::pin::pin!(connect(&runtime, Domain::IPV4, Type::STREAM, &address));
        assert!(
            futures_lite::future::poll_once(pending.as_mut())
                .await
                .is_none(),
            "the connection was reported before the listener had room for it",
        );

        let _accepted = listener.accept().unwrap();
        let source = pending.await.unwrap();

        assert!(
            socket2::SockRef::from(&source).peer_addr().is_ok(),
            "the connection was reported without a peer at the other end",
        );
    });
}

#[test]
#[timeout(15000)]
fn a_pending_connect_reports_the_socket_error() {
    under_every_watching_runtime(|runtime| async move {
        let refused = RefusedPort::on(Ipv4Addr::LOCALHOST.into());

        let error = connect(
            &runtime,
            Domain::IPV4,
            Type::STREAM,
            &SockAddr::from(refused.address()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionRefused);
    });
}

#[cfg(unix)]
#[test]
#[timeout(15000)]
fn a_full_backlog_is_retried_until_accepted() {
    under_every_runtime(|runtime| async move {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("socket");
        let listener = Socket::new(Domain::UNIX, Type::STREAM, None).unwrap();
        let address = SockAddr::unix(&path).unwrap();
        listener.bind(&address).unwrap();
        listener.listen(0).unwrap();

        // The listener's one place is taken from here on, so a further connection is turned away
        // until it accepts this one.
        let _queued = connect(&runtime, Domain::UNIX, Type::STREAM, &address)
            .await
            .unwrap();

        // A thread of its own is what accepts both connections, well after the connect below has
        // had room to see the backlog full and retry at least once. The retry loop has no bound
        // of its own, so this only changes how many times it retries before succeeding, never
        // whether it does; the test's own timeout is what would catch it getting stuck.
        let accepting = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            listener.accept().unwrap();
            listener.accept().unwrap();
        });

        connect(&runtime, Domain::UNIX, Type::STREAM, &address)
            .await
            .unwrap();
        accepting.join().unwrap();
    });
}

/// A loopback TCP port that refuses every connection made to it.
///
/// Only a port nothing listens on can refuse a connection, and a test needs one that no other
/// test can be handed while it runs. A socket that is bound and never listened on is both: the
/// kernel answers a connection to it with a reset, and the port is this value's until it is
/// dropped.
pub(crate) struct RefusedPort {
    // Holding the bound sockets is what holds the port; nothing else is ever done with them.
    _bound: Vec<Socket>,
    addresses: Vec<SocketAddr>,
}

impl RefusedPort {
    /// A refused port on the loopback address of one family.
    pub(crate) fn on(loopback: IpAddr) -> Self {
        Self::reserve(&[loopback])
    }

    /// A refused port on every address `host` resolves to.
    ///
    /// A name can stand for an address of either family, and a connection to it tries every
    /// address the resolver hands back, so all of them have to refuse it.
    pub(crate) fn for_host(host: &str) -> Self {
        let resolved = (host, 0)
            .to_socket_addrs()
            .unwrap_or_else(|e| panic!("`{host}` resolves to nothing: {e}"));
        let mut addresses = Vec::new();
        for address in resolved {
            if !addresses.contains(&address.ip()) {
                addresses.push(address.ip());
            }
        }

        Self::reserve(&addresses)
    }

    /// The address a connection is refused at, in the family reserved first.
    pub(crate) fn address(&self) -> SocketAddr {
        self.addresses[0]
    }

    /// The port, which is the same number in every family it was reserved in.
    pub(crate) fn port(&self) -> u16 {
        self.address().port()
    }

    /// One port, held in the family of every address in `addresses`.
    fn reserve(addresses: &[IpAddr]) -> Self {
        for _ in 0..PORT_ATTEMPTS {
            if let Some(reserved) = Self::attempt(addresses) {
                return reserved;
            }
        }

        panic!("no ephemeral port was free in every family of {addresses:?}");
    }

    /// One try at holding the same port in the family of every address in `addresses`.
    ///
    /// The kernel picks the number for the first address and the rest ask for that same one, so a
    /// number already taken in one of their families gives up on the try rather than on the whole
    /// reservation: another number may well be free in all of them.
    fn attempt(addresses: &[IpAddr]) -> Option<Self> {
        let (first, rest) = addresses
            .split_first()
            .expect("a port is reserved on at least one address");
        let first = SocketAddr::new(*first, 0);
        let first = bound(first)
            .unwrap_or_else(|e| panic!("cannot bind an ephemeral port on {first}: {e}"));
        let port = first
            .local_addr()
            .expect("a bound socket reports the address it was given")
            .as_socket()
            .expect("a bound TCP socket has an IP address")
            .port();

        let mut sockets = vec![first];
        for address in rest {
            let address = SocketAddr::new(*address, port);
            match bound(address) {
                Ok(socket) => sockets.push(socket),
                // This family has the number taken, and another number may be free in all of
                // them; anything else is not about the number at all.
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return None,
                Err(e) => panic!("`{address}` cannot be reserved: {e}"),
            }
        }

        Some(Self {
            addresses: addresses
                .iter()
                .map(|address| SocketAddr::new(*address, port))
                .collect(),
            _bound: sockets,
        })
    }
}

/// How many ports a reservation tries before it gives up.
///
/// Every try needs one number that is free in the family of each address involved, which a port
/// taken in one of them can deny; a second number is already unlikely to be needed.
const PORT_ATTEMPTS: usize = 8;

/// A TCP socket bound to `address` and listened on by nobody.
fn bound(address: SocketAddr) -> std::io::Result<Socket> {
    let socket = Socket::new(Domain::for_address(address), Type::STREAM, None)?;

    socket.bind(&address.into()).map(|()| socket)
}

/// A socket listening on loopback with room for a single connection at a time.
fn listening_socket() -> Socket {
    let listener = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
    listener
        .bind(&SockAddr::from(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            0,
        ))))
        .unwrap();
    listener.listen(0).unwrap();

    listener
}

/// `stream`, registered on `runtime` as one half of a socket with `ops`.
#[cfg(unix)]
fn half<S, O>(runtime: &Runtime, stream: S, ops: O) -> Arc<RegisteredIo<O>>
where
    S: Into<OwnedFd>,
    O: super::SocketOps,
{
    Arc::new(super::registered(runtime, stream, ops).unwrap())
}

/// A runtime whose registrations keep no source of their own.
///
/// That is what lets a test see when the source a [`RegisteredIo`] holds is released: the peer of
/// the socket pair only reads end-of-file once the last owner of the descriptor has let go.
#[cfg(unix)]
#[derive(Clone, Debug)]
struct Watching {
    inner: TestRuntime,
    peer: Arc<UnixStream>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[cfg(unix)]
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
#[cfg(unix)]
#[derive(Debug)]
struct WatchingRegistration {
    peer: Arc<UnixStream>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

#[cfg(unix)]
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

#[cfg(unix)]
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
