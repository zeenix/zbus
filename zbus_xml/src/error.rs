use std::{convert::Infallible, error, fmt, io, sync::Arc};

/// The error type for `zbus_xml`.
///
/// The various errors that can be reported by this crate.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Error {
    /// A zbus error, e.g. an invalid signature or an invalid D-Bus name.
    Zbus(zbus::Error),
    /// An XML parsing error.
    Xml(XmlError),
    /// An I/O error.
    Io(Arc<io::Error>),
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Zbus(s), Self::Zbus(o)) => s == o,
            (Self::Xml(s), Self::Xml(o)) => s == o,
            (_, _) => false,
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Error::Zbus(e) => Some(e),
            Error::Xml(e) => Some(e),
            Error::Io(e) => Some(e),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Zbus(e) => write!(f, "{e}"),
            Error::Xml(e) => write!(f, "XML error: {e}"),
            Error::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl From<zbus::Error> for Error {
    fn from(val: zbus::Error) -> Self {
        Error::Zbus(val)
    }
}

impl From<XmlError> for Error {
    fn from(val: XmlError) -> Self {
        Error::Xml(val)
    }
}

impl From<io::Error> for Error {
    fn from(val: io::Error) -> Self {
        Error::Io(Arc::new(val))
    }
}

impl From<Infallible> for Error {
    fn from(i: Infallible) -> Self {
        match i {}
    }
}

/// An error encountered while parsing an XML document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlError {
    message: String,
    position: usize,
}

impl XmlError {
    pub(crate) fn new(message: impl Into<String>, position: usize) -> Self {
        Self {
            message: message.into(),
            position,
        }
    }

    /// A message describing the error.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The byte offset in the document at which the error was encountered.
    pub fn position(&self) -> usize {
        self.position
    }
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte offset {})", self.message, self.position)
    }
}

impl error::Error for XmlError {}

/// Alias for a `Result` with the error type `zbus_xml::Error`.
pub type Result<T> = std::result::Result<T, Error>;
