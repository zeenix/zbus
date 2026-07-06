//! A parser for D-Bus introspection XML, and a matching attribute-value escaping helper.
//!
//! Introspection XML only uses elements and attributes, so rather than reaching for a
//! general-purpose XML library this is a small [`winnow`] parser that recognises exactly the
//! D-Bus introspection grammar — `<node>`, `<interface>`, `<method>`, `<signal>`, `<property>`,
//! `<arg>` and `<annotation>` — and builds the [`Node`] tree directly, one combinator per
//! element deciding which attributes are required, which are optional, and which child elements
//! are expected. Everything else a real document may carry (the XML declaration, a doctype,
//! comments, processing instructions, CDATA and foreign elements such as Telepathy's
//! `<tp:docstring>`) is recognised only well enough to skip over it.

use std::{borrow::Cow, collections::HashSet};

use winnow::{
    LocatingSlice, ModalResult, Parser,
    combinator::{alt, cut_err, delimited, eof, opt, preceded, repeat},
    error::{ErrMode, ParserError},
    stream::Location,
    token::{any, take_till, take_until, take_while},
};
use zbus_names::{InterfaceName, MemberName, PropertyName};

use crate::{
    Annotation, Arg, ArgDirection, Interface, Method, Node, Property, PropertyAccess, Signal,
    Signature,
    error::{Error, Result, XmlError},
};

/// The maximum element nesting depth accepted when parsing.
///
/// Far deeper than any real introspection tree (four levels, plus one per nested `<node>`). The
/// parser iterates rather than recurses over the two axes where a document can nest arbitrarily
/// deep — nested `<node>`s and skipped-over foreign elements — so this cap only bounds the memory
/// spent tracking the open elements, not the call stack.
const MAX_DEPTH: usize = 1024;

/// Parse a D-Bus introspection document into its [`Node`] tree.
pub(crate) fn parse<'a>(document: &str) -> Result<Node<'a>> {
    let mut input = LocatingSlice::new(document);
    match document_node(&mut input) {
        // The tree owns its data, so it outlives `document` and satisfies any caller lifetime.
        Ok(node) => Ok(node),
        Err(ErrMode::Backtrack(error) | ErrMode::Cut(error)) => Err(error.into_error()),
        Err(ErrMode::Incomplete(_)) => Err(Error::Xml(XmlError::new(
            "unexpected end of document",
            document.len(),
        ))),
    }
}

/// The document's root element, preceded by an optional prolog (declaration, doctype, comments).
///
/// The root element's name is not checked, for compatibility with servers that don't name it
/// `node` and with how previous (quick-xml-based) versions of this crate behaved.
fn document_node<'i>(input: &mut Input<'i>) -> PResult<Node<'static>> {
    ignorable(input)?;
    if opt(eof).parse_next(input)?.is_some() {
        return Err(error("missing root element", input));
    }
    let (tag, attrs, self_closing) = start_element(input)?;

    node(input, tag, attrs, self_closing)
}

/// The input stream: byte offsets come from [`LocatingSlice`], for error reporting.
type Input<'i> = LocatingSlice<&'i str>;

/// A parser result, using [`ParseError`] as winnow's error type.
type PResult<O> = ModalResult<O, ParseError>;

/// A parse failure: either a positioned structural/XML error, or a rejected value (an invalid
/// name or signature) that carries the matching [`Error`] variant.
#[derive(Debug)]
enum ParseError {
    Xml {
        message: Cow<'static, str>,
        offset: usize,
    },
    Domain(Error),
}

impl ParseError {
    /// A fatal XML error at `offset`.
    fn xml(message: impl Into<Cow<'static, str>>, offset: usize) -> ErrMode<Self> {
        ErrMode::Cut(ParseError::Xml {
            message: message.into(),
            offset,
        })
    }

    /// A fatal error from rejecting a value (an invalid name or signature).
    fn domain(error: Error) -> ErrMode<Self> {
        ErrMode::Cut(ParseError::Domain(error))
    }

    fn into_error(self) -> Error {
        match self {
            ParseError::Xml { message, offset } => Error::Xml(XmlError::new(message, offset)),
            ParseError::Domain(error) => error,
        }
    }
}

impl<'i> ParserError<Input<'i>> for ParseError {
    type Inner = Self;

    fn from_input(input: &Input<'i>) -> Self {
        ParseError::Xml {
            message: Cow::Borrowed("malformed markup"),
            offset: input.current_token_start(),
        }
    }

