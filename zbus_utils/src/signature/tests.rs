use super::*;
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    str::FromStr,
};

macro_rules! validate {
        ($($signature:literal => $expected:expr),+) => {
            $(
                assert!(validate($signature.as_bytes()).is_ok());
                let parsed = Signature::from_str($signature).unwrap();
                assert_eq!(parsed, $expected);
                assert_eq!(parsed, $signature);
                match parsed {
                    Signature::Structure(_) => {
                        assert!(
                            $signature.len() == parsed.string_len() - 2 ||
                            $signature.len() == parsed.string_len()
                        );
                    }
                    _ => {
                        assert_eq!(parsed.string_len(), $signature.len());
                    }
                }
            )+
        };
    }

#[test]
fn validate_strings() {
    validate!(
        "" => Signature::Unit,
        "y" => Signature::U8,
        "b" => Signature::Bool,
        "n" => Signature::I16,
        "q" => Signature::U16,
        "i" => Signature::I32,
        "u" => Signature::U32,
        "x" => Signature::I64,
        "t" => Signature::U64,
        "d" => Signature::F64,
        "s" => Signature::Str,
        "g" => Signature::Signature,
        "o" => Signature::ObjectPath,
        "v" => Signature::Variant,
        "xs" => Signature::structure(&[&Signature::I64, &Signature::Str][..]),
        "(ysa{sd})" => Signature::static_structure(
            &[
                &Signature::U8,
                &Signature::Str,
                &Signature::Dict {
                    key: Child::Static { child: &Signature::Str },
                    value: Child::Static { child: &Signature::F64 },
                },
            ]
        ),
        "a(y)" => Signature::static_array(
            &Signature::Structure(Fields::Static { fields: &[&Signature::U8] }),
        ),
        "a{yy}" => Signature::static_dict(&Signature::U8, &Signature::U8),
        "(yy)" => Signature::static_structure(&[&Signature::U8, &Signature::U8]),
        "a{sd}" => Signature::static_dict(&Signature::Str, &Signature::F64),
        "a{yy}" => Signature::static_dict(&Signature::U8, &Signature::U8),
        "a{sv}" => Signature::static_dict(&Signature::Str, &Signature::Variant),
        "a{sa{sv}}" => Signature::static_dict(
            &Signature::Str,
            &Signature::Dict {
                key: Child::Static {
                child: &Signature::Str,
                },
                value: Child::Static {
                child: &Signature::Variant
                }
            },
        ),
        "a{sa(ux)}" => Signature::static_dict(
            &Signature::Str,
            &Signature::Array(Child::Static {
                child: &Signature::Structure(Fields::Static {
                    fields: &[&Signature::U32, &Signature::I64]
                }),
            }),
        ),
        "(x)" => Signature::static_structure(&[&Signature::I64]),
        "(x(isy))" => Signature::static_structure(&[
            &Signature::I64,
            &Signature::Structure(Fields::Static {
                fields: &[&Signature::I32, &Signature::Str, &Signature::U8]
            }),
        ]),
        "(xa(isy))" => Signature::static_structure(&[
            &Signature::I64,
            &Signature::Array(Child::Static {
                child: &Signature::Structure(Fields::Static {
                    fields: &[&Signature::I32, &Signature::Str, &Signature::U8]
                }),
            }),
        ]),
        "(xa(s))" => Signature::static_structure(&[
            &Signature::I64,
            &Signature::Array(Child::Static {
                child: &Signature::Structure(Fields::Static {
                    fields: &[&Signature::Str]
                }),
            }),
        ]),
        "((yyyyuu)a(yv))" => Signature::static_structure(&[
            &Signature::Structure(Fields::Static {
                fields: &[
                    &Signature::U8, &Signature::U8, &Signature::U8, &Signature::U8,
                    &Signature::U32, &Signature::U32,
                ],
            }),
            &Signature::Array(Child::Static {
                child: &Signature::Structure(Fields::Static {
                    fields: &[&Signature::U8, &Signature::Variant],
                }),
            }),
        ])
    );
    #[cfg(unix)]
    validate!("h" => Signature::Fd);
}

macro_rules! invalidate {
        ($($signature:literal),+) => {
            $(
                assert!(validate($signature.as_bytes()).is_err());
            )+
        };
    }

#[test]
fn invalid_strings() {
    invalidate!(
        "a",
        "a{}",
        "a{y",
        "a{y}",
        "a{y}a{y}",
        "a{y}a{y}a{y}",
        "z",
        "()",
        "(x",
        "(x())",
        "(xa()",
        "(xa(s)",
        "(xs",
        "xs)",
        "s/",
        "a{yz}"
    );
}

