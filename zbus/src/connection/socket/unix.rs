#[cfg(feature = "async-io")]
use async_io::Async;
#[cfg(all(unix, any(feature = "async-io", feature = "tokio")))]
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd};
#[cfg(all(unix, feature = "async-io"))]
use std::os::unix::net::UnixStream;
#[cfg(feature = "async-io")]
use std::sync::Arc;
#[cfg(all(unix, any(feature = "async-io", feature = "tokio")))]
use std::{future::poll_fn, task::Poll};
#[cfg(all(windows, feature = "async-io"))]
use uds_windows::UnixStream;

#[cfg(all(
    any(target_os = "freebsd", target_os = "dragonfly"),
    any(feature = "async-io", feature = "tokio")
))]
use crate::runtime::io::unix::send_credentials_byte;
#[cfg(all(unix, any(feature = "async-io", feature = "tokio")))]
use crate::runtime::io::unix::{fd_recvmsg, fd_sendmsg, get_unix_peer_creds_blocking};

#[cfg(all(unix, feature = "async-io"))]
#[async_trait::async_trait]
impl super::ReadHalf for Arc<Async<UnixStream>> {
    async fn recvmsg(&mut self, buf: &mut [u8]) -> super::RecvmsgResult {
        poll_fn(|cx| {
            let (len, fds) = loop {
                match fd_recvmsg(self.as_fd(), buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        match self.poll_readable(cx) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(res) => res?,
                        }
                    }
                    v => break v?,
                }
            };
            Poll::Ready(Ok((len, fds)))
        })
        .await
    }

    /// Supports passing file descriptors.
    fn can_pass_unix_fd(&self) -> bool {
        true
    }

    async fn peer_credentials(&mut self) -> std::io::Result<crate::fdo::ConnectionCredentials> {
        get_unix_peer_creds(self).await
    }
}

#[cfg(all(unix, feature = "async-io"))]
#[async_trait::async_trait]
impl super::WriteHalf for Arc<Async<UnixStream>> {
    async fn sendmsg(
        &mut self,
        buffer: &[u8],
        #[cfg(unix)] fds: &[BorrowedFd<'_>],
    ) -> std::io::Result<usize> {
        poll_fn(|cx| {
            loop {
                match fd_sendmsg(
                    self.as_fd(),
                    buffer,
                    #[cfg(unix)]
                    fds,
                ) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        match self.poll_writable(cx) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(res) => res?,
                        }
                    }
                    v => return Poll::Ready(v),
                }
            }
        })
        .await
    }

    async fn close(&mut self) -> std::io::Result<()> {
        let stream = self.clone();
        crate::runtime::spawn_blocking(
            move || stream.get_ref().shutdown(std::net::Shutdown::Both),
            "close socket",
        )
        .await?
    }

    #[cfg(any(target_os = "freebsd", target_os = "dragonfly"))]
    async fn send_zero_byte(&mut self) -> std::io::Result<Option<usize>> {
        send_zero_byte(self).await.map(Some)
    }

    /// Supports passing file descriptors.
    fn can_pass_unix_fd(&self) -> bool {
        true
    }

    async fn peer_credentials(&mut self) -> std::io::Result<crate::fdo::ConnectionCredentials> {
        super::ReadHalf::peer_credentials(self).await
    }
}

#[cfg(all(unix, feature = "tokio"))]
impl super::Socket for tokio::net::UnixStream {
    type ReadHalf = tokio::net::unix::OwnedReadHalf;
    type WriteHalf = tokio::net::unix::OwnedWriteHalf;

    fn split(self) -> super::Split<Self::ReadHalf, Self::WriteHalf> {
        let (read, write) = self.into_split();

        super::Split { read, write }
    }
}

#[cfg(all(unix, feature = "tokio"))]
#[async_trait::async_trait]
impl super::ReadHalf for tokio::net::unix::OwnedReadHalf {
    async fn recvmsg(&mut self, buf: &mut [u8]) -> super::RecvmsgResult {
        let stream = self.as_ref();
        poll_fn(|cx| {
            loop {
                match stream.try_io(tokio::io::Interest::READABLE, || {
                    // We use own custom function for reading because we need to receive file
                    // descriptors too.
                    fd_recvmsg(stream.as_fd(), buf)
                }) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        match stream.poll_read_ready(cx) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(res) => res?,
                        }
                    }
                    v => return Poll::Ready(v),
                }
            }
        })
        .await
    }

    /// Supports passing file descriptors.
    fn can_pass_unix_fd(&self) -> bool {
        true
    }

    async fn peer_credentials(&mut self) -> std::io::Result<crate::fdo::ConnectionCredentials> {
        get_unix_peer_creds(self.as_ref()).await
    }
}