    fn into_inner(self) -> std::result::Result<Self::Inner, Self> {
        Ok(self)
    }
}

/// A fatal XML error at the input's current position.
fn error(message: impl Into<Cow<'static, str>>, input: &Input<'_>) -> ErrMode<ParseError> {
    ParseError::xml(message, input.current_token_start())
}

/// A `<node>` and its subtree.
///
/// Nested `<node>`s — the one axis on which an introspection tree itself can nest arbitrarily
/// deep — are walked iteratively on an explicit stack, so that a deeply nested document grows
/// this `Vec` rather than the call stack. Interfaces (and their fixed, shallow subtrees) recurse
/// normally.
fn node<'i>(
    input: &mut Input<'i>,
    tag: &'i str,
    attrs: Attrs<'i>,
    self_closing: bool,
) -> PResult<Node<'static>> {
    let root = empty_node(&attrs);
    if self_closing {
        return Ok(root);
    }

    let mut open: Vec<(&'i str, Node<'static>)> = vec![(tag, root)];
    loop {
        ignorable(input)?;
        let tag = open.last().expect("the root is popped only by returning").0;
        if opt(eof).parse_next(input)?.is_some() {
            return Err(error(format!("missing `</{tag}>`"), input));
        }
        if let Some(close) = opt(closing_tag).parse_next(input)? {
            if close != tag {
                return Err(error(
                    format!("unexpected `</{close}>` while parsing `<{tag}>`"),
                    input,
                ));
            }
            let (_, finished) = open.pop().expect("a close tag was just matched");
            match open.last_mut() {
                Some((_, parent)) => parent.nodes.push(finished),
                None => return Ok(finished),
            }
            continue;
        }

        let (child, child_attrs, child_self_closing) = start_element(input)?;
        match child {
            "interface" => {
                let interface = interface(input, child, child_attrs, child_self_closing)?;
                open.last_mut()
                    .expect("non-empty")
                    .1
                    .interfaces
                    .push(interface);
            }
            "node" if child_self_closing => {
                open.last_mut()
                    .expect("non-empty")
                    .1
                    .nodes
                    .push(empty_node(&child_attrs));
            }
            "node" => {
                if open.len() >= MAX_DEPTH {
                    return Err(error("maximum element nesting depth exceeded", input));
                }
                open.push((child, empty_node(&child_attrs)));
            }
            _ => skip_element(input, child, child_self_closing)?,
        }
    }
}

/// A `<node>` with only its `name`, ready to be filled in.
fn empty_node(attrs: &Attrs<'_>) -> Node<'static> {
    Node {
        name: attrs.optional("name").map(str::to_owned),
        interfaces: Vec::new(),
        nodes: Vec::new(),
    }
}

/// An `<interface>` and its members.
fn interface<'i>(
    input: &mut Input<'i>,
    tag: &'i str,
    attrs: Attrs<'i>,
    self_closing: bool,
) -> PResult<Interface<'static>> {
    let name = attrs.name(|n| InterfaceName::try_from(n).map_err(Error::Name))?;
    let mut methods = Vec::new();
    let mut properties = Vec::new();
    let mut signals = Vec::new();
    let mut annotations = Vec::new();
    children(
        input,
        tag,
        self_closing,
        |input, child, attrs, sc| match child {
            "method" => {
                methods.push(method(input, child, attrs, sc)?);
                Ok(true)
            }
            "property" => {
                properties.push(property(input, child, attrs, sc)?);
                Ok(true)
            }
            "signal" => {
                signals.push(signal(input, child, attrs, sc)?);
                Ok(true)
            }
            "annotation" => {
                annotations.push(annotation(input, child, attrs, sc)?);
                Ok(true)
            }
            _ => Ok(false),
        },
    )?;

    Ok(Interface {
        name,
        methods,
        properties,
        signals,
        annotations,
    })
}

/// A `<method>` with its arguments and annotations.
fn method<'i>(
    input: &mut Input<'i>,
    tag: &'i str,
    attrs: Attrs<'i>,
    self_closing: bool,
) -> PResult<Method<'static>> {
    let name = attrs.name(|n| MemberName::try_from(n).map_err(Error::Name))?;
    let mut args = Vec::new();
    let mut annotations = Vec::new();
    children(
        input,
        tag,
        self_closing,
        |input, child, attrs, sc| match child {
            "arg" => {
                args.push(arg(input, child, attrs, sc)?);
                Ok(true)
            }
            "annotation" => {
                annotations.push(annotation(input, child, attrs, sc)?);
                Ok(true)
            }
            _ => Ok(false),
        },
    )?;

    Ok(Method {
        name,
        args,
        annotations,
    })
}

