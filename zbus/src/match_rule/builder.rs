use crate::{
    Error, MatchRule, ObjectPath, Result, Str,
    match_rule::PathSpec,
    message::Type,
    names::{BusName, InterfaceName, MemberName, UniqueName},
};

const MAX_ARGS: u8 = 64;

/// Builder for [`MatchRule`].
///
/// This is created by [`MatchRule::builder`]. Each setter takes the value it is given, whether
/// that's an already parsed name or a string that still needs parsing, and converts it right
/// away, but it never fails: the first error a setter hits is recorded — a later call doesn't
/// clear it — and reported by [`Builder::build`], so a chain of setters never needs a `?` in
/// between.
#[derive(Debug)]
#[must_use]
pub struct Builder<'m> {
    rule: MatchRule<'m>,
    error: Option<Error>,
}

impl<'m> Builder<'m> {
    /// Build the `MatchRule`.
    ///
    /// # Errors
    ///
    /// The first error recorded by a setter.
    pub fn build(self) -> Result<MatchRule<'m>> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.rule),
        }
    }

    /// Set the sender.
    ///
    /// An invalid sender is reported by [`Builder::build`].
    pub fn sender<B>(mut self, sender: B) -> Self
    where
        B: TryInto<BusName<'m>>,
        B::Error: Into<Error>,
    {
        match sender.try_into() {
            Ok(sender) => self.rule.sender = Some(sender),
            Err(e) => self.record(e.into()),
        }

        self
    }

    /// Set the message type.
    pub fn msg_type(mut self, msg_type: Type) -> Self {
        self.rule.msg_type = Some(msg_type);

        self
    }

    /// Set the interface.
    ///
    /// An invalid interface name is reported by [`Builder::build`].
    pub fn interface<I>(mut self, interface: I) -> Self
    where
        I: TryInto<InterfaceName<'m>>,
        I::Error: Into<Error>,
    {
        match interface.try_into() {
            Ok(interface) => self.rule.interface = Some(interface),
            Err(e) => self.record(e.into()),
        }

        self
    }

    /// Set the member.
    ///
    /// An invalid member name is reported by [`Builder::build`].
    pub fn member<M>(mut self, member: M) -> Self
    where
        M: TryInto<MemberName<'m>>,
        M::Error: Into<Error>,
    {
        match member.try_into() {
            Ok(member) => self.rule.member = Some(member),
            Err(e) => self.record(e.into()),
        }

        self
    }

    /// Set the path.
    ///
    /// Note: Since both a path and a path namespace are not allowed to appear in a match rule at
    /// the same time, this overrides any path namespace previously set.
    ///
    /// An invalid path is reported by [`Builder::build`].
    pub fn path<P>(mut self, path: P) -> Self
    where
        P: TryInto<ObjectPath<'m>>,
        P::Error: Into<Error>,
    {
        match path.try_into() {
            Ok(path) => self.rule.path_spec = Some(PathSpec::Path(path)),
            Err(e) => self.record(e.into()),
        }

        self
    }

    /// Set the path namespace.
    ///
    /// Note: Since both a path and a path namespace are not allowed to appear in a match rule at
    /// the same time, this overrides any path previously set.
    ///
    /// An invalid path namespace is reported by [`Builder::build`].
    pub fn path_namespace<P>(mut self, path_namespace: P) -> Self
    where
        P: TryInto<ObjectPath<'m>>,
        P::Error: Into<Error>,
    {
        match path_namespace.try_into() {
            Ok(namespace) => self.rule.path_spec = Some(PathSpec::PathNamespace(namespace)),
            Err(e) => self.record(e.into()),
        }

        self
    }

    /// Set the destination.
    ///
    /// An invalid destination is reported by [`Builder::build`].
    pub fn destination<B>(mut self, destination: B) -> Self
    where
        B: TryInto<UniqueName<'m>>,
        B::Error: Into<Error>,
    {
        match destination.try_into() {
            Ok(destination) => self.rule.destination = Some(destination),
            Err(e) => self.record(e.into()),
        }

        self
    }

    /// Append an argument.
    ///
    /// Use this in instead of [`Builder::arg`] if you want to sequentially add args.
    ///
    /// An attempt to add the 65th argument is reported by [`Builder::build`].
    pub fn add_arg<S>(self, arg: S) -> Self
    where
        S: Into<Str<'m>>,
    {
        let idx = self.rule.args.len() as u8;

        self.arg(idx, arg)
    }

    /// Add an argument of a specified index.
    ///
    /// This replaces any argument previously added at `idx`.
    ///
    /// An `idx` of 64 or greater is reported by [`Builder::build`].
    pub fn arg<S>(mut self, idx: u8, arg: S) -> Self
    where
        S: Into<Str<'m>>,
    {
        if idx >= MAX_ARGS {
            self.record(Error::InvalidMatchRule);

            return self;
        }
        insert_indexed(&mut self.rule.args, idx, arg.into());

        self
    }

    /// Append a path argument.
    ///
    /// Use this in instead of [`Builder::arg_path`] if you want to sequentially add args.
    ///
    /// An invalid path, or an attempt to add the 65th path argument, is reported by
    /// [`Builder::build`].
    pub fn add_arg_path<P>(self, arg_path: P) -> Self
    where
        P: TryInto<ObjectPath<'m>>,
        P::Error: Into<Error>,
    {
        let idx = self.rule.arg_paths.len() as u8;

        self.arg_path(idx, arg_path)
    }

    /// Add a path argument of a specified index.
    ///
    /// This replaces any path argument previously added at `idx`.
    ///
    /// An invalid path, or an `idx` of 64 or greater, is reported by [`Builder::build`].
    pub fn arg_path<P>(mut self, idx: u8, arg_path: P) -> Self
    where
        P: TryInto<ObjectPath<'m>>,
        P::Error: Into<Error>,
    {
        if idx >= MAX_ARGS {
            self.record(Error::InvalidMatchRule);

            return self;
        }
        match arg_path.try_into() {
            Ok(arg_path) => insert_indexed(&mut self.rule.arg_paths, idx, arg_path),
            Err(e) => self.record(e.into()),
        }

        self
    }

    /// Set 0th argument's namespace.
    ///
    /// The namespace must be a valid bus name or a valid element of a bus name. For more
    /// information, see [the spec](https://dbus.freedesktop.org/doc/dbus-specification.html#message-protocol-names-bus).
    ///
    /// An invalid namespace is reported by [`Builder::build`].
    ///
    /// # Examples
    ///
    /// ```
    /// # use zbus::MatchRule;
    /// // Valid namespaces
    /// MatchRule::builder().arg0ns("org.mpris.MediaPlayer2").build().unwrap();
    /// MatchRule::builder().arg0ns("org").build().unwrap();
    /// MatchRule::builder().arg0ns(":org").build().unwrap();
    /// MatchRule::builder().arg0ns(":1org").build().unwrap();
    ///
    /// // Invalid namespaces
    /// MatchRule::builder().arg0ns("org.").build().unwrap_err();
    /// MatchRule::builder().arg0ns(".org").build().unwrap_err();
    /// MatchRule::builder().arg0ns("1org").build().unwrap_err();
    /// MatchRule::builder().arg0ns(".").build().unwrap_err();
    /// MatchRule::builder().arg0ns("org..freedesktop").build().unwrap_err();
    /// ````
    pub fn arg0ns<S>(mut self, namespace: S) -> Self
    where
        S: Into<Str<'m>>,
    {
        match validate_arg0ns(namespace.into()) {
            Ok(namespace) => self.rule.arg0ns = Some(namespace),
            Err(e) => self.record(e),
        }

        self
    }

    /// Create a builder for `MatchRule`.
    pub(crate) fn new() -> Self {
        Self {
            rule: MatchRule {
                msg_type: None,
                sender: None,
                interface: None,
                member: None,
                path_spec: None,
                destination: None,
                args: Vec::with_capacity(MAX_ARGS as usize),
                arg_paths: Vec::with_capacity(MAX_ARGS as usize),
                arg0ns: None,
            },
            error: None,
        }
    }

    /// Record `error`, unless an earlier setter already recorded one.
    fn record(&mut self, error: Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }
}

