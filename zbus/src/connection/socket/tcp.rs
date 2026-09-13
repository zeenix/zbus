#[cfg(feature = "async-io")]
use async_io::Async;
#[cfg(any(feature = "async-io", feature = "tokio"))]
use std::io;
#[cfg(all(unix, any(feature = "async-io", feature = "tokio")))]
use std::os::fd::BorrowedFd;
#[cfg(feature = "async-io")]
use std::{net::TcpStream, sync::Arc};

#[cfg(any(feature = "async-io", feature = "tokio"))]
use super::{ReadHalf, RecvmsgResult, WriteHalf};
#[cfg(feature = "tokio")]
use super::{Socket, Split};

#[cfg(feature = "async-io")]
#[async_trait::async_trait]
impl ReadHalf for Arc<Async<TcpStream>> {
    async fn recvmsg(&mut self, buf: &mut [u8]) -> RecvmsgResult {
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

    #[cfg(windows)]
    async fn peer_credentials(&mut self) -> io::Result<crate::fdo::ConnectionCredentials> {
        let peer_addr = self.get_ref().peer_addr()?;
        crate::runtime::spawn_blocking(
            move || crate::runtime::io::tcp::credentials_from_addr(&peer_addr),
            "peer credentials",
        )
        .await?
    }

    #[cfg(not(windows))]
    fn auth_mechanism(&self) -> crate::conn::AuthMechanism {
        crate::conn::AuthMechanism::Anonymous
    }
}

#[cfg(feature = "async-io")]
#[async_trait::async_trait]
impl WriteHalf for Arc<Async<TcpStream>> {
    async fn sendmsg(
        &mut self,
        buf: &[u8],
        #[cfg(unix)] fds: &[BorrowedFd<'_>],
    ) -> io::Result<usize> {
        #[cfg(unix)]
        if !fds.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fds cannot be sent with a tcp stream",
            ));
        }

        futures_lite::AsyncWriteExt::write(&mut self.as_ref(), buf).await
    }

    async fn close(&mut self) -> io::Result<()> {
        let stream = self.clone();
        crate::runtime::spawn_blocking(
            move || stream.get_ref().shutdown(std::net::Shutdown::Both),
            "close socket",
        )
        .await?
    }

    async fn peer_credentials(&mut self) -> io::Result<crate::fdo::ConnectionCredentials> {
        ReadHalf::peer_credentials(self).await
    }
}

#[cfg(feature = "tokio")]
impl Socket for tokio::net::TcpStream {
    type ReadHalf = tokio::net::tcp::OwnedReadHalf;
    type WriteHalf = tokio::net::tcp::OwnedWriteHalf;

    fn split(self) -> Split<Self::ReadHalf, Self::WriteHalf> {
        let (read, write) = self.into_split();

        Split { read, write }
    }
}

#[cfg(feature = "tokio")]
#[async_trait::async_trait]
impl ReadHalf for tokio::net::tcp::OwnedReadHalf {
    async fn recvmsg(&mut self, buf: &mut [u8]) -> RecvmsgResult {
        use tokio::io::{AsyncReadExt, ReadBuf};

        let mut read_buf = ReadBuf::new(buf);
        self.read_buf(&mut read_buf).await.map(|_| {
            let ret = read_buf.filled().len();
            #[cfg(unix)]
            let ret = (ret, vec![]);

            ret
        })
    }

    #[cfg(windows)]
    async fn peer_credentials(&mut self) -> io::Result<crate::fdo::ConnectionCredentials> {
        let peer_addr = self.peer_addr()?;
        crate::runtime::spawn_blocking(
            move || crate::runtime::io::tcp::credentials_from_addr(&peer_addr),
            "peer credentials",
        )
        .await?
    }

    #[cfg(not(windows))]
    fn auth_mechanism(&self) -> crate::conn::AuthMechanism {
        crate::conn::AuthMechanism::Anonymous
    }
}

#[cfg(feature = "tokio")]
#[async_trait::async_trait]
impl WriteHalf for tokio::net::tcp::OwnedWriteHalf {
    async fn sendmsg(
        &mut self,
        buf: &[u8],
        #[cfg(unix)] fds: &[BorrowedFd<'_>],
    ) -> io::Result<usize> {
        use tokio::io::AsyncWriteExt;

        #[cfg(unix)]
        if !fds.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fds cannot be sent with a tcp stream",
            ));
        }

        self.write(buf).await
    }

    async fn close(&mut self) -> io::Result<()> {
        tokio::io::AsyncWriteExt::shutdown(self).await
    }

    #[cfg(windows)]
    async fn peer_credentials(&mut self) -> io::Result<crate::fdo::ConnectionCredentials> {
        let peer_addr = self.peer_addr()?;
        crate::runtime::spawn_blocking(
            move || crate::runtime::io::tcp::credentials_from_addr(&peer_addr),
            "peer credentials",
        )
        .await?
    }
}
