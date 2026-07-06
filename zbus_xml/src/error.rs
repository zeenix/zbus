use quick_xml::{de::DeError, se::SeError};
use std::{convert::Infallible, error, fmt};
use zbus_names::Error as NamesError;
use zvariant::Error as VariantError;

/// The error type for `zbus_xml`.
///
/// The various errors that can be reported by this crate.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Error {
    /// A zvariant error, e.g. an invalid signature.
    Variant(VariantError),
    /// A D-Bus name error.
    Name(NamesError),
    /// An XML parsing error.
    Xml(XmlError),
    /// An XML error from quick_xml
    QuickXml(DeError),
    /// An XML serialization error from quick_xml
    QuickXmlSer(SeError),
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Variant(s), Self::Variant(o)) => s == o,
            (Self::Name(s), Self::Name(o)) => s == o,
            (Self::Xml(s), Self::Xml(o)) => s == o,
            (Self::QuickXml(_), Self::QuickXml(_)) => false,
            (Self::QuickXmlSer(_), Self::QuickXmlSer(_)) => false,
            (_, _) => false,
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Error::Variant(e) => Some(e),
            Error::Name(e) => Some(e),
            Error::Xml(e) => Some(e),
            Error::QuickXml(e) => Some(e),
            Error::QuickXmlSer(e) => Some(e),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Variant(e) => write!(f, "{e}"),
            Error::Name(e) => write!(f, "{e}"),
            Error::Xml(e) => write!(f, "XML error: {e}"),
            Error::QuickXml(e) => write!(f, "XML error: {e}"),
            Error::QuickXmlSer(e) => write!(f, "XML serialization error: {e}"),
        }
    }
}

impl From<VariantError> for Error {
    fn from(val: VariantError) -> Self {
        Error::Variant(val)
    }
}

impl From<NamesError> for Error {
    fn from(val: NamesError) -> Self {
        Error::Name(val)
    }
}

impl From<XmlError> for Error {
    fn from(val: XmlError) -> Self {
        Error::Xml(val)
    }
}

impl From<DeError> for Error {
    fn from(val: DeError) -> Self {
        Error::QuickXml(val)
    }
}

impl From<SeError> for Error {
    fn from(val: SeError) -> Self {
        Error::QuickXmlSer(val)
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