#[cfg(all(unix, feature = "tokio"))]
#[async_trait::async_trait]
impl super::WriteHalf for tokio::net::unix::OwnedWriteHalf {
    async fn sendmsg(
        &mut self,
        buffer: &[u8],
        #[cfg(unix)] fds: &[BorrowedFd<'_>],
    ) -> std::io::Result<usize> {
        let stream = self.as_ref();
        poll_fn(|cx| {
            loop {
                match stream.try_io(tokio::io::Interest::WRITABLE, || {
                    fd_sendmsg(
                        stream.as_fd(),
                        buffer,
                        #[cfg(unix)]
                        fds,
                    )
                }) {
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        match stream.poll_write_ready(cx) {
                            Poll::Pending => return Poll::Pending,
                            Poll::Ready(res) => res?,
                        }
                    }
                    v => return Poll::Ready(v),
                }
            }
        })
        .await
    }

    async fn close(&mut self) -> std::io::Result<()> {
        tokio::io::AsyncWriteExt::shutdown(self).await
    }

    #[cfg(any(target_os = "freebsd", target_os = "dragonfly"))]
    async fn send_zero_byte(&mut self) -> std::io::Result<Option<usize>> {
        send_zero_byte(self.as_ref()).await.map(Some)
    }

    /// Supports passing file descriptors.
    fn can_pass_unix_fd(&self) -> bool {
        true
    }

    async fn peer_credentials(&mut self) -> std::io::Result<crate::fdo::ConnectionCredentials> {
        get_unix_peer_creds(self.as_ref()).await
    }
}

#[cfg(all(windows, feature = "async-io"))]
#[async_trait::async_trait]
impl super::ReadHalf for Arc<Async<UnixStream>> {
    async fn recvmsg(&mut self, buf: &mut [u8]) -> super::RecvmsgResult {
        match futures_lite::AsyncReadExt::read(&mut self.as_ref(), buf).await {
            Err(e) => Err(e),
            Ok(len) => {
                #[cfg(unix)]
                let ret = (len, vec![]);
                #[cfg(not(unix))]
                let ret = len;
                Ok(ret)
            }
        }
    }

    async fn peer_credentials(&mut self) -> std::io::Result<crate::fdo::ConnectionCredentials> {
        let stream = self.clone();
        crate::runtime::spawn_blocking(
            move || crate::runtime::io::unix::credentials_from_socket(stream.get_ref()),
            "peer credentials",
        )
        .await?
    }
}

#[cfg(all(windows, feature = "async-io"))]
#[async_trait::async_trait]
impl super::WriteHalf for Arc<Async<UnixStream>> {
    async fn sendmsg(
        &mut self,
        buf: &[u8],
        #[cfg(unix)] _fds: &[BorrowedFd<'_>],
    ) -> std::io::Result<usize> {
        futures_lite::AsyncWriteExt::write(&mut self.as_ref(), buf).await
    }

    async fn close(&mut self) -> std::io::Result<()> {
        let stream = self.clone();
        crate::runtime::spawn_blocking(
            move || stream.get_ref().shutdown(std::net::Shutdown::Both),
            "close socket",
        )
        .await?
    }

    async fn peer_credentials(&mut self) -> std::io::Result<crate::fdo::ConnectionCredentials> {
        super::ReadHalf::peer_credentials(self).await
    }
}

#[cfg(all(unix, any(feature = "async-io", feature = "tokio")))]
async fn get_unix_peer_creds(fd: &impl AsFd) -> std::io::Result<crate::fdo::ConnectionCredentials> {
    let fd = fd.as_fd().as_raw_fd();
    // FIXME: Is it likely enough for sending of 1 byte to block, to justify a task (possibly
    // launching a thread in turn)?
    crate::runtime::spawn_blocking(move || get_unix_peer_creds_blocking(fd), "peer credentials")
        .await?
}

// Send 0 byte as a separate SCM_CREDS message.
#[cfg(all(
    any(target_os = "freebsd", target_os = "dragonfly"),
    any(feature = "async-io", feature = "tokio")
))]
async fn send_zero_byte(fd: &impl AsFd) -> std::io::Result<usize> {
    let fd = fd.as_fd().as_raw_fd();
    crate::runtime::spawn_blocking(move || send_credentials_byte(fd), "send zero byte").await?
}
