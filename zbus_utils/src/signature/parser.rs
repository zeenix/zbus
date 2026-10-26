//! The parser of signature strings.
//!
//! It's made of winnow's combinators, and picks how to parse each type from its first byte. It
//! makes either a [`Signature`] of each type or, to only check it, nothing at all, so that checking
//! allocates nothing.

use std::mem;

use winnow::{
    Parser,
    combinator::{delimited, empty, eof, fail, opt, peek, repeat, repeat_till, terminated},
    dispatch,
    token::any,
};

use super::{Child, Error, Signature};

/// Parse a signature string.
pub(super) fn parse(mut bytes: &[u8]) -> Result<Signature, Error> {
    signature
        .parse_next(&mut bytes)
        .map_err(|()| Error::InvalidSignature)
}

/// Check a signature string, without allocating.
pub(super) fn validate(mut bytes: &[u8]) -> Result<(), Error> {
    signature
        .parse_next(&mut bytes)
        .map_err(|()| Error::InvalidSignature)
}

/// A signature: no type at all, which is the unit type, a single type, or more than one, which
/// are the fields of a structure, written without its parentheses, so they end with the input.
fn signature<T>(input: &mut &[u8]) -> PResult<T>
where
    T: Output,
{
    if input.is_empty() {
        return Ok(T::unit());
    }
    let first = single_type(Depth::default()).parse_next(input)?;
    if input.is_empty() {
        return Ok(first);
    }

    T::structure(first, input, Depth::default(), eof.void())
}

/// What the parser makes of each type that it parses.
trait Output: Sized {
    /// The unit type, which an empty signature is.
    fn unit() -> Self;

    /// A basic type, or a variant.
    fn basic(signature: Signature) -> Self;

    /// An array of `child`.
    fn array(child: Self) -> Self;

    /// A dict of `key` to `value`.
    fn dict(key: Self, value: Self) -> Self;

    /// A maybe of `child`.
    #[cfg(feature = "gvariant")]
    fn maybe(child: Self) -> Self;

    /// A structure whose first field is `first`, parsing the rest of its fields from `input`, at
    /// the depth `depth`, up to `end`, which ends them.
    fn structure<'i, E>(first: Self, input: &mut &'i [u8], depth: Depth, end: E) -> PResult<Self>
    where
        E: Parser<&'i [u8], (), ()>;
}

impl Output for Signature {
    fn unit() -> Self {
        Signature::Unit
    }

    fn basic(signature: Signature) -> Self {
        signature
    }

    fn array(child: Self) -> Self {
        Signature::Array(into_child(child))
    }

    fn dict(key: Self, value: Self) -> Self {
        Signature::Dict {
            key: into_child(key),
            value: into_child(value),
        }
    }

    #[cfg(feature = "gvariant")]
    fn maybe(child: Self) -> Self {
        Signature::Maybe(into_child(child))
    }

    fn structure<'i, E>(
        first: Self,
        input: &mut &'i [u8],
        depth: Depth,
        mut end: E,
    ) -> PResult<Self>
    where
        E: Parser<&'i [u8], (), ()>,
    {
        // Most structures have one or two fields, which are parsed as they come. Any more are
        // checked first, which counts them, so that all of them are allocated at once, with their
        // exact number.
        let second = match opt(end.by_ref()).parse_next(input)? {
            Some(()) => return Ok(Signature::structure(vec![first])),
            None => single_type(depth).parse_next(input)?,
        };
        if opt(end.by_ref()).parse_next(input)?.is_some() {
            return Ok(Signature::structure(vec![first, second]));
        }
        let (rest, ()): (usize, _) =
            peek(repeat_till(1.., single_type::<()>(depth), end.by_ref())).parse_next(input)?;
        let mut fields = Vec::with_capacity(2 + rest);
        fields.extend([first, second]);
        terminated(
            repeat(rest, single_type(depth)).fold(|| (), |(), field| fields.push(field)),
            end,
        )
        .parse_next(input)?;

        Ok(Signature::structure(fields))
    }
}

impl Output for () {
    fn unit() -> Self {}

    fn basic(signature: Signature) -> Self {
        // A basic type owns nothing, so there's nothing to drop. Forgetting it spares a call to the
        // drop glue of `Signature`, which the compiler doesn't leave out.
        mem::forget(signature);
    }

    fn array(_child: Self) -> Self {}

    fn dict(_key: Self, _value: Self) -> Self {}

    #[cfg(feature = "gvariant")]
    fn maybe(_child: Self) -> Self {}

    fn structure<'i, E>(_first: Self, input: &mut &'i [u8], depth: Depth, end: E) -> PResult<Self>
    where
        E: Parser<&'i [u8], (), ()>,
    {
        repeat_till(0.., single_type::<()>(depth), end)
            .map(|((), ())| ())
            .parse_next(input)
    }
}

