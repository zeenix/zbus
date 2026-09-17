use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};

use socket2::{Domain, Type};

use super::encode_percents;
use crate::{
    Address, Error, Result,
    connection::socket::{BoxedSplit, WriteHalf},
    runtime::{
        Runtime,
        io::{RegisteredIo, TcpOps, connect},
    },
};

/// A TCP transport in a D-Bus address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tcp {
    pub(super) host: String,
    pub(super) bind: Option<String>,
    pub(super) port: u16,
    pub(super) family: Option<TcpTransportFamily>,
    pub(super) nonce_file: Option<Vec<u8>>,
}

impl Tcp {
    /// Create a new TCP transport with the given host and port.
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_owned(),
            port,
            bind: None,
            family: None,
            nonce_file: None,
        }
    }

    /// Set the `tcp:` address `bind` value.
    pub fn set_bind(mut self, bind: Option<String>) -> Self {
        self.bind = bind;

        self
    }

    /// Set the `tcp:` address `family` value.
    pub fn set_family(mut self, family: Option<TcpTransportFamily>) -> Self {
        self.family = family;

        self
    }

    /// Set the `tcp:` address `noncefile` value.
    pub fn set_nonce_file(mut self, nonce_file: Option<Vec<u8>>) -> Self {
        self.nonce_file = nonce_file;

        self
    }

    /// The `tcp:` address `host` value.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The `tcp:` address `bind` value.
    pub fn bind(&self) -> Option<&str> {
        self.bind.as_deref()
    }

    /// The `tcp:` address `port` value.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The `tcp:` address `family` value.
    pub fn family(&self) -> Option<TcpTransportFamily> {
        self.family
    }

    /// The nonce file path, if any.
    pub fn nonce_file(&self) -> Option<&[u8]> {
        self.nonce_file.as_deref()
    }

    /// Take ownership of the nonce file path, if any.
    pub fn take_nonce_file(&mut self) -> Option<Vec<u8>> {
        self.nonce_file.take()
    }

    pub(super) fn from_options(
        opts: HashMap<&str, &str>,
        nonce_tcp_required: bool,
    ) -> Result<Self> {
        let bind = None;
        if opts.contains_key("bind") {
            return Err(Error::Address("`bind` isn't yet supported".into()));
        }

        let host = opts
            .get("host")
            .ok_or_else(|| Error::Address("tcp address is missing `host`".into()))?
            .to_string();
        let port = opts
            .get("port")
            .ok_or_else(|| Error::Address("tcp address is missing `port`".into()))?;
        let port = port
            .parse::<u16>()
            .map_err(|_| Error::Address("invalid tcp `port`".into()))?;
        let family = opts
            .get("family")
            .map(|f| TcpTransportFamily::from_str(f))
            .transpose()?;
        let nonce_file = opts
            .get("noncefile")
            .map(|f| super::decode_percents(f))
            .transpose()?;
        if nonce_tcp_required && nonce_file.is_none() {
            return Err(Error::Address(
                "nonce-tcp address is missing `noncefile`".into(),
            ));
        }

        Ok(Self {
            host,
            bind,
            port,
            family,
            nonce_file,
        })
    }

    /// Connects to this address, passing the nonce it names along where there is one.
    pub(super) async fn connect(
        mut self,
        address: &Address,
        runtime: &Runtime,
    ) -> Result<BoxedSplit> {
        let nonce = match self.take_nonce_file() {
            Some(path) => Some(read_nonce(path, runtime).await?),
            None => None,
        };
        let addresses = self.resolve(address, runtime).await?;
        let mut error = self.nothing_found(address);

        #[cfg(all(windows, feature = "tokio"))]
        if let Runtime::Tokio(_) = runtime {
            return tokio_connect(&addresses, address, nonce.as_deref(), runtime, error).await;
        }

        for socket_address in addresses {
            let domain = Domain::for_address(socket_address);
            match connect(runtime, domain, Type::STREAM, &socket_address.into()).await {
                Ok(source) => {
                    let mut split = BoxedSplit::from(RegisteredIo::new(runtime, source, TcpOps)?);
                    if let Some(nonce) = nonce.as_deref() {
                        write_all(split.write_mut(), nonce).await?;
                    }

                    return Ok(split);
                }
                Err(e) => error = Error::Connection(Arc::new(e), Box::new(address.clone())),
            }
        }

        Err(error)
    }

    /// The socket addresses this one names, in the order they should be tried.
    ///
    /// A host written as an IP address is one already. Anything else is a name for the resolver,
    /// which goes to `runtime` because looking it up blocks.
    async fn resolve(&self, address: &Address, runtime: &Runtime) -> Result<Vec<SocketAddr>> {
        let port = self.port();
        let addresses = match self.host().parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => {
                let host = self.host().to_owned();

                runtime
                    .spawn_blocking(move || {
                        (host.as_str(), port).to_socket_addrs().map(Vec::from_iter)
                    })
                    .await
                    .map_err(|e| Error::Connection(Arc::new(e), Box::new(address.clone())))?
            }
        };

        Ok(match self.family() {
            Some(TcpTransportFamily::Ipv4) => {
                addresses.into_iter().filter(SocketAddr::is_ipv4).collect()
            }
            Some(TcpTransportFamily::Ipv6) => {
                addresses.into_iter().filter(SocketAddr::is_ipv6).collect()
            }
            None => addresses,
        })
    }

    /// The error a connection attempt that found nowhere to go reports.
    fn nothing_found(&self, address: &Address) -> Error {
        Error::Address(match self.family() {
            Some(family) => format!("no `{family}` addresses found for `{address}`"),
            None => format!("no addresses found for `{address}`"),
        })
    }
}

