// The stream constructors take an owned socket of the platform's own type.
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(windows)]
use uds_windows::UnixStream;

#[cfg(feature = "p2p")]
use crate::Guid;
use crate::{
    Error, Result, address::Address, blocking::Connection, conn::AuthMechanism,
    connection::socket::BoxedSplit, names::WellKnownName, runtime::traits, utils::block_on,
};
#[cfg(feature = "service")]
use crate::{ObjectPath, object_server::Interface};

/// A builder for [`zbus::blocking::Connection`].
///
/// The constructors and setters take the same loosely typed values as the rest of the API and
/// convert them right away, but they never fail: the first error one hits is recorded — a later
/// call doesn't clear it — and reported by [`Builder::build`].
#[derive(Debug)]
#[must_use]
pub struct Builder<'a>(crate::connection::Builder<'a>);

impl<'a> Builder<'a> {
    /// Create a builder for the session/user message bus connection.
    ///
    /// A failure to find the session bus address is reported by [`Builder::build`].
    pub fn session() -> Self {
        Self(crate::connection::Builder::session())
    }

    /// Create a builder for the system-wide message bus connection.
    ///
    /// A failure to find the system bus address is reported by [`Builder::build`].
    pub fn system() -> Self {
        Self(crate::connection::Builder::system())
    }

    /// Create a builder for an IBus connection.
    ///
    /// IBus (Intelligent Input Bus) is an input method framework. This method creates a builder
    /// that will query the IBus daemon for its D-Bus address using the `ibus address` command.
    ///
    /// # Platform Support
    ///
    /// This method is available on Unix-like systems where IBus is installed.
    ///
    /// # Errors
    ///
    /// [`Builder::build`] returns an error if:
    /// - The `ibus` command is not found or fails to execute
    /// - The IBus daemon is not running
    /// - The command output cannot be parsed as a valid D-Bus address
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::error::Error;
    /// # use zbus::blocking::connection;
    /// #
    /// let _conn = connection::Builder::ibus()
    ///     .build()?;
    ///
    /// // Use the connection to interact with IBus services.
    /// # Ok::<_, Box<dyn Error + Send + Sync>>(())
    /// ```
    #[cfg(all(unix, feature = "ibus"))]
    pub fn ibus() -> Self {
        Self(crate::connection::Builder::ibus())
    }

    /// Create a builder for a connection that will use the given [D-Bus bus address].
    ///
    /// An invalid address is reported by [`Builder::build`].
    ///
    /// [D-Bus bus address]: https://dbus.freedesktop.org/doc/dbus-specification.html#addresses
    pub fn address<A>(address: A) -> Self
    where
        A: TryInto<Address>,
        A::Error: Into<Error>,
    {
        Self(crate::connection::Builder::address(address))
    }

    /// Create a builder for a connection over `stream`.
    ///
    /// The stream is a [`std::os::unix::net::UnixStream`] (or [`uds_windows::UnixStream`] on
    /// Windows), and the connection takes ownership of it: it is switched to non-blocking mode
    /// and driven by the runtime the connection is built on. A stream of another kind is handed
    /// over as the socket it wraps, which is `into_std()` for a Tokio stream.
    ///
    /// [`uds_windows::UnixStream`]: https://docs.rs/uds_windows/latest/uds_windows/struct.UnixStream.html
    #[cfg(any(unix, windows))]
    pub fn unix_stream(stream: UnixStream) -> Self {
        Self(crate::connection::Builder::unix_stream(stream))
    }

    /// Create a builder for a connection over `stream`.
    ///
    /// The stream is a [`std::net::TcpStream`], and the connection takes ownership of it: it is
    /// switched to non-blocking mode and driven by the runtime the connection is built on. A
    /// stream of another kind is handed over as the socket it wraps, which is `into_std()` for a
    /// Tokio stream.
    pub fn tcp_stream(stream: std::net::TcpStream) -> Self {
        Self(crate::connection::Builder::tcp_stream(stream))
    }