/// A `<signal>` with its arguments and annotations.
fn signal<'i>(
    input: &mut Input<'i>,
    tag: &'i str,
    attrs: Attrs<'i>,
    self_closing: bool,
) -> PResult<Signal<'static>> {
    let name = attrs.name(|n| MemberName::try_from(n).map_err(Error::Name))?;
    let mut args = Vec::new();
    let mut annotations = Vec::new();
    children(
        input,
        tag,
        self_closing,
        |input, child, attrs, sc| match child {
            "arg" => {
                args.push(arg(input, child, attrs, sc)?);
                Ok(true)
            }
            "annotation" => {
                annotations.push(annotation(input, child, attrs, sc)?);
                Ok(true)
            }
            _ => Ok(false),
        },
    )?;

    Ok(Signal {
        name,
        args,
        annotations,
    })
}

/// A `<property>`: a `name`, a `type` signature, an `access` mode and any annotations.
fn property<'i>(
    input: &mut Input<'i>,
    tag: &'i str,
    attrs: Attrs<'i>,
    self_closing: bool,
) -> PResult<Property<'static>> {
    let name = attrs.name(|n| PropertyName::try_from(n).map_err(Error::Name))?;
    let ty = attrs.signature()?;
    let access = match attrs.required("access")? {
        "read" => PropertyAccess::Read,
        "write" => PropertyAccess::Write,
        "readwrite" => PropertyAccess::ReadWrite,
        other => return Err(error(format!("invalid property access `{other}`"), input)),
    };
    let mut annotations = Vec::new();
    children(
        input,
        tag,
        self_closing,
        |input, child, attrs, sc| match child {
            "annotation" => {
                annotations.push(annotation(input, child, attrs, sc)?);
                Ok(true)
            }
            _ => Ok(false),
        },
    )?;

    Ok(Property {
        name,
        ty,
        access,
        annotations,
    })
}

/// An `<arg>`: an optional `name`, a `type` signature, an optional `direction` and annotations.
fn arg<'i>(
    input: &mut Input<'i>,
    tag: &'i str,
    attrs: Attrs<'i>,
    self_closing: bool,
) -> PResult<Arg> {
    let name = attrs.optional("name").map(str::to_owned);
    let ty = attrs.signature()?;
    let direction = match attrs.optional("direction") {
        Some("in") => Some(ArgDirection::In),
        Some("out") => Some(ArgDirection::Out),
        Some(other) => {
            return Err(error(
                format!("invalid argument direction `{other}`"),
                input,
            ));
        }
        None => None,
    };
    let mut annotations = Vec::new();
    children(
        input,
        tag,
        self_closing,
        |input, child, attrs, sc| match child {
            "annotation" => {
                annotations.push(annotation(input, child, attrs, sc)?);
                Ok(true)
            }
            _ => Ok(false),
        },
    )?;

    Ok(Arg {
        name,
        ty,
        direction,
        annotations,
    })
}

/// An `<annotation>`: a `name`/`value` pair. Its content, if any, is ignored.
fn annotation<'i>(
    input: &mut Input<'i>,
    tag: &'i str,
    attrs: Attrs<'i>,
    self_closing: bool,
) -> PResult<Annotation> {
    let name = attrs.required("name")?.to_owned();
    let value = attrs.required("value")?.to_owned();
    children(input, tag, self_closing, |_, _, _, _| Ok(false))?;

    Ok(Annotation { name, value })
}

/// Dispatch each child element of the just-opened `<tag>` to `handle`, skipping over content it
/// does not claim, until the matching `</tag>`.
///
/// `handle` returns whether it consumed the element's body; elements it declines (returns
/// `Ok(false)` for) are skipped along with their subtree.
fn children<'i>(
    input: &mut Input<'i>,
    tag: &'i str,
    self_closing: bool,
    mut handle: impl FnMut(&mut Input<'i>, &'i str, Attrs<'i>, bool) -> PResult<bool>,
) -> PResult<()> {
    if self_closing {
        return Ok(());
    }
    loop {
        ignorable(input)?;
        if opt(eof).parse_next(input)?.is_some() {
            return Err(error(format!("missing `</{tag}>`"), input));
        }
        if let Some(close) = opt(closing_tag).parse_next(input)? {
            if close != tag {
                return Err(error(
                    format!("unexpected `</{close}>` while parsing `<{tag}>`"),
                    input,
                ));
            }
            return Ok(());
        }
        let (child, attrs, self_closing) = start_element(input)?;
        if !handle(input, child, attrs, self_closing)? {
            skip_element(input, child, self_closing)?;
        }
    }
}