/// Add `value` at `idx`, replacing the value already recorded at that index, if any.
fn insert_indexed<T>(entries: &mut Vec<(u8, T)>, idx: u8, value: T) {
    match entries.binary_search_by(|(i, _)| i.cmp(&idx)) {
        Ok(i) => entries[i] = (idx, value),
        Err(i) => entries.insert(i, (idx, value)),
    }
}

/// Check that `namespace` is a valid bus name or a valid element of a bus name.
pub(crate) fn validate_arg0ns(namespace: Str<'_>) -> Result<Str<'_>> {
    // Rules: https://dbus.freedesktop.org/doc/dbus-specification.html#message-protocol-names-bus
    // minus the requirement to have more than one element.

    if namespace.is_empty() || namespace.len() > 255 {
        return Err(Error::InvalidMatchRule);
    }

    let (is_unique, namespace_str) = match namespace.strip_prefix(':') {
        Some(s) => (true, s),
        None => (false, namespace.as_str()),
    };

    let valid_first_char = |s: &str| match s.chars().next() {
        None | Some('.') => false,
        Some('0'..='9') if !is_unique => false,
        _ => true,
    };

    if !valid_first_char(namespace_str)
        || !namespace_str.split('.').all(valid_first_char)
        || !namespace_str
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(Error::InvalidMatchRule);
    }

    Ok(namespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::OwnedBusName;
    use test_log::test;

    #[test]
    fn strings_are_converted() {
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender("org.freedesktop.DBus")
            .interface("org.freedesktop.DBus.Properties")
            .member("PropertiesChanged")
            .path("/org/zbus")
            .destination(":1.11")
            .add_arg("org.zbus")
            .add_arg_path("/org/zbus/Arg")
            .arg0ns("org.zbus")
            .build()
            .unwrap();

        assert_eq!(
            rule.to_string(),
            "type='signal',\
             sender='org.freedesktop.DBus',\
             interface='org.freedesktop.DBus.Properties',\
             member='PropertiesChanged',\
             destination=':1.11',\
             path='/org/zbus',\
             arg0='org.zbus',\
             arg0path='/org/zbus/Arg',\
             arg0namespace='org.zbus'",
        );
    }

    #[test]
    fn invalid_string_is_reported_by_build() {
        let err = MatchRule::builder()
            .sender("not a bus name")
            .member("Whatever")
            .build()
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            BusName::try_from("not a bus name").unwrap_err().to_string(),
        );
    }

    #[test]
    fn typed_values_need_no_error_handling() {
        let sender = BusName::try_from("org.zbus.Sender").unwrap();
        let path = ObjectPath::try_from("/org/zbus").unwrap();
        let rule = MatchRule::builder()
            .sender(&sender)
            .path(path.clone())
            .build()
            .unwrap();

        assert_eq!(rule.sender(), Some(&sender));
        assert_eq!(rule.path_spec(), Some(&PathSpec::Path(path)));

        let rule = MatchRule::builder()
            .sender(OwnedBusName::from(sender.clone()))
            .build()
            .unwrap();

        assert_eq!(rule.sender(), Some(&sender));
    }

    #[test]
    fn a_later_valid_call_keeps_the_first_error() {
        // Setting the field again, even to a valid value, doesn't clear the error it recorded.
        let err = MatchRule::builder()
            .path("bad")
            .path("/good")
            .build()
            .unwrap_err();

        assert_eq!(err, ObjectPath::try_from("bad").unwrap_err());

        let mut builder = MatchRule::builder();
        for i in 0..MAX_ARGS {
            builder = builder.add_arg(i.to_string());
        }

        // The 65th argument doesn't fit and the later valid argument doesn't undo that.
        let err = builder
            .add_arg("one too many")
            .arg(0, "replaced")
            .build()
            .unwrap_err();

        assert_eq!(err, Error::InvalidMatchRule);
    }

    #[test]
    fn rule_string_rejects_an_invalid_namespace_even_if_repeated() {
        // The parser validates each component itself, so a later valid `arg0namespace` must not
        // hide an earlier invalid one.
        let err = MatchRule::try_from("arg0namespace='org.',arg0namespace='org'").unwrap_err();

        assert_eq!(err, Error::InvalidMatchRule);
        MatchRule::try_from("arg0namespace='org'").unwrap();
    }
}