/// A single complete type, at the nesting depth `depth`.
fn single_type<'i, T>(depth: Depth) -> impl Parser<&'i [u8], T, ()>
where
    T: Output,
{
    move |input: &mut &'i [u8]| {
        // Reject types nested past the container-depth limits. Besides matching what zbus can
        // actually encode, this keeps the recursion — and hence the stack usage — bounded on
        // adversarial input.
        if depth.exceeded() {
            return fail.parse_next(input);
        }

        // `dispatch!` takes no `#[cfg]` on its arms (winnow-rs/winnow#609), so `h` and `m` are
        // parsed by functions that only parse them where they're types.
        dispatch! {any;
            b'y' => empty.map(|()| T::basic(Signature::U8)),
            b'b' => empty.map(|()| T::basic(Signature::Bool)),
            b'n' => empty.map(|()| T::basic(Signature::I16)),
            b'q' => empty.map(|()| T::basic(Signature::U16)),
            b'i' => empty.map(|()| T::basic(Signature::I32)),
            b'u' => empty.map(|()| T::basic(Signature::U32)),
            b'x' => empty.map(|()| T::basic(Signature::I64)),
            b't' => empty.map(|()| T::basic(Signature::U64)),
            b'd' => empty.map(|()| T::basic(Signature::F64)),
            b's' => empty.map(|()| T::basic(Signature::Str)),
            b'g' => empty.map(|()| T::basic(Signature::Signature)),
            b'o' => empty.map(|()| T::basic(Signature::ObjectPath)),
            b'v' => empty.map(|()| T::basic(Signature::Variant)),
            b'h' => fd,
            b'a' => array(depth.inc_array()),
            b'(' => structure(depth.inc_structure()),
            b'm' => maybe(depth),
            _ => fail,
        }
        .parse_next(input)
    }
}

/// A file descriptor, of which its code is all there is.
#[cfg(unix)]
fn fd<T>(input: &mut &[u8]) -> PResult<T>
where
    T: Output,
{
    empty.map(|()| T::basic(Signature::Fd)).parse_next(input)
}

/// A file descriptor, which isn't a type but on Unix.
#[cfg(not(unix))]
fn fd<T>(input: &mut &[u8]) -> PResult<T> {
    fail.parse_next(input)
}

/// What follows the `a` of an array or of a dict, whose children are at the depth `depth`: the
/// child of an array, or the key and value of a dict, between braces.
fn array<'i, T>(depth: Depth) -> impl Parser<&'i [u8], T, ()>
where
    T: Output,
{
    // No type starts with a `{`, so one after the `a` is that of a dict.
    dispatch! {peek(any);
        b'{' => delimited(b'{', (single_type(depth), single_type(depth)), b'}')
            .map(|(key, value)| T::dict(key, value)),
        _ => single_type(depth).map(T::array),
    }
}

/// What follows the `(` of a structure, whose fields are at the depth `depth`: its fields, and the
/// `)` that ends them.
fn structure<'i, T>(depth: Depth) -> impl Parser<&'i [u8], T, ()>
where
    T: Output,
{
    move |input: &mut &'i [u8]| {
        let first = single_type(depth).parse_next(input)?;

        T::structure(first, input, depth, b')'.void())
    }
}

/// What follows the `m` of a maybe: its child, at the same depth, as a maybe does not count toward
/// the depth limits (see `Depth`).
#[cfg(feature = "gvariant")]
fn maybe<'i, T>(depth: Depth) -> impl Parser<&'i [u8], T, ()>
where
    T: Output,
{
    single_type(depth).map(T::maybe)
}

/// A maybe, which isn't a type but with GVariant.
#[cfg(not(feature = "gvariant"))]
fn maybe<'i, T>(_depth: Depth) -> impl Parser<&'i [u8], T, ()> {
    fail
}

/// A child signature, which points at a static signature when it's that of a basic type, rather
/// than boxing it.
fn into_child(signature: Signature) -> Child {
    let child = match signature {
        Signature::U8 => &Signature::U8,
        Signature::Bool => &Signature::Bool,
        Signature::I16 => &Signature::I16,
        Signature::U16 => &Signature::U16,
        Signature::I32 => &Signature::I32,
        Signature::U32 => &Signature::U32,
        Signature::I64 => &Signature::I64,
        Signature::U64 => &Signature::U64,
        Signature::F64 => &Signature::F64,
        Signature::Str => &Signature::Str,
        Signature::Signature => &Signature::Signature,
        Signature::ObjectPath => &Signature::ObjectPath,
        Signature::Variant => &Signature::Variant,
        #[cfg(unix)]
        Signature::Fd => &Signature::Fd,
        signature => return Child::from(signature),
    };
    // As in `<() as Output>::basic`, there's nothing to drop.
    mem::forget(signature);

    Child::Static { child }
}

/// A parser result. Its error is `()`, as there's only one way for a signature to be invalid.
type PResult<T> = Result<T, ()>;

/// The struct- and array-nesting depth at a point in a signature, used to bound the recursion.
#[derive(Debug, Default, Clone, Copy)]
struct Depth {
    structure: u8,
    array: u8,
}

impl Depth {
    fn inc_structure(mut self) -> Self {
        self.structure += 1;
        self
    }

    fn inc_array(mut self) -> Self {
        self.array += 1;
        self
    }

    /// Whether either nesting limit has been exceeded.
    fn exceeded(self) -> bool {
        self.structure > MAX_STRUCT_DEPTH || self.array > MAX_ARRAY_DEPTH
    }
}

// The maximum struct- and array-nesting depths a signature may use, matching the limits zbus
// enforces when (de)serializing (see its `wire::container_depths` module). This also bounds the
// recursion of the parser, so a hostile signature string — e.g. tens of thousands of `(` — cannot
// exhaust the stack. `maybe` (gvariant) is deliberately not bounded, to stay no stricter than
// what the (de)serializer accepts.
const MAX_STRUCT_DEPTH: u8 = 32;
const MAX_ARRAY_DEPTH: u8 = 32;
