#![cfg(feature = "p2p")]

use std::time::Duration;

use ntest::timeout;
use test_log::test;
use tracing::{debug, instrument};

use zbus::{AuthMechanism, Guid, block_on, connection::Builder};

const UID: u32 = 0;

#[test]
#[timeout(15000)]
#[instrument]
fn issue_1003() {
    // Connect to a server using [`AuthMechanism::External`] and providing a user id
    block_on(async move {
        #[cfg(not(feature = "tokio"))]
        use std::os::unix::net::UnixStream;
        #[cfg(feature = "tokio")]
        use tokio::net::UnixStream;

        let guid = Guid::generate();

        let (p0, p1) = UnixStream::pair().unwrap();

        #[cfg(not(feature = "tokio"))]
        let (service_conn_builder, client_conn_builder) = (
            Builder::async_io_unix_stream(p0),
            Builder::async_io_unix_stream(p1),
        );
        #[cfg(feature = "tokio")]
        let (service_conn_builder, client_conn_builder) = (
            Builder::tokio_unix_stream(p0),
            Builder::tokio_unix_stream(p1),
        );
        let service_conn_builder = service_conn_builder
            .auth_mechanism(AuthMechanism::External)
            .server(guid)
            .unwrap()
            .p2p()
            .user_id(UID);
        let client_conn_builder = client_conn_builder.p2p().user_id(UID);

        let (service_conn, client_conn) = futures_util::try_join!(
            service_conn_builder.build(),
            client_conn_builder
                .method_timeout(Duration::from_millis(500))
                .build(),
        )
        .unwrap();
        debug!("Client connection created: {:?}", client_conn);
        debug!("Service connection created: {:?}", service_conn);
    });
}
