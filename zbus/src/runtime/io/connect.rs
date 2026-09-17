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
/// way, and [`outcome`] then says which way it went.
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
/// The two platforms are asked different questions, because they answer differently. A unix
/// socket has no peer until its connection is made, so `getpeername` reporting `ENOTCONN` — which
/// arrives here as [`io::ErrorKind::NotConnected`] — is the mark of one still under way, and
/// `SO_ERROR` holds the reason for one that failed. Winsock names the peer from the moment a
/// connect is issued, so there the peer is evidence of nothing; what is, is a zero-timeout
/// `select`, which puts the socket in `writefds` once the connection is made and in `exceptfds`
/// once it has failed, and in neither while it is still under way.
///
/// Either way a connection still under way is reported as a would-block, which is what makes the
/// wait a readiness wait like any other.
///
/// Both of the polling orders [`PollIo`] allows an implementation to choose between end up at the
/// same answer, which is why this predicate needs no help from either. Asked before the socket is
/// known to be ready, it says the connection is still under way and the caller waits; asked once
/// the kernel has reported writability — which it does as soon as it is done with the connection,
/// whichever way that went — it finds the settled answer.
///
/// [`PollIo`]: crate::runtime::traits::PollIo
pub(super) fn outcome(source: &IoSource) -> io::Result<()> {
    #[cfg(unix)]
    {
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
    #[cfg(windows)]
    {
        match progress(source)? {
            Progress::Connected => Ok(()),
            // Winsock is expected to keep the reason there, and a failure with none left is
            // still a failure: the connection is not to be reported as made.
            Progress::Failed => Err(SockRef::from(source)
                .take_error()?
                .unwrap_or_else(|| io::Error::other("the connection failed"))),
            Progress::UnderWay => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}

/// How far Winsock has got with a connection.
#[cfg(windows)]
enum Progress {
    /// The connection is made.
    Connected,
    /// The connection failed, and `SO_ERROR` is where the reason is.
    Failed,
    /// Winsock is still working on the connection.
    UnderWay,
}

/// What a zero-timeout `select` says about the connection on `source`.
///
/// `writefds` and `exceptfds` are the two answers Winsock has for a connect it has taken over,
/// and the zero timeout is what makes the call ask for them rather than wait for them: `select`
/// leaves a set holding only those of its sockets that the answer applies to, so an empty set is
/// a no.
#[cfg(windows)]
fn progress(source: &IoSource) -> io::Result<Progress> {
    use std::{os::windows::io::AsRawSocket, ptr};

    use windows_sys::Win32::Networking::WinSock::{
        FD_SET, SOCKET, SOCKET_ERROR, TIMEVAL, WSAGetLastError, select,
    };

    let mut connected = FD_SET {
        fd_count: 1,
        ..FD_SET::default()
    };
    connected.fd_array[0] = source.as_raw_socket() as SOCKET;
    let mut failed = connected;
    // All zero, which is the timeout that makes `select` report and return.
    let immediately = TIMEVAL::default();

    // SAFETY: `select` reads and writes the two sets and reads the timeout for the length of the
    // call and no longer, and all three live in this frame across it. The sets are in the shape
    // `select` expects, a count of one against one entry, and that entry is a socket `source`
    // holds open for the call. The first argument is ignored on Winsock.
    let ready = unsafe {
        select(
            1,
            ptr::null_mut(),
            &mut connected,
            &mut failed,
            &immediately,
        )
    };
    if ready == SOCKET_ERROR {
        // SAFETY: `WSAGetLastError` takes nothing and reads this thread's last Winsock error.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }

    // `exceptfds` also reports urgent data waiting, which a connection just made may have, so a
    // socket in both sets is a connected one; a failed connect is never in `writefds`.
    if connected.fd_count != 0 {
        Ok(Progress::Connected)
    } else if failed.fd_count != 0 {
        Ok(Progress::Failed)
    } else {
        Ok(Progress::UnderWay)
    }
}
