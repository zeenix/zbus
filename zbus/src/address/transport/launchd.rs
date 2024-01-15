use super::{Transport, Unix, UnixPath};
use crate::{process::run, Result};
use std::collections::HashMap;
use zvariant::Str;

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
/// The transport properties of a launchd D-Bus address.
pub struct Launchd<'l> {
    pub(super) env: Str<'l>,
}

impl<'l> Launchd<'l> {
    /// Create a new launchd D-Bus address.
    pub fn new(env: impl Into<Str<'a>>) -> Self {
        Self { env: env.into() }
    }

    /// The path of the unix domain socket for the launchd created dbus-daemon.
    pub fn env(&self) -> &str {
        self.env.as_str()
    }

    /// Determine the actual transport details behin a launchd address.
    pub(super) async fn bus_address(&self) -> Result<Transport<'l>> {
        let output = run("launchctl", ["getenv", self.env()])
            .await
            .expect("failed to wait on launchctl output");

        if !output.status.success() {
            return Err(crate::Error::Address(format!(
                "launchctl terminated with code: {}",
                output.status
            )));
        }

        let addr = std::str::from_utf8(&output.stdout).map_err(|e| {
            crate::Error::Address(format!("Unable to parse launchctl output as UTF-8: {}", e))
        })?;

        Ok(Transport::Unix(Unix::new(UnixPath::File(
            addr.trim().into(),
        ))))
    }

    pub(super) fn from_options(opts: HashMap<&str, &'l str>) -> Result<Self> {
        opts.get("env")
            .ok_or_else(|| crate::Error::Address("missing env key".into()))
            .map(|env| Self { env: env.into() })
    }
}
