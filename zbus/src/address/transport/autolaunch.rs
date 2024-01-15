use crate::{Error, Result};
use std::collections::HashMap;
use zvariant::Str;

/// Transport properties of an autolaunch D-Bus address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Autolaunch<'a> {
    pub(super) scope: Option<Str<'a>>,
}

impl<'a> Autolaunch<'a> {
    /// Create a new autolaunch transport.
    pub fn new() -> Self {
        Self { scope: None }
    }

    /// Set the `autolaunch:` address `scope` value.
    pub fn set_scope<S>(mut self, scope: Option<S>) -> Self
    where
        S: Into<Str<'a>>,
    {
        self.scope = scope.map(|s| s.into());

        self
    }

    /// The optional scope.
    pub fn scope(&self) -> Option<&str> {
        self.scope.as_deref()
    }

    pub(super) fn from_options(opts: HashMap<&str, &str>) -> Result<Self> {
        opts.get("scope")
            .map(|scope| -> Result<_> {
                std::str::from_utf8(&super::decode_percents(scope)?)
                    .map_err(|_| Error::Address("autolaunch scope is not valid UTF-8".to_owned()))
            })
            .transpose()
            .map(|scope| Self {
                scope: scope.map(Into::into),
            })
    }
}

impl<'a> ToOwned for Autolaunch<'a> {
    type Owned = Autolaunch<'static>;

    fn to_owned(&self) -> Self::Owned {
        Self {
            scope: self.scope.to_owned(),
        }
    }
}
