# Borrowed strings in the `zbus_xml` introspection tree

Design for [z-galaxy/zbus#9](https://github.com/z-galaxy/zbus/issues/9): stop allocating a
`String` for every attribute value and docstring when parsing an introspection document.

## Problem

`zbus_xml` parses a D-Bus introspection document into a `Node` tree. Half of the tree already
carries a lifetime — `Node<'a>`, `Interface<'a>`, `Method<'a>`, `Signal<'a>` and `Property<'a>`
hold their names as `zbus::names::*Name<'a>` — but the parser never makes use of it: every
name is copied into an owned `String`, and the remaining string fields (`Annotation` name and
value, `Arg` name, node names, Telepathy docstrings, `tp:type` references and the Telepathy type
definitions) are plain `String`s on lifetime-less types. A parsed tree is thus always
`Node<'static>` and the lifetime parameter is dead weight for users, who have to spell it out
without getting anything in return.

The typical consumer (`zbus_xmlgen`, or a client inspecting a freshly introspected object) uses
the tree once, while the document is still in hand. For them the copies are pure overhead — and
docstrings in Telepathy-style documents can be large.

## Design

### `zbus::Str` for every string field

All string fields become `zbus::Str<'a>` (or `Option<Str<'a>>`), not `Cow<'a, str>`:

* The name fields of the same structs are `zbus::names::*Name<'a>`, which are `Str<'a>`
  wrappers. One ownership model for the whole tree means `into_owned()`/`to_owned()` behave
  the same for every field.
* `Str` is what zbus uses for exactly this purpose everywhere else (`Value::Str`, the name
  types), so users meet no new type.
* Cloning a tree whose strings are owned is cheap (`Str` shares an `Arc<str>`), and
  `Str::to_owned()` on already-owned data does not reallocate. `Cow` would deep-copy.
* `Str: From<Cow<'a, str>>`, so the parser's unescaped attribute values (borrowed when the
  value needs no unescaping, owned otherwise) slot in directly.

The accessors keep returning `&str` / `Option<&str>`, so the field type is not visible in the
common read path.

### Types gaining a lifetime

* `Annotation<'a>` (`name`, `value`)
* `Arg<'a>` (`name`, `docstring`, `tp_type`, `annotations: Vec<Annotation<'a>>`)
* `telepathy::TypeDef<'a>`, `SimpleType<'a>`, `Enum<'a>`, `EnumValue<'a>`, `Struct<'a>`,
  `Member<'a>`, `Mapping<'a>`
* The already parameterised types switch their `String` fields (node `name`, `docstring`,
  `tp_type`) to `Str<'a>` and their `Vec<Annotation>`/`Vec<Arg>`/`Vec<TypeDef>` fields to
  the `'a` variants.

`Signature`, `ArgDirection`, `PropertyAccess`, `Warning`, `Error` and `XmlError` are
unchanged. `Warning` is a diagnostic handed back next to the tree, not part of it, and it
carries a formatted message anyway.

The `Serialize`/`Deserialize` derives on the tree types, with their quick-xml `@name` field
renames, and the `Signature` newtype (which only existed to be deserialized from an owned
string) are leftovers of the quick-xml parser that 5.2 replaced. Rather than teach them to
borrow, drop them along with the `serde` dependency; the `ty()` accessors return
`&zbus::Signature` directly.

### Parsing borrows, reading owns

* `impl TryFrom<&'a str> for Node<'a>` borrows from the document: attribute values that need
  no unescaping and docstrings are `&'a str` slices, everything else is owned. This is the
  zero-copy path.
* `Node::from_reader` and `Node::from_reader_with_warnings` read the input into a local
  `String`, so they cannot borrow. They return `Node<'static>` (previously the vacuous
  `Node<'a>`): parse borrowing from the local buffer, then `into_owned()`. Same number of
  allocations as today, and `Node<'static>` coerces to any `Node<'a>` (all types are
  covariant in `'a`).
* Internally, the parser functions' `'static` return types become `'i` (the input lifetime).

### Owning a tree

Every type with a lifetime gets, in the style of the name types:

```rust
/// Creates an owned clone of `self`.
pub fn to_owned(&self) -> Self<'static>;
/// Converts `self` into an owned tree, copying the strings it borrows.
pub fn into_owned(self) -> Self<'static>;
```

so that a user who parsed with `TryFrom<&str>` and wants to keep (part of) the tree past the
document can, and one holding `&Node<'a>` can extract an `Interface<'static>`.

## Consumers

* `zbus_xmlgen`: `TypeDef` → `TypeDef<'_>` in signatures; `Types<'i>` holds
  `&'i TypeDef<'i>`. No behavioural change.
* `zbus` integration tests: use `Node::from_reader`, unchanged.
* Book: an "Other changes in 6.0" entry in `upgrading-to-6.md`.

## Testing

* Existing tests exercise both parse paths and the round trip.
* New: `Node::try_from(&str)` borrows (a `Node` built from a `String` must not outlive it —
  checked through `into_owned()` making it outlive the buffer), `from_reader` yields a
  `Node<'static>`, `to_owned()`/`into_owned()` compare equal to the source.
