# FAQ

## How to use a struct as a dictionary?

Since the use of a dictionary, specifically one with strings as keys and variants as value (i-e
`a{sv}`) is very common in the D-Bus world and use of HashMaps isn't as convenient and type-safe as
a struct, you might find yourself wanting to use a struct as a dictionary.

We provide convenient macros for making this possible: [`SerializeDict`] and [`DeserializeDict`].\
You'll also need to tell [`Type`] macro to treat the type as a dictionary using the `signature`\
attribute. Here is a simple example:

```rust,noplayground
use zbus::{
    proxy, interface, fdo::Result,
    DeserializeDict, SerializeDict, Type,
};

#[derive(DeserializeDict, SerializeDict, Type)]
// `Type` treats `dict` is an alias for `a{sv}`.
#[zbus(signature = "dict")]
pub struct Dictionary {
    field1: u16,
    #[zbus(rename = "another-name")]
    field2: i64,
    optional_field: Option<String>,
}

#[proxy(
    interface = "org.zbus.DictionaryGiver",
    default_path = "/org/zbus/DictionaryGiver",
    default_service = "org.zbus.DictionaryGiver",
)]
trait DictionaryGiver {
    fn give_me(&self) -> Result<Dictionary>;
}

struct DictionaryGiverInterface;

#[interface(interface = "org.zbus.DictionaryGiver")]
impl DictionaryGiverInterface {
    fn give_me(&self) -> Result<Dictionary> {
        Ok(Dictionary {
            field1: 1,
            field2: 4,
            optional_field: Some(String::from("blah")),
        })
    }
}
```

## Why do async tokio API calls from interface methods not work?

Many of the tokio (and tokio-based) APIs assume the tokio runtime to be driving the async machinery
and since by default, zbus runs the `ObjectServer` in its own internal runtime thread, it's not
possible to use these APIs from interface methods. Moreover, by default zbus relies on `async-io`
crate to communicate with the bus, which uses its own thread.

Not to worry, though! You can enable tight integration between tokio and zbus by enabling `tokio`
feature:

```toml
# Sample Cargo.toml snippet.
[dependencies]
# Also disable the default `async-io` feature to avoid unused dependencies.
zbus = { version = "3", default-features = false, features = ["tokio"] }
```

**Note**: On Windows, the `async-io` feature is currently required for UNIX domain socket support,
see [the corresponding tokio issue on GitHub][tctiog].

## I'm experiencing hangs, what could be wrong?

There are typically two reasons this can happen with zbus:

### 1. A `interface` method that takes a `&mut self` argument is taking too long

Simply put, this is because of one of the primary rules of Rust: while a mutable reference to a
resource exists, no other references to that same resource can exist at the same time. This means
that before the method in question returns, all other method calls on the providing interface will
have to wait in line.

A typical solution here is use of interior mutability or launching tasks to do the actual work
combined with signals to report the progress of the work to clients. Both of these solutions
involve converting the methods in question to take `&self` argument instead of `&mut self`.

### 2. A stream (e.g `SignalStream`) is not being continuously polled

Please consult [`MessageStream`] documentation for details.

## Why aren't property values updating for my service that doesn't notify changes?

A common issue might arise when using a zbus proxy is that your proxy's property values aren't 
updating. This is due to zbus' default caching policy, which updates the value of a property only
when a change is signaled, primarily to minimize latency and optimize client request performance.
By default, if your service does not emit change notifications, the property values will not
update accordingly. 

However, you can disabling caching for specific properties:

- Add the `#[zbus(property(emits_changed_signal = "false"))]` annotation to the property for which
  you desire to disable caching on. For more information about all the possible values for
  `emits_changed_signal` refer to [`proxy`] documentation.

- Use `proxy::Builder` to build your proxy instance and use [`proxy::Builder::uncached_properties`]
  method to list all properties you wish to disable caching for.

- In order to disable caching for either type of proxy use the [`proxy::Builder::cache_properites`]
  method.

## How do I use `Option<T>`` with zbus?

While `Option<T>` is a very commonly used type in Rust, there is unfortunately [no concept of a
nullable-type in the D-Bus protocol][nonull]. However, there are two ways to simulate it:

### 1. Designation of a special value as `None`

This is the simplest way to simulate `Option<T>`. Typically the
default value for the type in question is a good choice. For example the empty string (`""`) is
often used as `None` for strings and string-based types. Note however that this solution can not
work for all types, for example `bool`.

Since this is the most widely used solution in the D-Bus world and is even used by the [D-Bus
standard interfaces][dsi], `zvariant` provides a custom type for this, [`Optional<T>`] that makes
it super easy to simulate a nullable type, especially if the contained type implements the `Default`
trait.

### 2. Encoding as an array (`a?`)

The idea here is to represent `None` case with 0 elements (empty array) and the `Some` case with 1
element. `zvariant` and `zbus` provide `option-as-array` Cargo feature, which when enabled, allows
the (de)serialization of `Option<T>`. Unlike the previous solution, this solution can be used with
all types. However, it does come with some caveats and limitations:
  1. Since the D-Bus type signature does not provide any hints about the array being in fact a
    nullable type, this can be confusing for users of generic tools like [`d-feet`]. It is therefore
    highly recommended that service authors document each use of `Option<T>` in their D-Bus
    interface documentation.
  2. Currently it is not possible to use `Option<T>` for `interface` and `proxy` property
    methods.
  3. Both the sender and receiver must agree on use of this encoding. If the sender sends `T`, the
    receiver will not be able to decode it successfully as `Option<T>` and vice versa.
  4. While `zvariant::Value` can be converted into `Option<T>`, the reverse is currently not
    possible.

Due to these limitations, `option-as-array` feature is not enabled by default and must be explicitly
enabled.

**Note**: We hope to be able to remove #2 and #4, once [specialization] lands in stable Rust.

[`proxy::Builder::uncached_properties`]: https://docs.rs/zbus/4/zbus/proxy/struct.Builder.html#method.uncached_properties
[`proxy::Builder::cache_properites`]: https://docs.rs/zbus/4/zbus/proxy/struct.Builder.html#method.cache_properties
[`proxy`]: https://docs.rs/zbus/4/zbus/attr.proxy.html
[tctiog]: https://github.com/tokio-rs/tokio/issues/2201
[`Type`]: https://docs.rs/zvariant/4/zvariant/derive.Type.html
[`SerializeDict`]: https://docs.rs/zvariant/4/zvariant/derive.SerializeDict.html
[`DeserializeDict`]: https://docs.rs/zvariant/4/zvariant/derive.DeserializeDict.html
[`MessageStream`]: https://docs.rs/zbus/4/zbus/struct.MessageStream.html
[nonull]: https://gitlab.freedesktop.org/dbus/dbus/-/issues/25
[dsi]: http://dbus.freedesktop.org/doc/dbus-specification.html#standard-interfaces
[`Optional<T>`]: https://docs.rs/zvariant/4/zvariant/struct.Optional.html
[`d-feet`]: https://wiki.gnome.org/Apps/DFeet
[specialization]: https://rust-lang.github.io/rfcs/1210-impl-specialization.html
