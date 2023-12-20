use crate::{Error, Result};
use std::collections::HashMap;

/// Transport properties of an autolaunch D-Bus address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Autolaunch {
    pub(super) scope: Option<String>,
}

impl Autolaunch {
    /// Create a new autolaunch transport.
    pub fn new() -> Self {
        Self { scope: None }
    }

    /// Set the `autolaunch:` address `scope` value.
    pub fn set_scope(mut self, scope: Option<&str>) -> Self {
        self.scope = scope.map(|s| s.to_owned());

        self
    }

    /// The optional scope.
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    pub(super) fn from_options(opts: HashMap<&str, &str>) -> Result<Self> {
        opts.get("scope")
            .map(|scope| -> Result<_> {
                String::from_utf8(super::decode_percents(scope)?)
                    .map_err(|_| Error::Address("autolaunch scope is not valid UTF-8".to_owned()))
            })
            .transpose()
            .map(|scope| Self { scope })
    }
}