#[test]
fn recursion_limits() {
    // Deeply nested structs or arrays are rejected rather than overflowing the stack — the
    // parser fails as soon as the limit is passed, without recursing through the whole string.
    // Both the parsing and the allocation-free `check_only` (`validate`) paths reject them.
    let deep_structs = format!("{}i{}", "(".repeat(20_000), ")".repeat(20_000));
    assert!(Signature::from_str(&deep_structs).is_err());
    assert!(validate(deep_structs.as_bytes()).is_err());
    let deep_arrays = format!("{}i", "a".repeat(20_000));
    assert!(Signature::from_str(&deep_arrays).is_err());
    assert!(validate(deep_arrays.as_bytes()).is_err());

    // The exact per-container limits mirror zbus's (de)serialization limits (see
    // `zbus::wire::container_depths`): 32 deep is accepted, 33 rejected — for structs and
    // arrays.
    assert!(validate(format!("{}i{}", "(".repeat(32), ")".repeat(32)).as_bytes()).is_ok());
    assert!(validate(format!("{}i{}", "(".repeat(33), ")".repeat(33)).as_bytes()).is_err());
    assert!(validate(format!("{}i", "a".repeat(32)).as_bytes()).is_ok());
    assert!(validate(format!("{}i", "a".repeat(33)).as_bytes()).is_err());

    // A long but shallow signature (many fields, little nesting) is fine — there is no length
    // limit, so a wide struct or a top-level run of many types is accepted.
    assert!(validate("i".repeat(1000).as_bytes()).is_ok());
    assert!(validate(format!("({})", "i".repeat(1000)).as_bytes()).is_ok());
}

#[test]
fn hash() {
    // We need to test if all variants of Signature hold this invariant:
    test_hash(&Signature::U16, &Signature::U16);
    test_hash(
        &Signature::array(Signature::U16),
        &Signature::static_array(&Signature::U16),
    );
    test_hash(
        &Signature::dict(Signature::U32, Signature::Str),
        &Signature::static_dict(&Signature::U32, &Signature::Str),
    );
    test_hash(
        &Signature::structure([Signature::Str, Signature::U64]),
        &Signature::static_structure(&[&Signature::Str, &Signature::U64]),
    );
}

fn test_hash(signature1: &Signature, signature2: &Signature) {
    assert_eq!(signature1, signature2);

    let mut hasher = DefaultHasher::new();
    signature1.hash(&mut hasher);
    let hash1 = hasher.finish();

    let mut hasher = DefaultHasher::new();
    signature2.hash(&mut hasher);
    let hash2 = hasher.finish();

    assert_eq!(hash1, hash2);
}

/// Check that the parser accepts and rejects exactly the strings that the one it replaced did, and
/// makes equal signatures of them, on strings of all the shapes that [`for_each_string`] makes.
#[test]
fn same_as_reference() {
    for_each_string(|string| {
        let parsed = Signature::from_bytes(string);
        assert_eq!(
            parsed,
            reference::parse(string, false),
            "{}",
            quoted(string)
        );

        let valid = validate(string).is_ok();
        assert_eq!(
            valid,
            reference::parse(string, true).is_ok(),
            "{}",
            quoted(string)
        );
        assert_eq!(valid, parsed.is_ok(), "{}", quoted(string));
    });
}

/// `string` as a literal of bytes, for the messages of failed assertions, which tells apart bytes
/// that are not UTF-8.
fn quoted(string: &[u8]) -> String {
    format!("b\"{}\"", string.escape_ascii())
}

/// The bytes of every type code, of the brackets of structures and dicts, and one that is none of
/// them.
const TYPE_BYTES: &[u8] = b"ybnqiuxtdsgovhma(){}z";

/// The bytes that tell the kinds of types apart, as the containers are built of them: the basic
/// types `y` and `s`, a variant, a file descriptor, the brackets of structures and dicts, an array
/// and a maybe, and one that is none of them.
const CONTAINER_BYTES: &[u8] = b"ysvha({)}mz";

/// Call `check` with strings to check the parser of signatures on: every one of up to 2 bytes,
/// those of up to 6 bytes made of the bytes that the containers are built of, those of up to 4 of
/// all the type codes, signatures nested either side of the limits of the depth of structures and
/// of arrays, and long ones.
///
/// The strings are made one after the other in a single buffer, as there are millions of them.
fn for_each_string<F>(mut check: F)
where
    F: FnMut(&[u8]),
{
    let mut string = Vec::with_capacity(8);
    check(&string);
    for first in 0..=u8::MAX {
        string.push(first);
        check(&string);
        for second in 0..=u8::MAX {
            string.push(second);
            check(&string);
            string.pop();
        }
        string.pop();
    }

    // To keep the number of strings down, those of 5 and 6 bytes are made only of the bytes that
    // the containers are built of, while all the type codes go up to 4 bytes.
    extend(&mut string, CONTAINER_BYTES, 6, &mut check);
    extend(&mut string, TYPE_BYTES, 4, &mut check);

    for_each_nested(&mut check);
    for_each_long(&mut check);
}