/// Skip an element whose start tag has been read, along with its whole subtree.
///
/// Iterative (tracking the open elements on a `Vec`) so that a deeply nested foreign element
/// cannot exhaust the call stack.
fn skip_element<'i>(input: &mut Input<'i>, tag: &'i str, self_closing: bool) -> PResult<()> {
    if self_closing {
        return Ok(());
    }
    let mut open = vec![tag];
    while let Some(&expected) = open.last() {
        ignorable(input)?;
        if opt(eof).parse_next(input)?.is_some() {
            return Err(error(format!("missing `</{expected}>`"), input));
        }
        if let Some(close) = opt(closing_tag).parse_next(input)? {
            if close != expected {
                return Err(error(
                    format!("unexpected `</{close}>` while parsing `<{expected}>`"),
                    input,
                ));
            }
            open.pop();
            continue;
        }
        let (child, _, self_closing) = start_element(input)?;
        if !self_closing {
            if open.len() >= MAX_DEPTH {
                return Err(error("maximum element nesting depth exceeded", input));
            }
            open.push(child);
        }
    }

    Ok(())
}

/// Consume any content that carries no introspection data: whitespace and text, comments, CDATA
/// sections, processing instructions and markup declarations. Stops at the next element boundary.
fn ignorable<'i>(input: &mut Input<'i>) -> PResult<()> {
    repeat(
        0..,
        alt((
            comment,
            cdata,
            processing_instruction,
            markup_declaration,
            text,
        )),
    )
    .parse_next(input)
}

/// An XML comment: `<!-- … -->`.
fn comment<'i>(input: &mut Input<'i>) -> PResult<()> {
    ("<!--", cut_err((take_until(0.., "-->"), "-->")))
        .void()
        .parse_next(input)
}

/// A CDATA section: `<![CDATA[ … ]]>`.
fn cdata<'i>(input: &mut Input<'i>) -> PResult<()> {
    ("<![CDATA[", cut_err((take_until(0.., "]]>"), "]]>")))
        .void()
        .parse_next(input)
}

/// A processing instruction, e. g. the `<?xml … ?>` declaration.
fn processing_instruction<'i>(input: &mut Input<'i>) -> PResult<()> {
    ("<?", cut_err((take_until(0.., "?>"), "?>")))
        .void()
        .parse_next(input)
}

/// A markup declaration such as `<!DOCTYPE …>`.
///
/// Tried after [`comment`] and [`cdata`], which also open with `<!`.
fn markup_declaration<'i>(input: &mut Input<'i>) -> PResult<()> {
    preceded("<!", cut_err(markup_body)).parse_next(input)
}

/// The body of a markup declaration, up to and including the terminating `>`.
///
/// A `>` inside a quoted literal or an internal subset (`[ … ]`, which may itself contain `>`s)
/// does not terminate the declaration.
fn markup_body<'i>(input: &mut Input<'i>) -> PResult<()> {
    let mut bracket_depth = 0usize;
    loop {
        match any.parse_next(input)? {
            quote @ ('"' | '\'') => {
                (take_until(0.., quote), any).void().parse_next(input)?;
            }
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            '>' if bracket_depth == 0 => return Ok(()),
            _ => (),
        }
    }
}

/// A run of character data between tags (carries no introspection data, so ignored).
fn text<'i>(input: &mut Input<'i>) -> PResult<()> {
    take_till(1.., '<').void().parse_next(input)
}

/// A closing tag such as `</node>`, yielding the element name.
fn closing_tag<'i>(input: &mut Input<'i>) -> PResult<&'i str> {
    delimited("</", xml_name, (whitespace, '>')).parse_next(input)
}

