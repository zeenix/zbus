# Deferred builder errors — design

Resolves z-galaxy/zbus#403 ("Error handling when creating instances via a builder") for the
zbus 6.0 major release.

## The problem

Every builder method that takes a D-Bus name, object path, GUID or address is generic over
`TryInto` and returns `Result<Self>`, so a caller writes `?` (or `.expect("cannot fail")`) after
each call even when the argument is already a validated `ObjectPath`, `BusName` or similar:

```rust
let proxy = PropertiesProxy::builder(&conn)
    .destination(proxy.destination())?   // &BusName: cannot fail
    .path(proxy.path())?                 // &ObjectPath: cannot fail
    .build()
    .await?;
```

Everything that can fail *before* `build()` today:

- `proxy::Builder` (and its `blocking` wrapper): `destination`, `path`, `interface`.
- `match_rule::Builder`: `sender`, `interface`, `member`, `path`, `path_namespace`,
  `destination`; also `arg`, `add_arg`, `arg_path`, `add_arg_path` (64-argument limit) and
  `arg0ns` (namespace rules), which fail for another reason.
- `message::Builder`: the `Message::method_call`, `Message::signal`, `Message::error` and
  `Message::method_return` constructors, `sender`, `path`, `interface`, `member`, `destination`;
  also `with_flags`, which fails for another reason (flag/type mismatch).
- `connection::Builder` (and its `blocking` wrapper): the `address` and `authenticated_socket`
  constructors, `server`, `serve_at`, `name`, `unique_name`; also `session`, `system` and
  `ibus`, which fail for another reason (environment lookup).

The functions outside the builders that take `TryInto` arguments — `Proxy::new`,
`Connection::call_method`, `ObjectServer::at`, `SignalEmitter::new`, … — fail for other reasons
too (I/O, missing objects) and are not part of the problem.

## Constraints

- 6.0 is unreleased on `main`, so this is the API-break window the issue was deferred to in
  2023. The upgrade guide promises a mechanical migration; every change here must be one the
  compiler points at and a one-token edit fixes.
- Strings stay accepted. Literals are the majority input (the issue's i3status-rust census: 11 of
  20 arguments were literals or `String`s) and the `#[proxy]` macro's generated `new()` takes
  generic `TryInto` arguments.
- MSRV 1.87, no new dependencies, no type-system machinery that leaks into rustdoc.

## Decision

Every builder setter and constructor keeps its zbus 5 generic signature:

```rust
pub fn path<P>(mut self, path: P) -> Self
where
    P: TryInto<ObjectPath<'a>>,
    P::Error: Into<Error>,
```

Only the return type changes, from `Result<Self>` to `Self`. The conversion still happens
immediately, inside the setter, but its outcome no longer travels back to the caller: a
converted value goes into the field it belongs to, and a conversion error goes into the one
`error: Option<Error>` the builder keeps, which only the first error to arrive fills. Nothing
clears it afterwards — not a later call to the same setter, not the sibling that overrides it
(`path` and `path_namespace`, `session` and `address`), not a valid value passed to any other
setter — so whether a setter replaces its field (`path`) or adds to it (`add_arg`, `name`,
`serve_at`, `with_flags`) makes no difference to the error. `build()` returns the recorded error
before it does anything else, with the same error value it would have returned eagerly today;
only the point where the error surfaces moves.

`Message::method_call`, `Message::signal`, `Message::error` and `Message::method_return` return
the builder directly, no `Result` involved. `match_rule::Builder::build()`
becomes fallible, returning `Result<MatchRule<'m>>`: it is the one builder whose `build()` was
previously infallible, because every field was already validated by the time it ran.
`MatchRule::try_from(&str)` keeps validating each of its components itself (`BusName::try_from`,
`ObjectPath::try_from`, …) before handing the typed value to the corresponding setter, so a bad
rule string reports the same parser error it does today. Generic code — `Proxy::new`,
`Connection::call_method`, the `#[proxy]`-generated `new()` — keeps its `TryInto` bounds
unchanged and simply drops the `?` after each setter call. There are no `try_`-prefixed twins
anywhere in this design.

Before, with string arguments:

