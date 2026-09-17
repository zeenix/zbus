//! The TCP stream a Tokio connection uses on Windows.

use std::{io, net::SocketAddr};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadBuf},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
};

use super::{ReadHalf, RecvmsgResult, Socket, Split, WriteHalf};
use crate::{
    fdo::ConnectionCredentials,
    runtime::{Runtime, io::tcp::credentials_from_addr},
};

/// A TCP stream Tokio owns and watches itself.
///
/// Tokio has no way to watch a Windows socket a connection keeps a handle of its own to, so this
/// is the one socket whose readiness a connection does not drive through its runtime. The
/// blocking part of a peer-credentials lookup goes to the runtime as for any other socket.
#[derive(Debug)]
pub(crate) struct TokioTcp {
    stream: TcpStream,
    runtime: Runtime,
}

impl TokioTcp {
    /// Wraps `stream`, whose blocking work goes to `runtime`.
    pub(crate) fn new(stream: TcpStream, runtime: Runtime) -> Self {
        Self { stream, runtime }
    }

    /// Writes the whole of `bytes` to the stream.
    pub(crate) async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await
    }
}

impl Socket for TokioTcp {
    type ReadHalf = Reader;
    type WriteHalf = Writer;

    fn split(self) -> Split<Reader, Writer> {
        let (read, write) = self.stream.into_split();

        Split::new(
            Reader {
                half: read,
                runtime: self.runtime.clone(),
            },
            Writer {
                half: write,
                runtime: self.runtime,
            },
        )
    }
}

/// The reading half of a [`TokioTcp`].
#[derive(Debug)]
pub(crate) struct Reader {
    half: OwnedReadHalf,
    runtime: Runtime,
}

#[async_trait::async_trait]
impl ReadHalf for Reader {
    async fn recvmsg(&mut self, buffer: &mut [u8]) -> RecvmsgResult {
        let mut read = ReadBuf::new(buffer);

        self.half
            .read_buf(&mut read)
            .await
            .map(|_| read.filled().len())
    }

    async fn peer_credentials(&mut self) -> io::Result<ConnectionCredentials> {
        peer_credentials(self.half.peer_addr()?, &self.runtime).await
    }
}

/// The writing half of a [`TokioTcp`].
#[derive(Debug)]
pub(crate) struct Writer {
    half: OwnedWriteHalf,
    runtime: Runtime,
}

#[async_trait::async_trait]
impl WriteHalf for Writer {
    async fn sendmsg(
        &mut self,
        buffer: &[u8],
        // The trait carries this parameter on unix, where this socket never exists; it is here so
        // that the two signatures agree.
        #[cfg(unix)] _fds: &[std::os::fd::BorrowedFd<'_>],
    ) -> io::Result<usize> {
        self.half.write(buffer).await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.half.shutdown().await
    }

    async fn peer_credentials(&mut self) -> io::Result<ConnectionCredentials> {
        peer_credentials(self.half.peer_addr()?, &self.runtime).await
    }
}

/// The credentials Windows reports for the peer at `address`.
///
/// Finding the peer means walking the machine's whole table of TCP connections, which is not work
/// for the thread a connection is polled on, so it goes to `runtime`.
async fn peer_credentials(
    address: SocketAddr,
    runtime: &Runtime,
) -> io::Result<ConnectionCredentials> {
    runtime
        .spawn_blocking(move || credentials_from_addr(&address))
        .await
}
