//! Compile-only fixture: a downstream crate whose target `zbus` has no blocking API, while its
//! build script depends on `zbus` with `blocking-api` enabled.
//!
//! Cargo unifies the features of host dependencies (build scripts and proc-macros) separately from
//! the target ones, so the `zbus_macros` compiled here can see a different feature set than the
//! `zbus` the generated code is compiled against. Whether a blocking proxy is emitted must
//! therefore be decided on the `zbus` side, which is what `__if_blocking_api_feature` does.
//!
//! Without that gate the blocking proxies land in a `zbus` that has no `blocking` module, and the
//! build fails with `could not find blocking in zbus` — which is what this fixture catches. With
//! `target-blocking`, the same proxies are expected to exist and are named below.
//!
//! The dependency is renamed as well, so that the gate is only reached through the `crate` path.

#![allow(dead_code)]

pub mod default_options {
    #[renamed_zbus::proxy(
        interface = "org.freedesktop.BlockingApiFixture",
        assume_defaults = true,
        crate = "renamed_zbus"
    )]
    pub trait DefaultOptions {
        fn some_method(&self) -> renamed_zbus::Result<()>;

        #[zbus(signal)]
        fn some_signal(&self, value: u32) -> renamed_zbus::Result<()>;
    }

    fn async_types(_: DefaultOptionsProxy<'_>, _: SomeSignalArgs<'_>) {}

    #[cfg(feature = "target-blocking")]
    fn blocking_types(_: DefaultOptionsProxyBlocking<'_>, _: SomeSignalIterator) {}
}

pub mod explicit_blocking {
    #[renamed_zbus::proxy(
        interface = "org.freedesktop.BlockingApiFixtureExplicit",
        assume_defaults = true,
        gen_blocking = true,
        blocking_name = "ExplicitBlocking",
        crate = "renamed_zbus"
    )]
    pub trait ExplicitBlocking {
        fn some_method(&self) -> renamed_zbus::Result<()>;
    }

    fn async_type(_: ExplicitBlockingProxy<'_>) {}

    // `gen_blocking = true` is still subject to the target's `blocking-api` feature.
    #[cfg(feature = "target-blocking")]
    fn blocking_type(_: ExplicitBlocking<'_>) {}
}

pub mod no_blocking {
    #[renamed_zbus::proxy(
        interface = "org.freedesktop.BlockingApiFixtureNoBlocking",
        assume_defaults = true,
        gen_blocking = false,
        crate = "renamed_zbus"
    )]
    pub trait NoBlocking {
        fn some_method(&self) -> renamed_zbus::Result<()>;
    }

    // Never generated, whatever the target's `blocking-api` feature is, so the name is free.
    pub struct NoBlockingProxyBlocking;

    fn async_type(_: NoBlockingProxy<'_>) {}
}

pub mod blocking_only {
    #[renamed_zbus::proxy(
        interface = "org.freedesktop.BlockingApiFixtureBlockingOnly",
        assume_defaults = true,
        gen_async = false,
        crate = "renamed_zbus"
    )]
    pub trait BlockingOnly {
        #[zbus(signal)]
        fn some_signal(&self, value: u32) -> renamed_zbus::Result<()>;
    }

    // With the target's blocking API off this proxy expands to nothing at all, which is the point:
    // a blocking-only proxy must not drag the blocking API into a target that lacks it.
    #[cfg(feature = "target-blocking")]
    fn blocking_types(_: BlockingOnlyProxy<'_>, _: SomeSignalArgs<'_>, _: SomeSignalIterator) {}
}