    /// Create a builder for a connection that will use the given pre-authenticated socket.
    ///
    /// This is similar to [`Builder::socket`], except that the socket is either already
    /// authenticated or does not require authentication.
    ///
    /// An invalid GUID is reported by [`Builder::build`].
    pub fn authenticated_socket<S, G>(socket: S, guid: G) -> Self
    where
        S: Into<BoxedSplit>,
        G: TryInto<crate::Guid<'a>>,
        G::Error: Into<Error>,
    {
        Self(crate::connection::Builder::authenticated_socket(
            socket, guid,
        ))
    }

    /// Create a builder for a connection that will use the given socket.
    pub fn socket<S: Into<BoxedSplit>>(socket: S) -> Self {
        Self(crate::connection::Builder::socket(socket))
    }

    /// Specify the mechanism to use during authentication.
    pub fn auth_mechanism(self, auth_mechanism: AuthMechanism) -> Self {
        Self(self.0.auth_mechanism(auth_mechanism))
    }

    /// Specify the user id during authentication.
    ///
    /// This can be useful when using [`AuthMechanism::External`] with `socat`
    /// to avoid the host decide what uid to use and instead provide one
    /// known to have access rights.
    #[cfg(unix)]
    pub fn user_id(self, id: u32) -> Self {
        Self(self.0.user_id(id))
    }

    /// The to-be-created connection will be a peer-to-peer connection.
    ///
    /// This method is only available when the `p2p` feature is enabled.
    #[cfg(feature = "p2p")]
    pub fn p2p(self) -> Self {
        Self(self.0.p2p())
    }

    /// The to-be-created connection will be a server using the given GUID.
    ///
    /// The to-be-created connection will wait for incoming client authentication handshake and
    /// negotiation messages, for peer-to-peer communications after successful creation.
    ///
    /// This method is only available when the `p2p` feature is enabled.
    ///
    /// **NOTE:** This method is redundant when using [`Builder::authenticated_socket`] since the
    /// latter already sets the GUID for the connection and zbus doesn't differentiate between a
    /// server and a client connection, except for authentication.
    ///
    /// An invalid GUID is reported by [`Builder::build`].
    #[cfg(feature = "p2p")]
    pub fn server<G>(self, guid: G) -> Self
    where
        G: TryInto<Guid<'a>>,
        G::Error: Into<Error>,
    {
        Self(self.0.server(guid))
    }

    /// Set the capacity of the main (unfiltered) queue.
    ///
    /// Since typically you'd want to set this at instantiation time, you can set it through the
    /// builder.
    ///
    /// # Example
    ///
    /// ```
    /// # use std::error::Error;
    /// # use zbus::blocking::connection;
    /// #
    /// let conn = connection::Builder::session()
    ///     .max_queued(30)
    ///     .build()?;
    /// assert_eq!(conn.max_queued(), 30);
    ///
    /// // Do something useful with `conn`..
    /// # Ok::<_, Box<dyn Error + Send + Sync>>(())
    /// ```
    pub fn max_queued(self, max: usize) -> Self {
        Self(self.0.max_queued(max))
    }

    /// Register a D-Bus [`Interface`] to be served at a given path.
    ///
    /// This is similar to [`zbus::blocking::ObjectServer::at`], except that it allows you to have
    /// your interfaces available immediately after the connection is established. Typically, this
    /// is exactly what you'd want. Also in contrast to [`zbus::blocking::ObjectServer::at`], this
    /// method will replace any previously added interface with the same name at the same path.
    ///
    /// An invalid path is reported by [`Builder::build`].
    #[cfg(feature = "service")]
    pub fn serve_at<P, I>(self, path: P, iface: I) -> Self
    where
        I: Interface,
        P: TryInto<ObjectPath<'a>>,
        P::Error: Into<Error>,
    {
        Self(self.0.serve_at(path, iface))
    }