/// A start tag such as `<arg name="foo" type="s">`, or the self-closing `<arg … />`, yielding
/// the element name, its [`Attrs`] and whether it is self-closing.
fn start_element<'i>(input: &mut Input<'i>) -> PResult<(&'i str, Attrs<'i>, bool)> {
    let (name, span) = preceded('<', xml_name.with_span()).parse_next(input)?;
    let pairs = attributes(input)?;
    let self_closing =
        preceded(whitespace, alt(("/>".value(true), ">".value(false)))).parse_next(input)?;

    Ok((
        name,
        Attrs {
            element: name,
            offset: span.start,
            pairs,
        },
        self_closing,
    ))
}

/// The attributes of an element: names paired with their unescaped values, in document order.
struct Attrs<'i> {
    /// The element name, for error messages.
    element: &'i str,
    /// The byte offset of the element name, for missing-attribute errors.
    offset: usize,
    pairs: Vec<(&'i str, Cow<'i, str>)>,
}

impl<'i> Attrs<'i> {
    /// The value of `key`, if present.
    fn optional(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.as_ref())
    }

    /// The value of `key`, or a "missing attribute" error anchored at the element.
    fn required(&self, key: &str) -> PResult<&str> {
        self.optional(key).ok_or_else(|| {
            ParseError::xml(
                format!("missing attribute `{key}` on `<{}>`", self.element),
                self.offset,
            )
        })
    }

    /// The required `name` attribute, validated by `parse` (e. g. into an [`InterfaceName`]).
    fn name<T>(&self, parse: impl FnOnce(String) -> std::result::Result<T, Error>) -> PResult<T> {
        parse(self.required("name")?.to_owned()).map_err(ParseError::domain)
    }

    /// The required `type` attribute, parsed as a signature.
    fn signature(&self) -> PResult<Signature> {
        zvariant::Signature::try_from(self.required("type")?.as_bytes())
            .map(Signature)
            .map_err(|e| ParseError::domain(zvariant::Error::from(e).into()))
    }
}

/// The attributes of an element, unescaped and with duplicates rejected.
fn attributes<'i>(input: &mut Input<'i>) -> PResult<Vec<(&'i str, Cow<'i, str>)>> {
    let raw: Vec<Attribute<'i>> = repeat(0.., attribute).parse_next(input)?;
    let mut pairs: Vec<(&'i str, Cow<'i, str>)> = Vec::with_capacity(raw.len());
    // Real elements carry only a few attributes, for which a linear duplicate scan is cheapest;
    // but that scan is O(n²), so once a (only ever hostile) tag grows past a threshold, track the
    // names in a set to keep the work linear.
    let mut seen: Option<HashSet<&'i str>> = None;
    for attr in raw {
        let duplicate = match seen {
            Some(ref mut seen) => !seen.insert(attr.name),
            None => {
                let duplicate = pairs.iter().any(|(name, _)| *name == attr.name);
                if !duplicate && pairs.len() >= 32 {
                    let mut set: HashSet<&'i str> = pairs.iter().map(|(name, _)| *name).collect();
                    set.insert(attr.name);
                    seen = Some(set);
                }
                duplicate
            }
        };
        if duplicate {
            return Err(ParseError::xml(
                format!("duplicate attribute `{}`", attr.name),
                attr.name_offset,
            ));
        }
        let value = unescape(attr.value)
            .map_err(|(message, at)| ParseError::xml(message, attr.value_offset + at))?;
        pairs.push((attr.name, value));
    }

    Ok(pairs)
}

/// A single attribute with the byte offsets needed to anchor errors precisely.
struct Attribute<'i> {
    name: &'i str,
    name_offset: usize,
    /// The raw (still escaped) value.
    value: &'i str,
    value_offset: usize,
}

/// A single attribute, e. g. `name="foo"`, along with the whitespace XML requires before it.
///
/// The mandatory leading whitespace is what rejects run-together attributes like
/// `name="a"type="s"`.
fn attribute<'i>(input: &mut Input<'i>) -> PResult<Attribute<'i>> {
    let (name, name_span) = preceded(whitespace1, xml_name.with_span()).parse_next(input)?;
    (whitespace, '=', whitespace).parse_next(input)?;
    let (value, value_offset) = quoted_value(input)?;

    Ok(Attribute {
        name,
        name_offset: name_span.start,
        value,
        value_offset,
    })
}

/// An element or attribute name: everything up to a delimiter.
///
/// Names are only tokenized here, not validated against the XML grammar: the parser only ever
/// compares them against the fixed set of introspection names.
fn xml_name<'i>(input: &mut Input<'i>) -> PResult<&'i str> {
    take_while(1.., |c: char| {
        !c.is_ascii_whitespace() && !matches!(c, '=' | '/' | '>' | '<')
    })
    .parse_next(input)
}

