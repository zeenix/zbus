use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;

/// A Unix domain socket transport in a D-Bus address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unix<'u> {
    path: UnixPath<'u>,
}

impl<'u> Unix<'u> {
    /// Create a new Unix transport with the given path.
    pub fn new(path: UnixPath<'u>) -> Self {
        Self { path }
    }

    /// The path.
    pub fn path(&self) -> &UnixPath<'_> {
        &self.path
    }

    /// Take the path, consuming `self`.
    pub fn take_path(self) -> UnixPath<'u> {
        self.path
    }

    #[cfg(any(unix, not(feature = "tokio")))]
    pub(super) fn from_options(opts: HashMap<&str, &'u str>) -> crate::Result<Self> {
        let path = opts.get("path");
        let abs = opts.get("abstract");
        let dir = opts.get("dir");
        let tmpdir = opts.get("tmpdir");
        let path = match (path, abs, dir, tmpdir) {
            (Some(p), None, None, None) => UnixPath::File(OsStr::new(p).into()),
            #[cfg(target_os = "linux")]
            (None, Some(p), None, None) => UnixPath::Abstract(p.as_bytes().into()),
            #[cfg(not(target_os = "linux"))]
            (None, Some(_), None, None) => {
                return Err(crate::Error::Address(
                    "abstract sockets currently Linux-only".to_owned(),
                ));
            }
            (None, None, Some(p), None) => UnixPath::Dir(OsStr::new(p).into()),
            (None, None, None, Some(p)) => UnixPath::TmpDir(OsStr::new(p).into()),
            _ => {
                return Err(crate::Error::Address("unix: address is invalid".to_owned()));
            }
        };

        Ok(Self::new(path))
    }
}

/// A Unix domain socket path in a D-Bus address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnixPath<'p> {
    /// A path to a unix domain socket on the filesystem.
    File(Cow<'p, OsStr>),
    /// A abstract unix domain socket name.
    #[cfg(target_os = "linux")]
    Abstract(Cow<'p, [u8]>),
    /// A listenable address using the specified path, in which a socket file with a random file
    /// name starting with 'dbus-' will be created by the server. See [UNIX domain socket address]
    /// reference documentation.
    ///
    /// This address is mostly relevant to server (typically bus broker) implementations.
    ///
    /// [UNIX domain socket address]: https://dbus.freedesktop.org/doc/dbus-specification.html#transports-unix-domain-sockets-addresses
    Dir(Cow<'p, OsStr>),
    /// The same as UnixDir, except that on platforms with abstract sockets, the server may attempt
    /// to create an abstract socket whose name starts with this directory instead of a path-based
    /// socket.
    ///
    /// This address is mostly relevant to server (typically bus broker) implementations.
    TmpDir(Cow<'p, OsStr>),
}
