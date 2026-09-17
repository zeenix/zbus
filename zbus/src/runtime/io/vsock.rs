//! The operations of a VSOCK stream.

use std::{io, os::fd::BorrowedFd};

use super::SocketOps;
use crate::{conn::AuthMechanism, connection::socket::RecvmsgResult, runtime::IoSource};

/// The operations of a VSOCK stream.
#[derive(Debug)]
pub(crate) struct VsockOps;

impl SocketOps for VsockOps {
    fn recv(&self, source: &IoSource, buffer: &mut [u8]) -> RecvmsgResult {
        super::recv(source, buffer)
    }

    fn send(&self, source: &IoSource, buffer: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
        if !fds.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fds cannot be sent with a vsock stream",
            ));
        }

        super::send(source, buffer)
    }

    /// A VSOCK peer proves nothing about itself, so it authenticates anonymously.
    fn auth_mechanism(&self) -> AuthMechanism {
        AuthMechanism::Anonymous
    }
}