    /// Register a well-known name for this connection on the bus.
    ///
    /// This is similar to [`zbus::blocking::Connection::request_name`], except the name is
    /// requested as part of the connection setup ([`Builder::build`]), immediately after
    #[cfg_attr(
        feature = "service",
        doc = "interfaces registered (through [`Builder::serve_at`]) are advertised. Typically"
    )]
    #[cfg_attr(
        not(feature = "service"),
        doc = "interfaces registered (through `Builder::serve_at`, which requires the `service`",
        doc = "feature) are advertised. Typically"
    )]
    /// this is exactly what you want.
    ///
    /// An invalid name is reported by [`Builder::build`].
    pub fn name<W>(self, well_known_name: W) -> Self
    where
        W: TryInto<WellKnownName<'a>>,
        W::Error: Into<Error>,
    {
        Self(self.0.name(well_known_name))
    }

    /// Whether the [`zbus::fdo::RequestNameFlags::AllowReplacement`] flag will be set when
    /// requesting names.
    pub fn allow_name_replacements(self, allow_replacement: bool) -> Self {
        Self(self.0.allow_name_replacements(allow_replacement))
    }

    /// Whether the [`zbus::fdo::RequestNameFlags::ReplaceExisting`] flag will be set when
    /// requesting names.
    pub fn replace_existing_names(self, replace_existing: bool) -> Self {
        Self(self.0.replace_existing_names(replace_existing))
    }

    /// Set the unique name of the connection.
    ///
    /// This method is only available when the `bus-impl` feature is enabled.
    ///
    /// # Panics
    ///
    /// This method panics if the to-be-created connection is not a peer-to-peer connection.
    /// It will always panic if the connection is to a message bus as it's the bus that assigns
    /// peers their unique names. This is mainly provided for bus implementations. All other users
    /// should not need to use this method.
    ///
    /// An invalid name is reported by [`Builder::build`].
    #[cfg(feature = "bus-impl")]
    pub fn unique_name<U>(self, unique_name: U) -> Self
    where
        U: TryInto<crate::names::UniqueName<'a>>,
        U::Error: Into<Error>,
    {
        Self(self.0.unique_name(unique_name))
    }

    /// Set a timeout for method calls.
    ///
    /// Method calls will return
    /// `zbus::Error::InputOutput(std::io::Error(kind: ErrorKind::TimedOut))` if a client does not
    /// receive an answer from a service in time.
    pub fn method_timeout(self, timeout: std::time::Duration) -> Self {
        Self(self.0.method_timeout(timeout))
    }

    /// Run the connection on `runtime`.
    ///
    /// See [`crate::connection::Builder::runtime`] for what the connection takes from it.
    pub fn runtime(self, runtime: impl traits::Runtime) -> Self {
        Self(self.0.runtime(runtime))
    }

    /// Build the connection, consuming the builder.
    ///
    /// # Errors
    ///
    /// Returns the first error recorded by a constructor or setter, then any error from
    /// connecting, authenticating or setting the connection up.
    ///
    /// Until server-side bus connection is supported, attempting to build such a connection will
    /// result in a [`Error::Unsupported`] error.
    pub fn build(self) -> Result<Connection> {
        block_on(self.0.build()).map(Into::into)
    }

    /// Build the connection and return a [`MessageIterator`] to receive messages from it.
    ///
    /// This is the blocking counterpart of [`crate::connection::Builder::build_message_stream`].
    /// The iterator is set up **before** the socket-reader task is started, so no messages can
    /// be lost in the window between the connection being built and the iterator being created.
    /// Use this when the peer may pipeline traffic right after authentication — e.g. a bus
    /// implementation reading a `Hello` method call from a just-connected client.
    ///
    /// To get the [`Connection`] out of the returned iterator, use `Connection::from(&iter)`.
    ///
    /// This method is only available when the `bus-impl` feature is enabled.
    ///
    /// [`MessageIterator`]: crate::blocking::MessageIterator
    #[cfg(feature = "bus-impl")]
    pub fn build_message_iterator(self) -> Result<crate::blocking::MessageIterator> {
        block_on(self.0.build_message_stream())
            .map(|azync| crate::blocking::MessageIterator { azync: Some(azync) })
    }
}