/// Extend `string` with each of `bytes`, up to `len` bytes, and call `check` with each string of 3
/// bytes or more that it makes.
fn extend<F>(string: &mut Vec<u8>, bytes: &[u8], len: usize, check: &mut F)
where
    F: FnMut(&[u8]),
{
    for byte in bytes {
        string.push(*byte);
        if string.len() > 2 {
            check(string);
        }
        if string.len() < len {
            extend(string, bytes, len, check);
        }
        string.pop();
    }
}

/// Call `check` with each prefix of `string`, the empty one and `string` itself included.
///
/// All but a few of the prefixes of a signature leave a structure or a dict unclosed, or a type
/// without its child.
fn check_prefixes<F>(string: &[u8], check: &mut F)
where
    F: FnMut(&[u8]),
{
    for len in 0..=string.len() {
        check(&string[..len]);
    }
}

/// Call `check` with signatures nested 30 to 34 deep, either side of the limits of 32 structs and
/// of 32 arrays, and with each prefix of them.
///
/// There are arrays, structs, dicts (whose key and value are one array deeper), maybes (which are
/// not limited), and mixes of them, as the structs and the arrays are counted separately: 32 of
/// each, nested together, are accepted. Each is also made the second of two types, to check that
/// the types of the top level of a signature, which are those of an implicit structure, are not
/// counted as one.
fn for_each_nested<F>(check: &mut F)
where
    F: FnMut(&[u8]),
{
    // The text to repeat before the innermost type, `i`, and the one to repeat after it.
    const SHAPES: [(&str, &str); 8] = [
        ("a", ""),
        ("(", ")"),
        ("a(", ")"),
        ("a{s", "}"),
        ("a{", "i}"),
        ("m", ""),
        ("am", ""),
        ("(i", ")"),
    ];

    for depth in 30..=34 {
        for (before, after) in SHAPES {
            let nested = format!("{}i{}", before.repeat(depth), after.repeat(depth));
            check_prefixes(nested.as_bytes(), check);
            check_prefixes(format!("i{nested}").as_bytes(), check);
        }
    }

    for arrays in 30..=34 {
        for structs in 30..=34 {
            let (arrays, structs, closes) =
                ("a".repeat(arrays), "(".repeat(structs), ")".repeat(structs));
            check_prefixes(format!("{arrays}{structs}i{closes}").as_bytes(), check);
            check_prefixes(format!("{structs}{arrays}i{closes}").as_bytes(), check);
        }
    }
}

/// Call `check` with long signatures: a run of 1000 types, a structure of the same run, and a
/// structure of 255 bytes, which is the length that the specification limits signatures to, with
/// each of its prefixes.
fn for_each_long<F>(check: &mut F)
where
    F: FnMut(&[u8]),
{
    check("i".repeat(1000).as_bytes());
    check(format!("({})", "i".repeat(1000)).as_bytes());

    let signature = format!("({}(usti))", "((st(ts)atas)a{sv})".repeat(13));
    assert_eq!(signature.len(), 255);
    check_prefixes(signature.as_bytes(), check);
}

/// The parser as it was, written with winnow's combinators, for the parser, which is written for
/// speed, to be checked against.
mod reference {
    use super::super::{Error, Signature};