/// A quoted attribute value, returning its raw (still escaped) contents and their byte offset.
fn quoted_value<'i>(input: &mut Input<'i>) -> PResult<(&'i str, usize)> {
    alt((
        delimited('"', take_until(0.., '"').with_span(), '"'),
        delimited('\'', take_until(0.., '\'').with_span(), '\''),
    ))
    .map(|(value, span)| (value, span.start))
    .parse_next(input)
}

/// Optional run of XML whitespace.
fn whitespace<'i>(input: &mut Input<'i>) -> PResult<&'i str> {
    take_while(0.., |c: char| c.is_ascii_whitespace()).parse_next(input)
}

/// At least one XML whitespace character.
fn whitespace1<'i>(input: &mut Input<'i>) -> PResult<&'i str> {
    take_while(1.., |c: char| c.is_ascii_whitespace()).parse_next(input)
}

/// Resolve entity and character references in an attribute value and normalize whitespace.
///
/// Per the XML attribute-value normalization rules, literal whitespace characters are replaced
/// with spaces, while whitespace escaped through character references (e.g. `&#10;`) is kept. A
/// literal `\r\n` collapses to a single space, as line-ending normalization (which folds it to a
/// lone `\n`) precedes attribute-value normalization. On failure, the error carries the byte
/// offset of the offending reference within `value`.
fn unescape(value: &str) -> std::result::Result<Cow<'_, str>, (String, usize)> {
    if !value.contains(['&', '\t', '\n', '\r']) {
        return Ok(Cow::Borrowed(value));
    }

    let mut unescaped = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(i) = rest.find(['&', '\t', '\n', '\r']) {
        unescaped.push_str(&rest[..i]);
        let reference = value.len() - rest.len() + i;
        if rest.as_bytes()[i] != b'&' {
            unescaped.push(' ');
            // A `\r\n` pair is a single line ending, so it normalizes to one space, not two.
            let width = if rest.as_bytes()[i] == b'\r' && rest.as_bytes().get(i + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            };
            rest = &rest[i + width..];
            continue;
        }
        rest = &rest[i + 1..];
        let end = rest
            .find(';')
            .ok_or(("unterminated entity reference".to_string(), reference))?;
        let entity = &rest[..end];
        match entity {
            "amp" => unescaped.push('&'),
            "lt" => unescaped.push('<'),
            "gt" => unescaped.push('>'),
            "quot" => unescaped.push('"'),
            "apos" => unescaped.push('\''),
            _ => unescaped.push(char_reference(entity).map_err(|e| (e, reference))?),
        }
        rest = &rest[end + 1..];
    }
    unescaped.push_str(rest);

    Ok(Cow::Owned(unescaped))
}

/// Resolve a numeric character reference (the part of `&#...;` between `&` and `;`).
fn char_reference(entity: &str) -> std::result::Result<char, String> {
    let invalid = || format!("invalid character reference `&{entity};`");

    let code = if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        u32::from_str_radix(hex, 16).ok()
    } else if let Some(dec) = entity.strip_prefix('#') {
        if dec.is_empty() || !dec.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        dec.parse().ok()
    } else {
        return Err(format!("unknown entity `&{entity};`"));
    };

    code.and_then(char::from_u32)
        .filter(|c| is_xml_char(*c))
        .ok_or_else(invalid)
}

/// Whether `c` is a valid XML 1.0 character (the `Char` production).
fn is_xml_char(c: char) -> bool {
    matches!(
        c,
        '\u{9}' | '\u{A}' | '\u{D}' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}'
    )
}

/// Escape a string for use as an attribute value.
///
/// Whitespace other than the space character is escaped as character references so that it
/// survives the attribute-value normalization done by parsers.
pub(crate) fn escape(value: &str) -> Cow<'_, str> {
    if !value.contains(['&', '<', '>', '"', '\'', '\t', '\n', '\r']) {
        return Cow::Borrowed(value);
    }

    let mut escaped = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\t' => escaped.push_str("&#9;"),
            '\n' => escaped.push_str("&#10;"),
            '\r' => escaped.push_str("&#13;"),
            c => escaped.push(c),
        }
    }

    Cow::Owned(escaped)
}
