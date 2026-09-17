#[cfg(all(feature = "vsock", feature = "async-io"))]
#[async_trait::async_trait]
impl super::ReadHalf for std::sync::Arc<async_io::Async<vsock::VsockStream>> {
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

    fn auth_mechanism(&self) -> crate::conn::AuthMechanism {
        crate::conn::AuthMechanism::Anonymous
    }
}

#[cfg(all(feature = "vsock", feature = "async-io"))]
#[async_trait::async_trait]
impl super::WriteHalf for std::sync::Arc<async_io::Async<vsock::VsockStream>> {
    async fn sendmsg(
        &mut self,
        buf: &[u8],
        #[cfg(unix)] fds: &[std::os::fd::BorrowedFd<'_>],
    ) -> std::io::Result<usize> {
        use std::io;

        #[cfg(unix)]
        if !fds.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fds cannot be sent with a vsock stream",
            ));
        }

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
}
