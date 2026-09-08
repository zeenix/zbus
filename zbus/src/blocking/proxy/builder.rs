use crate::{
    Error, ObjectPath, Result,
    blocking::Connection,
    names::{BusName, InterfaceName},
    proxy::CacheProperties,
    utils::block_on,
};

pub use crate::proxy::Defaults;

/// Builder for proxies.
///
/// Each setter takes the value it is given, whether that's an already parsed name or a string
/// that still needs parsing, and converts it right away, but it never fails: the first error a
/// setter hits is recorded — a later call doesn't clear it — and reported by [`Builder::build`],
/// so a chain of setters never needs a `?` in between.
#[derive(Debug, Clone)]
pub struct Builder<'a, T = ()>(crate::proxy::Builder<'a, T>);

impl<'a, T> Builder<'a, T> {
    /// Set the proxy destination address.
    ///
    /// An invalid destination is reported by [`Builder::build`].
    #[must_use]
    pub fn destination<D>(self, destination: D) -> Self
    where
        D: TryInto<BusName<'a>>,
        D::Error: Into<Error>,
    {
        Self(crate::proxy::Builder::destination(self.0, destination))
    }

    /// Set the proxy path.
    ///
    /// An invalid path is reported by [`Builder::build`].
    #[must_use]
    pub fn path<P>(self, path: P) -> Self
    where
        P: TryInto<ObjectPath<'a>>,
        P::Error: Into<Error>,
    {
        Self(crate::proxy::Builder::path(self.0, path))
    }

    /// Set the proxy interface.
    ///
    /// An invalid interface name is reported by [`Builder::build`].
    #[must_use]
    pub fn interface<I>(self, interface: I) -> Self
    where
        I: TryInto<InterfaceName<'a>>,
        I::Error: Into<Error>,
    {
        Self(crate::proxy::Builder::interface(self.0, interface))
    }

    /// Set whether to cache properties.
    #[must_use]
    pub fn cache_properties(self, cache: CacheProperties) -> Self {
        Self(self.0.cache_properties(cache))
    }

    /// Specify a set of properties (by name) which should be excluded from caching.
    #[must_use]
    pub fn uncached_properties(self, properties: &[&'a str]) -> Self {
        Self(self.0.uncached_properties(properties))
    }

    /// Build a proxy from the builder.
    ///
    /// # Errors
    ///
    /// The first error recorded by a setter, then [`Error::MissingParameter`] if the builder is
    /// lacking the necessary parameters to build a proxy.
    pub fn build(self) -> Result<T>
    where
        T: From<crate::Proxy<'a>> + crate::proxy::Defaults,
    {
        block_on(self.0.build())
    }
}

impl<T> Builder<'_, T>
where
    T: Defaults,
{
    /// Create a new [`Builder`] for the given connection.
    #[must_use]
    pub fn new(conn: &Connection) -> Self {
        Self(crate::proxy::Builder::new(&conn.clone().into()))
    }
}