```rust
let proxy = PropertiesProxy::builder(&conn)
    .destination("org.zbus.Foo")?
    .path("/org/zbus/Foo")?
    .build()
    .await?;
```

After:

```rust
let proxy = PropertiesProxy::builder(&conn)
    .destination("org.zbus.Foo")
    .path("/org/zbus/Foo")
    .build()
    .await?;
```

Before, with already-typed arguments:

```rust
let proxy = PropertiesProxy::builder(&conn)
    .destination(proxy.destination())?   // &BusName: cannot fail
    .path(proxy.path())?                 // &ObjectPath: cannot fail
    .build()
    .await?;
```

After:

```rust
let proxy = PropertiesProxy::builder(&conn)
    .destination(proxy.destination())   // &BusName: still cannot fail
    .path(proxy.path())                 // &ObjectPath: still cannot fail
    .build()
    .await?;
```

Both callers end up with exactly one `?`, at `build()`, regardless of whether their arguments
were strings or typed values.

## Why

Strings are the majority of real call sites, not the exception. A design that keeps setters
eager and adds a `try_`-prefixed fallible twin makes the common, string-passing caller rename
every call to `try_` and keep the `?` it already had, while only the minority typed-argument
caller gets to drop anything — it optimizes for the minority case and adds typing (a renamed
method, an extra character) for the majority. Deferring the error instead gives every caller,
string or typed, exactly one `?`, at `build()`, which is also where three of the four builders
(`proxy::Builder`, `message::Builder`, `connection::Builder`) already return `Result` today.

The sticky-state objection that the first draft of this spec raised against deferred errors —
that a corrected later call would not clear an earlier error — is accepted rather than
engineered away. An error per field, replaced whenever that field is set again, would buy a
recovery path for a chain no correct program writes: an invalid value reaching a setter is a bug
in the caller to fix, not a state to recover from at run time. One error per builder is less to
implement, less to document and less to read, and it is the design the maintainer chose. Parser
error identity for `MatchRule::try_from(&str)` is preserved for the same reason it always was: the
parser validates each component itself before it ever touches a setter, so the error a bad rule
string produces does not depend on which setter deferred errors internally. And the error values
themselves are unchanged: `build()` returns exactly the error a setter would have returned
eagerly today, just later; today's eager errors do not carry the name of the setter that
produced them either, so nothing is lost on that front.

## Costs accepted

- `MatchRule::builder().build()` now returns `Result`, even for a chain built entirely from
  already-typed arguments that could not have failed.
- An error surfaces at `build()` — or at the connection builder's `build().await` — instead of
  at the call site that actually caused it.
- The setter signature alone no longer shows whether it can fail: `path<P>(self, path: P) -> Self`
  gives no hint. `build()`'s `# Errors` section documents the deferral once per builder, and each
  fallible setter's own doc gets one sentence pointing at it.
- The recorded error is sticky: calling the same setter again with a valid value does not clear
  it and `build()` still fails. The way out is to fix the invalid value in the chain.

## Migration

The migration is mechanical and compiler-guided:

- Remove the `?` (or `.expect(...)`) after every builder setter call.
- Remove the `?` after `Message::method_call`, `Message::signal`, `Message::error` and
  `Message::method_return`.
- Remove the `?` after the `connection::Builder` constructors (`session`, `system`, `ibus`,
  `address`, `authenticated_socket`) and after `server`, `serve_at`, `name` and
  `unique_name`.
- Add a `?` after `MatchRule::builder()...build()`, which is now fallible.

Nothing else changes: argument types, generic bounds and error values are the same as today.

| Before | After |
|---|---|
| `.path("/org/zbus/Foo")?` | `.path("/org/zbus/Foo")` |
| `.path(object_path)?` or `.path(object_path).unwrap()` | `.path(object_path)` |
| `Message::method_call("/", "Ping")?` | `Message::method_call("/", "Ping")` |
| `MatchRule::builder().build()` | `MatchRule::builder().build()?` |

A stale `?` left after a setter fails to compile with `the ? operator can only be applied to
values that implement Try`, pointing straight at the call site to clean up. A missing `?` after
`MatchRule::builder().build()` fails the opposite way, with a type mismatch between `MatchRule`
and the value the rest of the function expects. Both are one-line fixes.