    /// Parse a signature string into a `Signature`.
    ///
    /// When `check_only` is true, the function will not allocate memory for the dynamic types.
    /// Instead it will return dummy values in the parsed Signature.
    pub(super) fn parse(bytes: &[u8], check_only: bool) -> Result<Signature, Error> {
        use winnow::{
            Parser,
            combinator::{alt, delimited, empty, eof, fail, repeat},
            dispatch,
            token::any,
        };

        let unit = eof.map(|_| Signature::Unit);

        // `many1` allocates so we only want to use it when `check_only == false`
        type ManyError = winnow::error::ErrMode<()>;

        // The maximum struct- and array-nesting depths a signature may use, matching the limits
        // zbus enforces when (de)serializing (see its `wire::container_depths` module). This also
        // bounds the recursion below, so a hostile signature string — e.g. tens of thousands of `(`
        // — cannot exhaust the stack. `maybe` (gvariant) is deliberately not bounded, to stay no
        // stricter than what the (de)serializer accepts.
        const MAX_STRUCT_DEPTH: u8 = 32;
        const MAX_ARRAY_DEPTH: u8 = 32;

        /// The struct- and array-nesting depth at a point in a signature, used to bound the
        /// recursion.
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

        fn many(
            bytes: &mut &[u8],
            check_only: bool,
            top_level: bool,
            depth: Depth,
        ) -> Result<Signature, ManyError> {
            let parser = |s: &mut _| parse_signature(s, check_only, depth);
            if check_only {
                return repeat(1.., parser)
                    .map(|_: ()| Signature::Unit)
                    .parse_next(bytes);
            }

            // Avoid the allocation of `Vec<Signature>` in case of a single signature on the
            // top-level. This is a a very common case, especially in variants, where the signature
            // needs to be parsed at runtime.
            enum SignatureList {
                Unit,
                One(Signature),
                Structure(Vec<Signature>),
            }

            repeat(1.., parser)
                .fold(
                    || SignatureList::Unit,
                    |acc, signature| match acc {
                        // On the top-level, we want to return the signature directly if there is
                        // only one.
                        SignatureList::Unit if top_level => SignatureList::One(signature),
                        SignatureList::Unit => SignatureList::Structure(vec![signature]),
                        SignatureList::One(one) => SignatureList::Structure(vec![one, signature]),
                        SignatureList::Structure(mut signatures) => {
                            signatures.push(signature);
                            SignatureList::Structure(signatures)
                        }
                    },
                )
                .map(|sig_list| match sig_list {
                    SignatureList::Unit => Signature::Unit,
                    SignatureList::One(sig) => sig,
                    SignatureList::Structure(signatures) => Signature::structure(signatures),
                })
                .parse_next(bytes)
        }

        fn parse_signature(
            bytes: &mut &[u8],
            check_only: bool,
            depth: Depth,
        ) -> Result<Signature, ManyError> {
            // Reject types nested past the container-depth limits. Besides matching what zbus can
            // actually encode, this keeps the recursion below — and hence the stack usage —
            // bounded on adversarial input.
            if depth.exceeded() {
                return fail.parse_next(bytes);
            }
            let array_depth = depth.inc_array();
            let struct_depth = depth.inc_structure();

            let simple_type = dispatch! {any;
                b'y' => empty.value(Signature::U8),
                b'b' => empty.value(Signature::Bool),
                b'n' => empty.value(Signature::I16),
                b'q' => empty.value(Signature::U16),
                b'i' => empty.value(Signature::I32),
                b'u' => empty.value(Signature::U32),
                b'x' => empty.value(Signature::I64),
                b't' => empty.value(Signature::U64),
                b'd' => empty.value(Signature::F64),
                b's' => empty.value(Signature::Str),
                b'g' => empty.value(Signature::Signature),
                b'o' => empty.value(Signature::ObjectPath),
                b'v' => empty.value(Signature::Variant),
                _ => fail,
            };

            let dict = (
                b'a',
                delimited(
                    b'{',
                    (
                        move |s: &mut _| parse_signature(s, check_only, array_depth),
                        move |s: &mut _| parse_signature(s, check_only, array_depth),
                    ),
                    b'}',
                ),
            )
                .map(|(_, (key, value))| {
                    if check_only {
                        return Signature::Dict {
                            key: Signature::Unit.into(),
                            value: Signature::Unit.into(),
                        };
                    }

                    Signature::Dict {
                        key: key.into(),
                        value: value.into(),
                    }
                });

            let array = (b'a', move |s: &mut _| {
                parse_signature(s, check_only, array_depth)
            })
                .map(|(_, child)| {
                    if check_only {
                        return Signature::Array(Signature::Unit.into());
                    }

                    Signature::Array(child.into())
                });

            let structure = delimited(
                b'(',
                move |s: &mut _| many(s, check_only, false, struct_depth),
                b')',
            );

            // `maybe` does not count toward the depth limits (see `Depth`), so its child is parsed
            // at the same depth.
            #[cfg(feature = "gvariant")]
            let maybe =
                (b'm', move |s: &mut _| parse_signature(s, check_only, depth)).map(|(_, child)| {
                    if check_only {
                        return Signature::Maybe(Signature::Unit.into());
                    }

                    Signature::Maybe(child.into())
                });

            alt((
                simple_type,
                dict,
                array,
                structure,
                #[cfg(feature = "gvariant")]
                maybe,
                // FIXME: Should be part of `simple_type` but that's not possible right now:
                // https://github.com/winnow-rs/winnow/issues/609
                #[cfg(unix)]
                b'h'.map(|_| Signature::Fd),
            ))
            .parse_next(bytes)
        }

        let signature = alt((unit, |s: &mut _| {
            many(s, check_only, true, Depth::default())
        }))
        .parse(bytes)
        .map_err(|_| Error::InvalidSignature)?;

        Ok(signature)
    }
}