/// The nonce the server expects before anything else, read off `path`.
async fn read_nonce(path: Vec<u8>, runtime: &Runtime) -> Result<Vec<u8>> {
    let path = nonce_path(path)?;

    runtime
        .spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(Into::into)
}

/// `path`, as this platform names files.
fn nonce_path(path: Vec<u8>) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        Ok(PathBuf::from(std::ffi::OsString::from_vec(path)))
    }
    #[cfg(windows)]
    {
        String::from_utf8(path)
            .map(PathBuf::from)
            .map_err(|_| Error::Address("nonce file path is invalid UTF-8".to_owned()))
    }
}

/// Writes the whole of `bytes` to `half`, however many writes that takes.
async fn write_all(half: &mut impl WriteHalf, bytes: &[u8]) -> Result<()> {
    let mut rest = bytes;

    while !rest.is_empty() {
        let written = half
            .sendmsg(
                rest,
                #[cfg(unix)]
                &[],
            )
            .await?;
        rest = &rest[written..];
    }

    Ok(())
}

/// Connects to the first of `addresses` that answers, on a stream Tokio drives itself.
///
/// Each attempt is made as a task on the connection's own runtime, because a socket of Tokio's
/// belongs to whichever runtime is current where it is created, and that is not something this
/// future can tell from where it is polled: reading the nonce file and looking the host up have
/// each given it a chance to move, and a host that drives zbus itself polls it outside Tokio
/// altogether. The runtime the connection was built with is the one that has to end up watching
/// the socket, so the connect goes to it rather than to whatever is current.
///
/// `error` is what a run out of addresses reports, unless one of them said something of its own.
#[cfg(all(windows, feature = "tokio"))]
async fn tokio_connect(
    addresses: &[SocketAddr],
    address: &Address,
    nonce: Option<&[u8]>,
    runtime: &Runtime,
    mut error: Error,
) -> Result<BoxedSplit> {
    use crate::connection::socket::TokioTcp;

    for &socket_address in addresses {
        let connecting = runtime.spawn("tcp connect", async move {
            tokio::net::TcpStream::connect(socket_address).await
        });

        // The outer result is the task's own, which a runtime that lost the task reports; the
        // inner one is the connect's. Either failure is this address's to record.
        match connecting.await.and_then(|connected| connected) {
            Ok(stream) => {
                let mut stream = TokioTcp::new(stream, runtime.clone());
                if let Some(nonce) = nonce {
                    stream.write_all(nonce).await?;
                }

                return Ok(stream.into());
            }
            Err(e) => error = Error::Connection(Arc::new(e), Box::new(address.clone())),
        }
    }

    Err(error)
}

impl Display for Tcp {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.nonce_file() {
            Some(nonce_file) => {
                f.write_str("nonce-tcp:noncefile=")?;
                encode_percents(f, nonce_file)?;
                f.write_str(",")?;
            }
            None => f.write_str("tcp:")?,
        }
        f.write_str("host=")?;

        encode_percents(f, self.host().as_bytes())?;

        write!(f, ",port={}", self.port())?;

        if let Some(bind) = self.bind() {
            f.write_str(",bind=")?;
            encode_percents(f, bind.as_bytes())?;
        }

        if let Some(family) = self.family() {
            write!(f, ",family={family}")?;
        }

        Ok(())
    }
}

/// A `tcp:` address family.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TcpTransportFamily {
    Ipv4,
    Ipv6,
}

impl FromStr for TcpTransportFamily {
    type Err = Error;

    fn from_str(family: &str) -> Result<Self> {
        match family {
            "ipv4" => Ok(Self::Ipv4),
            "ipv6" => Ok(Self::Ipv6),
            _ => Err(Error::Address(format!(
                "invalid tcp address `family`: {family}"
            ))),
        }
    }
}

impl Display for TcpTransportFamily {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ipv4 => write!(f, "ipv4"),
            Self::Ipv6 => write!(f, "ipv6"),
        }
    }
}

#[cfg(all(test, windows, feature = "tokio"))]
mod tests {
    use std::{net::TcpListener, str::FromStr};

    use ntest::timeout;

    use super::Tcp;
    use crate::{Address, runtime::Runtime};

    /// A Tokio connection reaches a `tcp:` address from a thread that knows nothing of Tokio.
    ///
    /// Tokio owns the socket behind this transport on Windows, and a socket of Tokio's belongs to
    /// whichever runtime is current where it is created. The one that has to end up with it is
    /// the connection's own, however the future that opens it is driven: a host with a runtime of
    /// its own polls a connection from outside Tokio entirely.
    #[test]
    #[timeout(15000)]
    fn a_connect_reaches_the_connections_own_runtime() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        let runtime = {
            let _guard = tokio_runtime.enter();

            Runtime::default_for_build().unwrap()
        };
        assert!(
            matches!(runtime, Runtime::Tokio(_)),
            "the runtime taken from inside Tokio was not Tokio's",
        );
        let address = Address::from_str(&format!("tcp:host=127.0.0.1,port={port}")).unwrap();

        // Nothing on this thread points at the runtime above, so a connect that went to whatever
        // is current would find nothing to register the socket on.
        let _split =
            futures_lite::future::block_on(Tcp::new("127.0.0.1", port).connect(&address, &runtime))
                .unwrap();

        let (_accepted, peer) = listener.accept().unwrap();
        assert_eq!(peer.ip(), std::net::Ipv4Addr::LOCALHOST);
    }
}