## Alternatives considered

**A. An infallible setter plus a `try_`-prefixed fallible twin.** Every setter would come in two
forms: an infallible one taking `impl Into<T>` for already-typed values, and a `try_`-prefixed
twin keeping the zbus 5 `TryInto` signature for strings and anything else needing validation,
following the `from`/`try_from` convention. This was implemented in full, on all four builders
and their blocking wrappers, with every in-tree caller, the README and the book migrated and the
whole CI matrix passing. It was rejected because it optimizes for
the minority of call sites: strings are the majority input (11 of 20 arguments in the
i3status-rust census), and this design is the one that forces exactly those callers to rename to
`try_` and keep their `?`, while adding a second method name and a naming convention that every
future fallible setter has to follow. Deferred errors reach the same `?`-free result for typed
arguments without a second method for anyone to learn.

**B. One setter with a conditional return type.** A trait with a generic associated type
(`type Output<B>`) would make `path(p)` return `Self` for typed input and `Result<Self>` for
strings, so there would be only one setter name per field. This was prototyped and compiling,
and it produced the smallest possible migration (only typed call sites would change at all). It
was rejected because the signature renders as `-> P::Output<Self> where P: Convert<ObjectPath<'a>>`,
which says nothing about when the call can fail; it needs one blanket impl plus around 55
concrete impls the crate would commit to forever; generic code cannot use `?` on the projected
type and would need to learn a `try_convert()`-first rule instead; the blanket `Into` impl stops
`try_into()` from inferring its target type from the setter that follows it; downstream types
that only implement `TryFrom<Their> for ObjectPath` would lose the setter entirely; and
multi-argument constructors such as `Message::method_call(path, member)` cannot use this shape at
all. No published crate exposes anything like it. The maintainer called this idea "a bit too
complicated" when the issue was first opened in 2023, and building it did not change that.

**C. Additive infallible setters under another name.** Give the infallible, typed-argument path a
distinct name instead of overloading the existing one — `raw_path`, `path_typed`, and so on —
leaving the original fallible setter untouched. This has zero migration cost, since nothing
existing changes. It was rejected because the convention it relies on runs backwards: an
ecosystem that marks one of two setters distinguishes the fallible one, not the infallible one,
so callers who never learn about the second method keep writing `.expect("cannot fail")`
indefinitely. The issue thread had already rejected the specific `raw_` prefix for the same
reason.

The first draft of this spec considered deferred errors and rejected them. Each objection it
raised, revisited against the design above:

- A stale error surviving after a later call fixes the field ("sticky state") is accepted as a
  cost: the builder records the first error a setter hits and nothing clears it, because a chain
  that passes an invalid value is a bug to fix rather than a state to recover from.
- `MatchRule::try_from(&str)` reporting a different error for the same input depending on what
  follows it in the string is resolved: the parser validates each component itself and calls the
  typed setter, so parser errors do not depend on setter internals.
- `MatchRule::builder().build()` becoming fallible for callers who never touched a fallible
  setter is accepted as a cost.
- A fully typed chain still ending in a `Result` is accepted as a cost.
- Error locality dropping to a first-error-only error with no setter name attached does not
  apply: the reported error carries the same value a setter would have returned eagerly today,
  and today's eager errors do not carry a setter name either.
- Needing a `# Errors` note on every `-> Self` setter is accepted, but reduced to one sentence per
  setter, with the deferral mechanism itself documented once, on `build()`.

## Follow-up, out of scope here

**Compile-time validated literals.** `zbus::object_path!("/org/zbus/Foo")`,
`zbus::names::interface_name!("org.zbus.Foo")` and siblings, backed by `const fn` validators,
would make literals infallible too: `.path(object_path!("/org/zbus/Foo"))` with a compile error
for a typo. The prototype rewrote the object-path and name validators as `const fn` byte loops
(0 mismatches against the winnow parsers over ~2.2M random and boundary inputs, winnow dropped
from the wire and names code) and showed `#[proxy]` can emit the macros for its `default_*`
literals. It is orthogonal to the setter design, has its own review surface, and lands as a
separate PR.
