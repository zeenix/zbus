use std::{collections::HashMap, sync::Arc};

use socket2::{Domain, SockAddr, Type};

use crate::{
    Address, Error, Result,
    connection::socket::BoxedSplit,
    runtime::{
        Runtime,
        io::{RegisteredIo, VsockOps, connect},
    },
};

/// A VSOCK D-Bus address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vsock {
    pub(super) cid: u32,
    pub(super) port: u32,
}

impl Vsock {
    /// Create a new VSOCK address.
    pub fn new(cid: u32, port: u32) -> Self {
        Self { cid, port }
    }

    /// The Client ID.
    pub fn cid(&self) -> u32 {
        self.cid
    }

    /// The port.
    pub fn port(&self) -> u32 {
        self.port
    }

    /// Connects to the VSOCK port this address names.
    pub(super) async fn connect(self, address: &Address, runtime: &Runtime) -> Result<BoxedSplit> {
        let socket_address = SockAddr::vsock(self.cid(), self.port());
        let source = connect(runtime, Domain::VSOCK, Type::STREAM, &socket_address)
            .await
            .map_err(|e| Error::Connection(Arc::new(e), Box::new(address.clone())))?;

        Ok(RegisteredIo::new(runtime, source, VsockOps)?.into())
    }

    pub(super) fn from_options(opts: HashMap<&str, &str>) -> Result<Self> {
        let cid = opts
            .get("cid")
            .ok_or_else(|| Error::Address("VSOCK address is missing cid=".into()))?;
        let cid = cid
            .parse::<u32>()
            .map_err(|e| Error::Address(format!("Failed to parse VSOCK cid `{}`: {}", cid, e)))?;
        let port = opts
            .get("port")
            .ok_or_else(|| Error::Address("VSOCK address is missing port=".into()))?;
        let port = port
            .parse::<u32>()
            .map_err(|e| Error::Address(format!("Failed to parse VSOCK port `{}`: {}", port, e)))?;

        Ok(Self { cid, port })
    }
}

impl std::fmt::Display for Vsock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vsock:cid={},port={}", self.cid, self.port)
    }
}
