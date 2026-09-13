mod auth_mechanism;
mod client;
mod command;
mod common;
#[cfg(feature = "p2p")]
mod server;

use async_trait::async_trait;
#[cfg(unix)]
use rustix::process::geteuid;
use std::fmt::Debug;

#[cfg(windows)]
use crate::win32;
use crate::{Error, OwnedGuid, Result, names::OwnedUniqueName};

use super::socket::{BoxedSplit, ReadHalf, WriteHalf};

pub use auth_mechanism::AuthMechanism;
use client::Client;
use command::Command;
use common::Common;
#[cfg(feature = "p2p")]
use server::Server;

/// The result of a finalized handshake
///
/// The result of a finalized [`ClientHandshake`] or [`ServerHandshake`].
///
/// [`ClientHandshake`]: struct.ClientHandshake.html
/// [`ServerHandshake`]: struct.ServerHandshake.html
#[derive(Debug)]
pub struct Authenticated {
    pub(crate) socket_write: Box<dyn WriteHalf>,
    /// The server Guid
    pub(crate) server_guid: OwnedGuid,
    /// Whether file descriptor passing has been accepted by both sides
    #[cfg(unix)]
    pub(crate) cap_unix_fd: bool,

    pub(crate) socket_read: Option<Box<dyn ReadHalf>>,
    pub(crate) already_received_bytes: Vec<u8>,
    #[cfg(unix)]
    pub(crate) already_received_fds: Vec<std::os::fd::OwnedFd>,
    pub(crate) unique_name: Option<OwnedUniqueName>,
}

impl Authenticated {
    /// Create a client-side `Authenticated` for the given `socket`.
    pub async fn client(
        socket: BoxedSplit,
        server_guid: Option<OwnedGuid>,
        mechanism: Option<AuthMechanism>,
        bus: bool,
        user_id: Option<u32>,
    ) -> Result<Self> {
        Client::new(socket, mechanism, server_guid, bus, user_id)
            .perform()
            .await
    }

    /// Create a server-side `Authenticated` for the given `socket`.
    ///
    /// The function takes `client_uid` on Unix only. On Windows, it takes `client_sid` instead.
    #[cfg(feature = "p2p")]
    pub async fn server(
        socket: BoxedSplit,
        guid: OwnedGuid,
        #[cfg(unix)] client_uid: Option<u32>,
        #[cfg(windows)] client_sid: Option<String>,
        auth_mechanism: Option<AuthMechanism>,
        unique_name: Option<OwnedUniqueName>,
    ) -> Result<Self> {
        Server::new(
            socket,
            guid,
            #[cfg(unix)]
            client_uid,
            #[cfg(windows)]
            client_sid,
            auth_mechanism,
            unique_name,
        )?
        .perform()
        .await
    }
}

#[async_trait]
pub trait Handshake {
    /// Perform the handshake.
    ///
    /// On a successful handshake, you get an `Authenticated`. If you need to send a Bus Hello,
    /// this remains to be done.
    async fn perform(mut self) -> Result<Authenticated>;
}

fn sasl_auth_id() -> Result<String> {
    let id = {
        #[cfg(unix)]
        {
            geteuid().as_raw().to_string()
        }

        #[cfg(windows)]
        {
            win32::ProcessToken::open(None)?.sid()?
        }
    };

    Ok(id)
}

// The handshake runs over a real socket pair.
#[cfg(all(test, unix, feature = "p2p"))]
mod tests {
    use std::{io::Write, os::unix::net::UnixStream};

    use futures_util::future::join;
    use ntest::timeout;
    use test_log::test;

    use super::*;
    use crate::{
        Guid,
        connection::socket::BoxedSplit,
        runtime::{
            Runtime,
            io::{UnixOps, registered},
            test_runtime::under_every_runtime,
        },
    };

    #[test]
    #[timeout(15000)]
    fn handshake() {
        under_every_runtime(|runtime| async move {
            let (p0, p1) = socket_pair(&runtime);

            let guid = OwnedGuid::from(Guid::generate());
            let client = Client::new(p0, None, Some(guid.clone()), false, None);
            let server = Server::new(p1, guid, Some(geteuid().as_raw()), None, None).unwrap();

            let (client, server) =
                join(async move { client.perform().await.unwrap() }, async move {
                    server.perform().await.unwrap()
                })
                .await;

            assert_eq!(client.server_guid, server.server_guid);
            assert_eq!(client.cap_unix_fd, server.cap_unix_fd);
        });
    }

    #[test]
    #[timeout(15000)]
    fn pipelined_handshake() {
        let commands = format!(
            "\0AUTH EXTERNAL {}\r\nNEGOTIATE_UNIX_FD\r\nBEGIN\r\n",
            hex::encode(sasl_auth_id().unwrap()),
        );

        under_every_runtime(|runtime| {
            let commands = commands.clone();

            async move {
                let server = server_greeted_with(&runtime, commands.as_bytes(), None);

                assert!(server.await.unwrap().cap_unix_fd);
            }
        });
    }

    #[test]
    #[timeout(15000)]
    fn separate_external_data() {
        let commands = format!(
            "\0AUTH EXTERNAL\r\nDATA {}\r\nBEGIN\r\n",
            hex::encode(sasl_auth_id().unwrap()),
        );

        under_every_runtime(|runtime| {
            let commands = commands.clone();

            async move {
                server_greeted_with(&runtime, commands.as_bytes(), None)
                    .await
                    .unwrap();
            }
        });
    }

    #[test]
    #[timeout(15000)]
    fn missing_external_data() {
        under_every_runtime(|runtime| async move {
            server_greeted_with(&runtime, b"\0AUTH EXTERNAL\r\nDATA\r\nBEGIN\r\n", None)
                .await
                .unwrap();
        });
    }

    #[test]
    #[timeout(15000)]
    fn anonymous_handshake() {
        under_every_runtime(|runtime| async move {
            server_greeted_with(
                &runtime,
                b"\0AUTH ANONYMOUS abcd\r\nBEGIN\r\n",
                Some(AuthMechanism::Anonymous),
            )
            .await
            .unwrap();
        });
    }

    #[test]
    #[timeout(15000)]
    fn separate_anonymous_data() {
        under_every_runtime(|runtime| async move {
            server_greeted_with(
                &runtime,
                b"\0AUTH ANONYMOUS\r\nDATA abcd\r\nBEGIN\r\n",
                Some(AuthMechanism::Anonymous),
            )
            .await
            .unwrap();
        });
    }

    /// A server handshake over a socket a client has already sent `commands` down.
    ///
    /// Everything the client has to say is in the socket before the server reads a byte, which is
    /// what a client that pipelines its commands does.
    fn server_greeted_with(
        runtime: &Runtime,
        commands: &[u8],
        mechanism: Option<AuthMechanism>,
    ) -> impl Future<Output = Result<Authenticated>> {
        let (client, server) = UnixStream::pair().unwrap();
        (&client).write_all(commands).unwrap();

        let server = Server::new(
            registered(runtime, server, UnixOps).unwrap().into(),
            Guid::generate().into(),
            Some(geteuid().as_raw()),
            mechanism,
            None,
        )
        .unwrap();

        async move {
            let authenticated = server.perform().await;
            drop(client);

            authenticated
        }
    }

    /// Both ends of a socket pair, each registered on `runtime`.
    fn socket_pair(runtime: &Runtime) -> (BoxedSplit, BoxedSplit) {
        let (p0, p1) = UnixStream::pair().unwrap();

        (
            registered(runtime, p0, UnixOps).unwrap().into(),
            registered(runtime, p1, UnixOps).unwrap().into(),
        )
    }
}
