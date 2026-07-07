//! Verify that the generated code in test fixtures compiles against the current
//! zbus types. Catches regressions where xmlgen emits code that no longer
//! satisfies the trait bounds expected by the `#[proxy]` macro.

mod sample_object0 {
    use zbus::proxy;
    include!("data/sample_object0.rs");
}

mod struct_return {
    use zbus::proxy;
    include!("data/struct_return.rs");
}

mod property_setters {
    use zbus::proxy;
    include!("data/property_setters.rs");
}

mod telepathy_docstrings {
    // The generated type aliases are only referenced from the (macro-consumed) trait.
    #![allow(dead_code)]

    use zbus::proxy;
    include!("data/telepathy_docstrings.rs");
}

mod telepathy_edge_cases {
    // The generated type aliases are only referenced from the (macro-consumed) trait.
    #![allow(dead_code)]

    use zbus::proxy;
    include!("data/telepathy_edge_cases.rs");
}
