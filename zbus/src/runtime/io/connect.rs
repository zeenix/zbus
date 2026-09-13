//! Opening a socket without ever waiting in the kernel.

use std::io;
#[cfg(unix)]
use std::time::Duration;

use socket2::{Domain, SockAddr, SockRef, Socket, Type};

use crate::runtime::{Interest, IoSource, Runtime};

/// A new socket of `domain` and `ty`, connected to `address`.
///
/// The socket is non-blocking from the moment it exists, so a connection the kernel cannot
/// complete straight away is waited for on `runtime` instead of on the calling thread: the wait
/// is for writable readiness, which the kernel reports once it is done with the connection either
/// way, and `SO_ERROR` then says which way it went.
pub(crate) async fn connect(
    runtime: &Runtime,
    domain: Domain,
    ty: Type,
    address: &SockAddr,
) -> io::Result<IoSource> {
    #[cfg(not(unix))]
    {
        attempt(runtime, domain, ty, address).await
    }
    // A blocking `connect(2)` on a unix socket waits in the kernel for room to open up in the
    // listener's backlog; a non-blocking one is turned away at once with `EAGAIN` instead, and
    // trying again needs a socket of its own. So this loop is that same wait, done on the
    // runtime's timer rather than in the kernel: it keeps trying for as long as the backlog stays
    // full, and only whoever is awaiting this call bounds how long that is, through their own
    // timeout or by dropping the future.
    #[cfg(unix)]
    {
        loop {
            match attempt(runtime, domain, ty, address).await {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    runtime.sleep(BACKLOG_INTERVAL).await;
                }
                result => return result,
            }
        }
    }
}

/// How long a connection a full listen backlog turned away waits before it is tried again.
#[cfg(unix)]
const BACKLOG_INTERVAL: Duration = Duration::from_millis(20);

/// One try at a socket of `domain` and `ty` connected to `address`.
async fn attempt(
    runtime: &Runtime,
    domain: Domain,
    ty: Type,
    address: &SockAddr,
) -> io::Result<IoSource> {
    let socket = Socket::new(domain, ty, None)?;
    socket.set_nonblocking(true)?;

    match socket.connect(address) {
        Ok(()) => return Ok(IoSource::from_socket(socket)),
        Err(e) if is_in_progress(&e) => {}
        Err(e) => return Err(e),
    }

    let source = IoSource::from_socket(socket);
    let registration = runtime.register_io_source(source.clone())?;
    let connected = registration
        .io(Interest::Writable, || outcome(&source))
        .await;
    // The registration has no more to watch for, and the caller is about to register the source
    // again for the connection's own traffic.
    drop(registration);
    connected?;

    Ok(source)
}

/// Whether `error` says the kernel is still working on the connection.
fn is_in_progress(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::EINPROGRESS)
    }
    #[cfg(windows)]
    {
        error.kind() == io::ErrorKind::WouldBlock
    }
}

/// What has become of a connection the kernel took over.
///
/// A socket that has failed to connect keeps the reason in `SO_ERROR`, and one that has not
/// finished connecting has no peer to name: `getpeername` reports `ENOTCONN` on unix and
/// `WSAENOTCONN` on Winsock, both of which arrive here as [`io::ErrorKind::NotConnected`].
/// Reporting that as a would-block is what makes the wait a readiness wait like any other.
///
/// Both of the polling orders [`PollIo`] allows an implementation to choose between end up at the
/// same answer, which is why this predicate needs no help from either. Asked before the socket is
/// known to be ready, it says the connection is still under way and the caller waits; asked once
/// the kernel has reported writability — which it does as soon as it is done with the connection,
/// whichever way that went — it finds the error or the peer.
///
/// [`PollIo`]: crate::runtime::traits::PollIo
pub(super) fn outcome(source: &IoSource) -> io::Result<()> {
    let socket = SockRef::from(source);

    match socket.take_error()? {
        Some(e) => Err(e),
        None => match socket.peer_addr() {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotConnected => {
                Err(io::ErrorKind::WouldBlock.into())
            }
            Err(e) => Err(e),
        },
    }
}
