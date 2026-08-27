# zvariant merge into zbus — design

Resolves z-galaxy/zbus#1919 ("RFC: Merge zvariant into zbus directly") and #1144 ("Drop GVariant
support") for the zbus 6.0 major release. The deprecated GVariant support is removed, then the
`zvariant`, `zvariant_derive` and `zbus_names` crates are folded into `zbus` and `zbus_macros`;
the D-Bus API is put behind a `comms` feature so that
`zbus = { version = "6", default-features = false }` is the replacement for anyone who only needs
the wire format.

## Goals

- One crate for D-Bus users. `zbus::wire` (the former zvariant) and `zbus::names` (the former
  zbus_names) are modules of zbus, with a single `zbus::Error`.
- A drop-in for wire-format-only users: zbus with `default-features = false` builds exactly the
  dependency set zvariant 5 builds today, exposes the same API surface under `zbus::wire`, and
  offers the same optional-impl features under the same names.
- No manifest change for existing zbus users, including the
  `default-features = false, features = ["tokio"]` idiom.
- The old paths keep compiling through a deprecated compatibility layer wherever the compiler can
  express that, so downstream code migrates on its own schedule.
- Finish the GVariant split (#1144): the support deprecated in zvariant 5.15 is deleted before
  the move, so zbus never carries GVariant code or a `gvariant` feature.

## Non-goals

- Removing the deprecated API (#1915), `proc-macro-crate` (#1365) or the dummy `async-fs` feature
  (#1287). Those stay separate issues; only the GVariant-specific deprecated API goes away, with
  the GVariant removal.
- Splitting the D-Bus API further into `proxy`/`service` features (#1135). The feature model here
  is designed so that split slots underneath `comms` later.
- A facade `zvariant` 6.0 crate re-exporting from zbus. Decided against: no ecosystem precedent,
  it defeats `cargo-semver-checks`, and derive-macro path resolution makes it fragile for exactly
  the users it targets.
- Merging `zvariant_utils`. zgvariant depends on it, so it stays a separately published 4.x crate.
- Updating downstream repositories (busd, zbus_polkit) — they get trivial follow-up PRs.

## Decisions

| Question | Decision |
|---|---|
| Transition for direct zvariant/zbus_names users | No facade; final 5.x/4.x banner releases |
| Name of the merged wire-format module | `zbus::wire` (the D-Bus specification's own term) |
| Error types | One `zbus::Error`; the other two enums are flattened into it |
| Gate for the D-Bus API | Feature `comms`, implied by every runtime/D-Bus feature |
| Branching | main becomes 6.0-dev; the merge PR bumps versions first |
| gvariant code | Removed in this effort (#1144), as the first step, before the move |

## Crate layout and versions

| Crate | Before | After |
|---|---|---|
| zbus | 5.19.0 | 6.0.0 — gains `pub mod wire` and `pub mod names`; D-Bus API behind `comms` |
| zbus_macros | 5.19.0 | 6.0.0 — gains the derives and `signature!`; no zvariant/zbus_names deps |
| zvariant_utils | 4.2.0 | 4.3.0 — gains the `names` and `object_path` validators (additive) |
| zbus_xml | 5.2.1 | 6.0.0 — depends on zbus (`default-features = false`) instead |
| zbus_xmlgen | 5.4.1 | 6.0.0 |
| zvariant | 5.15.0 | removed; final docs-only release 5.15.1 from a pre-merge tree |
| zvariant_derive | 5.15.0 | removed; final docs-only release 5.15.1 from a pre-merge tree |
| zbus_names | 4.3.4 | removed; final docs-only release 4.3.5 from a pre-merge tree |

The final releases of the three removed crates happen after zbus 6.0.0 is published (see
"Final releases of the removed crates"). Dependency chain after the merge:
`zvariant_utils → zbus_macros → zbus → zbus_xml → zbus_xmlgen`. zbus_macros no longer builds
host-side copies of zvariant, zbus_names, enumflags2, zcheapstr and endi (serde and winnow
stay: zvariant_utils depends on both).

zvariant_utils policy: it stays on 4.x for the whole 6.0 cycle. zgvariant pins `^4.1`; a major
bump there would resolve two copies of `zvariant_utils` and split `Signature` into two
incompatible types across zbus and zgvariant.

## GVariant removal (#1144)

The deprecated GVariant support is deleted first, in the same PR, so the move never sees it.
Removed from zvariant:

- `src/gvariant/{mod,de,ser}.rs`, `src/maybe.rs`, `src/framing_offset_size.rs`,
  `src/framing_offsets.rs` and their `lib.rs` declarations (2,173 LOC).
- `Value::Maybe` with its `visit_some`/`visit_none` support, `Context::new_gvariant`, the
  `VARIANT_ALIGNMENT_GVARIANT`/`MAYBE_SIGNATURE_CHAR`/`MAYBE_SIGNATURE_STR` constants,
  `Error::MissingFramingOffset`, and the 56 `#[cfg(feature = "gvariant")]` sites across 12
  files (`value.rs`, `serialized/data.rs`, `container_depths.rs`, `de.rs`, `ser.rs`,
  `owned_value.rs`, `into_value.rs`, `from_value.rs`, `utils.rs`, `serialized/context.rs`,
  `type/libstd.rs`, `lib.rs`), keeping the D-Bus branch of each.
- The `gvariant` and `ostree-tests` features of zvariant and the `gvariant` pass-through
  features of zvariant_derive and zbus_macros.
- The gvariant-gated tests: the files and modules that only run under the feature
  (`serde_bytes_gvariant.rs`, `issue/issue_99.rs`, …), the GVariant halves of the
  format-parametrized tests (`tests/number/`, `array_value.rs` — their D-Bus halves stay), and
  the seven `#![cfg_attr(feature = "gvariant", allow(deprecated))]` lines. The GVariant test
  suite lives on in the zgvariant repository.
- The two `cargo test -p zvariant --features gvariant` CI runs. The "deprecated, removed in
  6.0" wording in zvariant's README, the book and AGENTS.md becomes a plain pointer to
  zgvariant.

Kept, because they serve the zbus + zgvariant coexistence graph rather than GVariant support:
zvariant_utils' own `gvariant` feature with `Format::GVariant`/`Signature::Maybe` and the
`Signature::Maybe` arm in its shared codegen; zvariant's wildcard/rejection arms for
`Format::GVariant` and maybe-bearing signatures (`ser.rs`, `serialized/data.rs`,
`dbus::reject_maybe`, the `dbus_maybe_rejection.rs` test); and the coexistence CI check
(`--features zvariant_utils/gvariant`). The non-GVariant deprecated items (`DeserializeValue`,
`SerializeValue`, `vec_to_cstr`) stay for #1915.

## Public API

### `zbus::wire`

`zvariant/src` becomes `zbus/src/wire/`, exposing the same items at `zbus::wire::…`: `Type`,
`Basic`, `DynamicType`, `NoneValue`, `Value`, `OwnedValue`, `Signature`, `ObjectPath`,
`OwnedObjectPath`, `Str`, `Fd`, `OwnedFd`, `FilePath`, `Array`, `Dict`, `Structure`,
`StructureBuilder`, `DynamicTuple`, `Optional`, the signature-char constants, `Endian`/`LE`/`BE`/
`NATIVE_ENDIAN`, `to_bytes`, `to_bytes_for_signature`, `serialized_size`, the submodules
`serialized`, `signature`, `dbus`, `as_value`, the derives `Type`, `Value`, `OwnedValue`,
`SerializeDict`, `DeserializeDict` and the `signature!` macro. Two things move out of it:
`Error`/`Result`/`MaxDepthExceeded` live at the zbus root (see "Unified `zbus::Error`"); `wire`
keeps `#[doc(hidden)] pub use crate::{Error, Result}` because the shared derive codegen in
zvariant_utils emits `<path>::Error`/`<path>::Result` (zgvariant uses the same codegen against
its own root). `wire::export { pub use serde; }` stays as hidden macro support.

The module docs are the former `zvariant/README.md`, rewritten for the new paths and with
intra-doc links instead of docs.rs URLs. GVariant users are pointed at zgvariant.

### `zbus::names`

`zbus_names/src` becomes `zbus/src/names/`, unchanged in API except that `names::Error` and
`names::Result` become deprecated aliases of the root types. The module docs are the former
`zbus_names/README.md`.

### Compatibility module `zbus::zvariant`

What rustc 1.98 actually warns about, verified with a scratch workspace:

- `#[deprecated] pub mod zvariant` warns on `use zbus::zvariant;` and `use zbus::{zvariant, …}`
  (the idiom most zbus code uses), but not on paths through it (`use zbus::zvariant::Value;`).
- Deprecated type aliases, functions, constants and `macro_rules!` warn on use.
- `#[deprecated]` on a `pub use` never warns, whatever it re-exports.

So `zbus/src/zvariant.rs` is one `#[deprecated(since = "6.0.0", note = "renamed to `wire`")]`
module containing:

- deprecated type aliases for every struct and enum listed above (`pub type Value<'a> =
  crate::wire::Value<'a>;` and so on), plus nested deprecated `serialized` and `signature`
  modules aliasing `Context`, `Data`, `Format`, `Size`, `Written`, `Signature`, `Child`,
  `Fields`, `Error`;
- deprecated `Error`, `Result` and `MaxDepthExceeded` aliases of the root types;
- deprecated wrapper functions for `to_bytes`, `to_bytes_for_signature`, `serialized_size` and
  `padding_for_n_bytes`, with the same signatures;
- deprecated constants for the signature chars/strings and `LE`/`BE`/`NATIVE_ENDIAN`;
- plain (silent) re-exports for the four traits, the five derives, `signature!`, and the
  `as_value` and `dbus` submodules — nothing can make these warn.

Enum variants and associated functions resolve through type aliases (`zvariant::Value::U8(1)`,
`zvariant::Value::from(x)`, `impl From<X> for zvariant::Value<'_>` all work). The one thing that
does not is a tuple-struct constructor call through an alias (E0423); no wire type is normally
constructed that way. A `compile_fail` doctest with `#![deny(deprecated)]` on the module proves
the warnings fire. The module is deleted in 7.0.

### Derive attribute

`#[zbus(...)]` is the canonical helper attribute for the derives and `#[zvariant(...)]` stays
accepted, as both already are today (`zvariant_derive/src/lib.rs` registers
`attributes(zbus, zvariant)`; `zvariant_utils/src/derive/attrs.rs` parses both lists).

## Unified `zbus::Error`

One ungated `#[non_exhaustive] pub enum Error` in `zbus/src/error.rs`, with
`pub type Result<T> = std::result::Result<T, Error>` and `MaxDepthExceeded` beside it.

Variants:

- From zbus (22 → 20): all except `Variant(zvariant::Error)` and `Names(zbus_names::Error)`,
  which are flattened away.
- From zvariant (14 → 10): `IncorrectType`, `Utf8`, `PaddingNot0`, `UnknownFd`,
  `SignatureMismatch`, `OutOfBounds`, `MaxDepthExceeded`,
  `SignatureParse`, `EmptyStructure`, `InvalidObjectPath`. `InputOutput(Arc<io::Error>)`
  dedupes with zbus's identical variant. `Message(String)` — the target of serde's
  `Error::custom` — folds into zbus's `Failure(String)`: same meaning, same `Display` (`{s}`),
  and `Error::Message` would be misleading next to `zbus::Message`. `MissingFramingOffset` is
  GVariant-only and goes with the GVariant removal.
- `IncompatibleFormat` is not carried: nothing has constructed it since 2020.
- From zbus_names (2): `InvalidName(&'static str)`, `InvalidNameConversion { from, to }`. The
  seven `Invalid*Name(String)` variants deprecated since 4.1 are constructed by nothing and are
  not carried over.

Gating: exactly the variants that embed `comms`-only types get `#[cfg(feature = "comms")]` —
`MethodError(OwnedErrorName, Option<String>, Message)`, `FDO(Box<fdo::Error>)`,
`Connection(Arc<io::Error>, Address)` — together with `impl From<Message>` and
`impl From<fdo::Error>`. `InterfaceExists(InterfaceName<'static>, ObjectPath<'static>)` only
uses core types and stays ungated, as do the remaining D-Bus-flavoured unit variants (they are
inert in a wire-only build). The five hand-written matches (`PartialEq`, `Error::source`,
`Display`, `description`, `Clone`) gain three cfg arms each. Every variant keeps its current
`Display` text — except `InputOutput`, whose zvariant text gains zbus's `I/O error: ` prefix —
and compares to itself as it does today. What flattening changes is comparison
*across* the former wrappers — `Failure(s)` and the old `Variant(zvariant::Error::Message(s))`
are one value now, so they compare equal — and the `source()` chain, which loses the wrapper hop.

Impls: `From<io::Error>`, `From<zvariant_utils::signature::Error>`, `From<Infallible>`,
serde `ser::Error` and `de::Error` (`custom` → `Failure`), the inherent `description()` extended
to the new variants. `From<zvariant::Error>` and `From<zbus_names::Error>` disappear (same type).

zbus_xml's `Error::{Variant, Name}` collapse into one variant wrapping `zbus::Error`.

## Feature model

The rule: no feature → wire format and names only; any D-Bus feature → `comms`.

```toml
default = ["async-io", "blocking-api"]

# Wire-format optional impls — the former zvariant features, same names.
arrayvec = ["dep:arrayvec"]        # new to zbus
camino = ["dep:camino"]
chrono = ["dep:chrono"]
enumflags2 = ["dep:enumflags2"]    # now optional; `comms` turns it on
heapless = ["dep:heapless"]
option-as-array = []
serde_bytes = ["dep:serde_bytes"]
time = ["dep:time"]
url = ["dep:url"]
uuid = ["dep:uuid"]

# The D-Bus API (connection, message, proxy, object server, fdo, …).
comms = [
    "enumflags2", "zbus_macros/comms", "dep:uuid", "dep:serde_repr", "dep:futures-core",
    "dep:futures-lite", "dep:async-broadcast", "dep:hex", "dep:ordered-stream",
    "dep:event-listener", "dep:async-trait", "dep:tracing", "dep:async-recursion",
    "dep:rustix", "dep:libc", "dep:windows-sys", "dep:uds_windows",
]
async-io = ["comms", <today's list, unchanged>]
tokio = ["comms", "dep:tokio"]
blocking-api = ["comms", "zbus_macros/blocking-api"]
p2p = ["comms", "uuid/v4"]
bus-impl = ["p2p"]
vsock = ["dep:vsock", "async-io"]
tokio-vsock = ["dep:tokio-vsock", "tokio"]
async-fs = []                      # dummy, untouched (#1287)
```

Consequences:

- Every existing zbus manifest keeps working; all D-Bus features imply `comms`.
- `default-features = false` builds zvariant 5's dependency set: zbus_macros (in place of
  zvariant_derive), zvariant_utils, zcheapstr, endi, serde, winnow. Today's unconditional uuid,
  enumflags2, serde_repr, tracing, hex, async-trait, futures-core, futures-lite, event-listener,
  async-broadcast, ordered-stream, rustix, libc, async-recursion, windows-sys and uds_windows
  become optional behind `comms`. Target-specific optional dependencies are referenced with
  `dep:` regardless of which `[target.…]` table declares them.
- The existing implicit features for the async-io runtime deps (`async-lock`, `blocking`, …)
  are kept as they are; the only change to those lines is adding `"comms"`.
- `enumflags2` is opt-in for wire-only users (it was forced on by zbus/zbus_names before) and
  gates both the dependency and the `BitFlags` `Type`/`Value` impls. `comms` enables the `uuid`
  dependency (`guid.rs` uses it outside `p2p`) without enabling the `uuid` feature, so the `Uuid`
  wire impls stay opt-in.
- `zbus/src/lib.rs`: ungated are `pub mod wire`, `pub mod names`, `Error`/`Result`/
  `MaxDepthExceeded`, the deprecated `zvariant` module and `extern crate self as zbus`. Under
  `#[cfg(feature = "comms")]`: `win32`, `dbus_error`, `address`, `guid`, `message`, `connection`,
  `message_stream`, `abstractions`, `match_rule`, `proxy`, `object_server`, `utils`, `fdo`,
  `blocking`, `pub use zbus_macros::{DBusError, interface, proxy}`, `pub mod export`, and the
  `#[cfg(doctest)] mod doctests` block (the book and README examples need a connection).
- The "either async-io or tokio must be enabled" `compile_error!` moves under `comms`: it only
  fires when `comms` is enabled explicitly without a runtime. The vsock-only-on-Linux
  `compile_error!` is unchanged.
- zbus_macros gets a `comms` feature gating `proxy`, `interface`, `DBusError` and their modules;
  the derives are always compiled.
- `[package.metadata.docs.rs] features` gains `comms`, `arrayvec`, `enumflags2`.
- Feature unification: in a workspace where one crate wants the wire-only build and another the
  full API, everyone gets the full build. Same as `p2p`/`bus-impl` today; documented.

## Proc-macros

### Derives move into zbus_macros

The six entry points (`Type`, `Value`, `OwnedValue`, `SerializeDict`, `DeserializeDict`,
`signature!`) move to `zbus_macros/src/lib.rs` (proc-macro entry points must live at the crate
root). `attributes(zbus, zvariant)` and `attr_lists: &["zbus", "zvariant"]` carry over
unchanged. Their docs' 24 doctests are rewritten `use zvariant::` → `use zbus::wire::`; the four
`crate = "zvariant"` examples become `crate = "zbus::wire"`. `zvariant_derive/tests/tests.rs`
and `tests/no_prelude.rs` move to `zbus_macros/tests/` (`no_prelude.rs` switches to
`::zbus::wire` and stays — it is the only guard that generated code is fully `::std`-qualified).
zbus_macros already dev-depends on zbus. zbus_macros' `gvariant` pass-through feature is deleted
in the GVariant removal step.

### Crate-path resolution

In `zbus_macros/src/utils.rs` the derives' default path is `<zbus_path()>::wire`, i.e.
`crate_name("zbus")` → `FoundCrate::Name(n)` ⇒ `::n::wire`; `FoundCrate::Itself` and `Err` ⇒
`::zbus::wire`, which resolves inside zbus through the existing `extern crate self as zbus`. The
`crate_name("zvariant")` branch of `zvariant_derive/src/utils.rs` is dropped: with the crate
gone it could only bind to a stale zvariant 5 in a user's graph and emit paths to the wrong
types. `proc-macro-crate` itself stays (#1365).

The `crate` attribute keeps its current semantics. For the derives it names the module holding
the wire types — `#[zbus(crate = "mybus::wire")]`, exactly how
`zbus/tests/iface_and_proxy/types.rs` uses `crate = "zbus::zvariant"` today — because the shared
zvariant_utils codegen (also used by zgvariant with `::zgvariant`) splices that path verbatim.
For `proxy`/`interface`/`DBusError` it names the zbus crate, as today.

Two latent bugs are fixed as part of the path changes: `signature!` hardcodes `::zvariant`
(`zvariant_derive/src/lib.rs:669-672`; `zbus/src/message/header.rs:365` relies on it) and is
routed through the same resolution; `zbus_macros/src/iface.rs:543,544,583` emit a literal
`::zbus::zvariant::Value` instead of `#zbus::…`, breaking `crate = ...` for property setters
taking a `Value`. All `#zbus::zvariant::` sites in proxy/iface codegen become `#zbus::wire::`;
all `#zv::…` prefixes in the shared codegen resolve under `zbus::wire` (`Error`/`Result` via the
hidden re-export).

### Validators move to zvariant_utils 4.3.0

zbus_macros validates literal names at expansion time (`zbus_macros/src/proxy.rs:174-192`,
`iface.rs:324`) through `zbus_names::{InterfaceName, BusName}` and `zvariant::ObjectPath`. It
cannot depend on the merged zbus (cycle), so the validators move to zvariant_utils — already the
shared home of the signature parser and the only crate both sides depend on:

```rust
pub mod names {
    pub enum BusNameKind { Unique, WellKnown }
    pub fn validate_unique_name(bytes: &[u8]) -> Result<(), &'static str>;
    pub fn validate_well_known_name(bytes: &[u8]) -> Result<(), &'static str>;
    pub fn validate_bus_name(bytes: &[u8]) -> Result<BusNameKind, &'static str>;
    pub fn validate_interface_name(bytes: &[u8]) -> Result<(), &'static str>;
    pub fn validate_error_name(bytes: &[u8]) -> Result<(), &'static str>;
    pub fn validate_member_name(bytes: &[u8]) -> Result<(), &'static str>;
    pub fn validate_property_name(bytes: &[u8]) -> Result<(), &'static str>;
}
pub mod object_path {
    pub fn validate(bytes: &[u8]) -> Result<(), &'static str>;
}
```

The winnow parsers (`zbus_names/src/{unique_name,well_known_name,interface_name,member_name}.rs`
`validate_bytes`, `property_name.rs` length checks, `zvariant/src/object_path.rs:248-263`) and
their error strings move verbatim, so `zbus::names` types and `zbus::wire::ObjectPath` wrap them
with byte-identical public API and messages (`Error::InvalidName(msg)`, `Error::InvalidObjectPath`).
`BusNameKind` lets `BusName` learn which parser succeeded without parsing twice. This is additive
(4.3.0); zgvariant's `^4.1` is unaffected. zvariant_utils' three doctests that dev-depend on
zvariant are rewritten against zvariant_utils itself, avoiding a zbus ↔ zvariant_utils dev-cycle;
its description becomes "used by zbus and zgvariant".

## Source move

### Layout

`git mv zvariant/src zbus/src/wire` (lib.rs → `wire/mod.rs`) and
`git mv zbus_names/src zbus/src/names` (lib.rs → `names/mod.rs`), so history follows the files.
The `zvariant/`, `zvariant_derive/` and `zbus_names/` directories are then deleted, CHANGELOGs
included (history stays in git and on crates.io).

### Rewrites

In `wire/`: `crate::` → `crate::wire::` (196 code sites, 9 doc links) except the 79
`crate::Error`/`crate::Result` references, which now point at the root and stay as they are;
delete `extern crate self as zvariant;`; the 14 bare `zvariant::` code paths (`structure.rs`,
`tuple.rs`, `type/dynamic.rs`, `array.rs`, `file_path.rs`) → `crate::wire::`; the four `$crate::`
uses in `impl_type_with_repr!` → `$crate::wire::`; 32 doc paths → `zbus::wire::`. In `names/`:
31 `crate::` → `crate::names::`, 37 `zvariant::` → `crate::wire::`, 8 doctest
`use zbus_names::` → `use zbus::names::`. In the rest of `zbus/src`: 85 `zvariant::` and 23
`zbus_names::` references → `crate::wire::`/`crate::names::`; `Error::Variant(e)`/
`Error::Names(e)` construction and match sites are flattened (the `From` conversions become
identities, so `?` keeps working). zbus_macros' codegen and zbus_xml/zbus_xmlgen follow the same
rewrites.

### Crate-level attributes and macros

`#![cfg_attr(test, recursion_limit = "256")]` moves to `zbus/src/lib.rs` (crate-level only;
needed by wire's unit tests). `#![allow(clippy::unusual_byte_groupings)]` becomes `#[allow]` on
`mod wire`. The other zvariant/zbus_names crate attributes duplicate zbus's and are dropped. The
two `#[macro_export]` macros `impl_type_with_repr!` and `static_str_type!` necessarily land at
the zbus root; they are `#[doc(hidden)]` there and `#[doc(inline)] pub use`d from `wire`, so
`zbus::wire::impl_type_with_repr!` is the documented path. The 78 `pub(crate)` items in the
moved code are left as they are; tightening them to `pub(super)` is an optional follow-up.

### Tests, benches, fuzz

zvariant's 41 test files become one integration-test target `zbus/tests/wire.rs` with modules
under `zbus/tests/wire/` (`common.rs` becomes a real `#[macro_use] mod common`; feature-gated
files become `#[cfg(feature = …)] mod`; zvariant's `issue/` tests stay under `wire/issue/`),
so the whole suite runs with `--no-default-features`. zbus's D-Bus test, bench and example
targets get `required-features = ["comms"]`. `zvariant/benches/benchmarks.rs` →
`zbus/benches/wire.rs`, `zbus_names/benches/benchmarks.rs` → `zbus/benches/names.rs`
(CodSpeed series restart, accepted). `zvariant/fuzz/` → `zbus/fuzz/`, the same nested workspace,
depending on `zbus = { path = "..", default-features = false }`; corpus and artifacts are
gitignored, nothing to migrate.

### Manifests

Workspace members and `[workspace.dependencies]` drop the three crates. zbus_xml switches to
`zbus = { path = "../zbus", version = "6.0.0", default-features = false }`. The merge PR's first
commit sets zbus, zbus_macros, zbus_xml and zbus_xmlgen to 6.0.0 and zvariant_utils to 4.3.0,
with matching version floors on the path dependencies, so the semver-checks job is green on the
same PR. `Cargo.lock` and `zbus/fuzz/Cargo.lock` are regenerated.

## Documentation

### In-tree

- Book: every `zbus::zvariant` in code samples (client.md, faq.md, blocking.md) becomes
  `zbus::wire` — the samples are zbus doctests under `deny(warnings)`, so the deprecation would
  otherwise fail the build. Prose and the nine `docs.rs/zvariant/5/…` links (client.md, faq.md,
  introduction.md, service.md) become `zbus::wire` paths / `docs.rs/zbus/latest/zbus/wire/…`.
  introduction.md's "two crates … ## zvariant" section is rewritten around one crate with a
  wire-format core, pointing GVariant users at zgvariant.
- Root `README.md`: the crate list becomes zbus, zbus_macros, zbus_xml, zbus_xmlgen,
  zvariant_utils, plus zgvariant as a sibling project. `zbus/README.md` gains a short "wire
  format only" paragraph (`default-features = false`, `zbus::wire`).
- AGENTS.md (commands, crate list, key files, fuzz path) and CONTRIBUTING.md (package-prefix
  examples) are updated.
- Source doc comments with docs.rs/zvariant or docs.rs/zbus_names links
  (`zbus/src/message/mod.rs`, `zbus_macros/src/lib.rs`, `zbus_xml/src/{error,lib}.rs`,
  `zvariant_utils/src/signature/mod.rs`, which also has a wrong `struct.Signature` URL) switch
  to intra-doc links where the crate depends on zbus, and to `docs.rs/zbus/latest/zbus/wire/…`
  URLs in zbus_macros and zvariant_utils, which do not. `doc_build` runs with `-D warnings`, so
  broken links fail CI.

### Upgrading chapter

A new book chapter `book/src/upgrading-to-6.md` ("Upgrading to zbus 6.0"), listed in
`SUMMARY.md` and registered in zbus's `#[cfg(doctest)]` block so its before/after snippets are
compile-checked. Content:

- Cargo.toml before/after for both audiences: zbus users change nothing; zvariant-only users
  replace `zvariant = "5"` with `zbus = { version = "6", default-features = false }` (same
  feature names; `enumflags2` is now opt-in; `arrayvec` newly available).
- The path table: `zvariant::X` and `zbus::zvariant::X` → `zbus::wire::X`; `zbus_names::X` →
  `zbus::names::X`; `zvariant::Error`, `zbus_names::Error`, `zbus::Error::Variant(e)`,
  `zbus::Error::Names(e)` → `zbus::Error` (flattened variants); `#[zvariant(...)]` →
  `#[zbus(...)]` (optional).
- What still compiles with a deprecation warning, what compiles silently (traits, derives,
  paths through the module) and what does not (matching the removed wrapper variants;
  tuple-struct constructor calls through an alias).
- `cargo tree -i zvariant` to find a lingering zvariant 5 in the graph; GVariant → zgvariant.

A condensed version goes into the 6.0.0 GitHub release notes.

### Final releases of the removed crates

After zbus 6.0.0 is published, from a `5.x` branch cut at the last pre-merge commit: a commit
adding to zvariant, zvariant_derive and zbus_names a README banner (the README is each crate's
root doc) — "merged into zbus as of 6.0 — use `zbus = { version = "6", default-features = false }`
and `zbus::wire` / `zbus::names`; this crate receives no further releases" — a crates.io
description suffixed with "(deprecated: merged into zbus)" and the `deprecated` keyword.
Docs-only patch releases (5.15.1, 5.15.1, 4.3.5), no yanking. Then RustSec
`informational = "unmaintained"` advisories for the three crates — the only mechanism that
shows a banner on crates.io and reaches `cargo audit`.

## CI, release and tooling

- `rust.yml`: `MSRV` and `clippy` add `-p zbus --no-default-features`; `linux_test` and
  `windows_test` add `cargo test -p zbus --no-default-features` and the same with
  `--features arrayvec,camino,chrono,enumflags2,heapless,option-as-array,serde_bytes,time,url,uuid`;
  the two `-p zvariant --features gvariant` runs go away and the zbus + zgvariant coexistence
  check is retargeted to `-p zbus --no-default-features --features zvariant_utils/gvariant`;
  `doc_build` drops the zvariant/zbus_names steps and adds `-p zbus --no-default-features`; the
  fuzz job points at
  `zbus/fuzz` and is renamed `zbus_fuzz` (branch-protection required checks must follow).
  `semver-checks` is unchanged.
- `deploy.yml`: tag filter `zbus-5.*` → `zbus-6.*`.
- `release-plz.toml`: the `zvariant` version group is deleted; the `zbus` group stays. Versions
  are set by hand in the PR and release-plz honours a manifest version above the registry's; a
  local `release-plz update --dry-run` confirms this before the PR is opened.
- `.gitignore` drops the `zvariant/` path (`.vscode/settings.json` is untracked and only edited
  locally). commitlint, renovate and `bench.yml` need nothing.

Sequence:

1. The merge PR lands on main with the 6.0.0/4.3.0 manifests. A `5.x` branch is cut from the
   pre-merge main.
2. release-plz opens the release PR (zvariant_utils 4.3.0, then zbus_macros, zbus, zbus_xml,
   zbus_xmlgen 6.0.0); merging it publishes. zgvariant is unaffected (`^4.1` resolves to 4.3.0).
3. From the `5.x` tree: the banner commit and the final zvariant/zvariant_derive/zbus_names
   releases.
4. Outside this repo: RustSec advisories, GitHub release notes, downstream path updates (busd,
   zbus_polkit).

## Testing strategy

All with `--locked`; run locally and encoded in CI:

- Wire-only: `check`, `clippy`, `test` with `-p zbus --no-default-features` and with all ten wire
  features; `cargo doc --no-default-features -p zbus` with `-D warnings`.
- Unchanged configurations: default; `--no-default-features --features tokio`;
  `--features uuid,url,time,chrono,option-as-array,vsock,bus-impl`;
  `-p zbus --features tokio,p2p,vsock`; `--all-features` docs; cross-target `check` for
  windows-gnu, darwin, freebsd, netbsd and android, for both default and `--no-default-features`.
- Doctests: book chapters, README, the upgrading chapter, the moved zvariant/zbus_names/derive
  doctests — all under `deny(warnings)`.
- Compatibility layer: the `compile_fail` doctest with `#![deny(deprecated)]` proves the aliases
  warn; a `#![allow(deprecated)]` integration test proves the old paths compile for the common
  shapes (`use zbus::zvariant;`, `zvariant::Value::from`, a `Value::U8(..)` pattern,
  `zvariant::to_bytes`, the `zbus::names::Error` alias).
- Dependency weight: `cargo tree -e normal -p zbus --no-default-features` on the branch equals
  `cargo tree -p zvariant` on main (zbus_macros in place of zvariant_derive); `cargo tree -e
  features` shows no `comms`. The numbers go into the PR description.
- Coexistence: `-p zbus --no-default-features --features zvariant_utils/gvariant` still builds
  and `dbus_maybe_rejection.rs` still passes after the GVariant removal.
- Fuzz smoke run from `zbus/fuzz`; `release-plz update --dry-run`; every commit builds
  (`git rebase -x`), per the atomic-commit convention.
- Downstream dry run, not committed: build busd and zbus_polkit (local checkouts) against the
  branch via `[patch]` to see real migration friction and feed the upgrading chapter.

## Risks

1. Old paths mostly compile but only some warn, so users may miss the deprecation until 7.0
   removes the module. Mitigated by the upgrading chapter, the release notes, and the fact that
   `use zbus::zvariant;` — the dominant idiom — does warn.
2. A graph that still contains zvariant 5 next to zbus 6 has two `Type`/`Value` types and
   produces confusing errors. The guide says to run `cargo tree -i zvariant`; the 5.15.1 banner
   points the same way.
3. Feature unification silently upgrades a wire-only crate to the full build in a mixed
   workspace. Documented; correctness is unaffected.
4. zvariant_utils must stay 4.x while zgvariant pins `^4.1`, or `Signature` splits into two
   types. Stated as policy above.
5. CodSpeed loses history for the moved benches (run a `workflow_dispatch` backtest after the
   merge); branch protection must learn the renamed fuzz job.
6. The merge is a large diff. It is split into reviewable, individually green commits: GVariant
   removal, validators, error merge, gate, move, compatibility module, tests/benches/fuzz, docs,
   CI, versions.

## Preconditions

- zvariant 5.15.0, zvariant_derive 5.15.0 and zvariant_utils 4.2.0 are published (they are), so
  the coexistence fix from the zgvariant split is on crates.io before 6.0 removes the support.

## Follow-ups (out of scope)

- #1135: `proxy`/`service` features underneath `comms`.
- #1365: drop `proc-macro-crate`; the derive default path is already `::zbus::wire`.
- #1915: remove the deprecated API carried over by the move, and the `zbus::zvariant`
  compatibility module in 7.0.
- Tighten the moved `pub(crate)` items to `pub(super)`.
- Adopt the shared `object_path::validate` in zgvariant (it carries its own copy today).
