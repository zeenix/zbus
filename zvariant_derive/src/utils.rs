use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// The default zvariant crate path, via `proc-macro-crate` detection.
///
/// FIXME: proc-macro-crate is a hack; drop it in 6.0 (issue #1365).
pub fn zvariant_path() -> TokenStream {
    match crate_name("zbus") {
        Ok(FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote! { ::#ident::zvariant }
        }
        // `FoundCrate::Itself` is what compiling inside zbus itself reports; `::zbus` resolves
        // there through `extern crate self as zbus`.
        _ => quote! { ::zbus::zvariant },
    }
}

/// The shared-codegen configuration for zvariant_derive.
pub fn config() -> zvariant_utils::derive::Config {
    zvariant_utils::derive::Config {
        attr_lists: &["zbus", "zvariant"],
        default_path: zvariant_path(),
    }
}
