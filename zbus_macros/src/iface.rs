use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use std::collections::BTreeMap;
use syn::{
    parse::{Parse, ParseStream},
    parse_quote,
    punctuated::Punctuated,
    spanned::Spanned,
    AngleBracketedGenericArguments, Attribute, Error, Expr, ExprLit, FnArg, GenericArgument,
    ImplItem, ImplItemFn, ItemImpl,
    Lit::Str,
    Meta, MetaNameValue, PatType, PathArguments, ReturnType, Signature, Token, Type, TypePath,
    Visibility,
};
use zvariant_utils::{case, def_attrs, macros::AttrParse, old_new};

use crate::utils::*;

pub mod old {
    use super::def_attrs;
    def_attrs! {
        crate dbus_interface;

        pub TraitAttributes("trait") {
            interface str,
            name str,
            spawn bool
        };

        pub MethodAttributes("method") {
            name str,
            signal none,
            property {
                pub PropertyAttributes("property") {
                    emits_changed_signal str
                }
            },
            out_args [str]
        };
    }
}

def_attrs! {
    crate zbus;

    pub TraitAttributes("trait") {
        interface str,
        name str,
        spawn bool
    };

    pub MethodAttributes("method") {
        name str,
        signal none,
        property {
            pub PropertyAttributes("property") {
                emits_changed_signal str
            }
        },
        out_args [str]
    };

    pub ArgAttributes("argument") {
        object_server none,
        connection none,
        header none,
        signal_context none
    };
}

old_new!(TraitAttrs, old::TraitAttributes, TraitAttributes);
old_new!(MethodAttrs, old::MethodAttributes, MethodAttributes);

#[derive(Debug)]
struct Property<'a> {
    read: bool,
    write: bool,
    emits_changed_signal: PropertyEmitsChangedSignal,
    ty: Option<&'a Type>,
    doc_comments: TokenStream,
}

impl<'a> Property<'a> {
    fn new() -> Self {
        Self {
            read: false,
            write: false,
            emits_changed_signal: PropertyEmitsChangedSignal::True,
            ty: None,
            doc_comments: quote!(),
        }
    }
}

#[derive(PartialEq)]
enum MethodType {
    Signal,
    Property(PropertyType),
    Other,
}

#[derive(PartialEq)]
enum PropertyType {
    Inputs,
    NoInputs,
}

struct MethodInfo {
    /// The type of method being parsed
    method_type: MethodType,
    /// Whether the method has inputs
    has_inputs: bool,
    /// Whether the method is async
    is_async: bool,
    /// Doc comments on the methods
    doc_comments: TokenStream,
    /// Whether self is passed as mutable to the method
    is_mut: bool,
    /// The await to append to method calls
    method_await: TokenStream,
    /// The typed inputs passed to the method
    typed_inputs: Vec<PatType>,
    /// The method arguments' introspection
    intro_args: TokenStream,
    /// Whether the output type is a Result
    is_result_output: bool,
    /// Code block to deserialize arguments from zbus message
    args_from_msg: TokenStream,
    /// Names of all arguments to the method
    args_names: TokenStream,
    /// Code stream to match on the reply of the method call
    reply: TokenStream,
    /// The signal context object argument
    signal_context_arg: Option<PatType>,
    /// The name of the method (setters are stripped of set_ prefix)
    member_name: String,
}

impl MethodInfo {
    fn new(
        zbus: &TokenStream,
        method: &ImplItemFn,
        attrs: &MethodAttrs,
        cfg_attrs: &[&Attribute],
    ) -> syn::Result<MethodInfo> {
        let is_async = method.sig.asyncness.is_some();
        let Signature {
            ident,
            inputs,
            output,
            ..
        } = &method.sig;
        let docs = get_doc_attrs(&method.attrs)
            .iter()
            .filter_map(|attr| {
                if let Ok(MetaNameValue {
                    value: Expr::Lit(ExprLit { lit: Str(s), .. }),
                    ..
                }) = &attr.meta.require_name_value()
                {
                    Some(s.value())
                } else {
                    // non #[doc = "..."] attributes are not our concern
                    // we leave them for rustc to handle
                    None
                }
            })
            .collect();
        let doc_comments = to_xml_docs(docs);
        let (is_property, is_signal, out_args, attrs_name) = match attrs {
            MethodAttrs::Old(old) => (
                old.property.is_some(),
                old.signal,
                old.out_args.clone(),
                old.name.clone(),
            ),
            MethodAttrs::New(new) => (
                new.property.is_some(),
                new.signal,
                new.out_args.clone(),
                new.name.clone(),
            ),
        };
        assert!(!is_property || !is_signal);

        let has_inputs = inputs.len() > 1;

        let is_mut = if let FnArg::Receiver(r) = inputs
            .first()
            .ok_or_else(|| Error::new_spanned(ident, "not &self method"))?
        {
            r.mutability.is_some()
        } else if is_signal {
            false
        } else {
            return Err(Error::new_spanned(method, "missing receiver"));
        };
        if is_signal && !is_async {
            return Err(Error::new_spanned(method, "signals must be async"));
        }
        let method_await = if is_async {
            quote! { .await }
        } else {
            quote! {}
        };

        let mut typed_inputs = inputs
            .iter()
            .filter_map(typed_arg)
            .cloned()
            .collect::<Vec<_>>();
        let signal_context_arg: Option<PatType> = if is_signal {
            if typed_inputs.is_empty() {
                return Err(Error::new_spanned(
                    inputs,
                    "Expected a `&zbus::object_server::SignalContext<'_> argument",
                ));
            }
            Some(typed_inputs.remove(0))
        } else {
            None
        };

        let mut intro_args = quote!();
        intro_args.extend(introspect_input_args(&typed_inputs, is_signal, cfg_attrs));
        let is_result_output =
            introspect_add_output_args(&mut intro_args, output, out_args.as_deref(), cfg_attrs)?;

        let (args_from_msg, args_names) = get_args_from_inputs(&typed_inputs, zbus)?;

        let reply = if is_result_output {
            let ret = quote!(r);

            quote!(match reply {
                ::std::result::Result::Ok(r) => c.reply(m, &#ret).await,
                ::std::result::Result::Err(e) => {
                    let hdr = m.header();
                    c.reply_dbus_error(&hdr, e).await
                }
            })
        } else {
            quote!(c.reply(m, &reply).await)
        };

        let member_name = attrs_name.clone().unwrap_or_else(|| {
            let mut name = ident.to_string();
            if is_property && has_inputs {
                assert!(name.starts_with("set_"));
                name = name[4..].to_string();
            }
            pascal_case(&name)
        });

        let method_type = if is_signal {
            MethodType::Signal
        } else if is_property {
            if has_inputs {
                MethodType::Property(PropertyType::Inputs)
            } else {
                MethodType::Property(PropertyType::NoInputs)
            }
        } else {
            MethodType::Other
        };

        Ok(MethodInfo {
            method_type,
            has_inputs,
            is_async,
            doc_comments,
            is_mut,
            method_await,
            typed_inputs,
            signal_context_arg,
            intro_args,
            is_result_output,
            args_from_msg,
            args_names,
            reply,
            member_name,
        })
    }
}

pub fn expand<T: AttrParse + Into<TraitAttrs>, M: AttrParse + Into<MethodAttrs>>(
    args: Punctuated<Meta, Token![,]>,
    mut input: ItemImpl,
) -> syn::Result<TokenStream> {
    let zbus = zbus_path();

    let self_ty = &input.self_ty;
    let mut properties = BTreeMap::new();
    let mut set_dispatch = quote!();
    let mut set_mut_dispatch = quote!();
    let mut get_dispatch = quote!();
    let mut get_all = quote!();
    let mut call_dispatch = quote!();
    let mut call_mut_dispatch = quote!();
    let mut introspect = quote!();
    let mut generated_signals = quote!();

    // the impl Type
    let ty = match input.self_ty.as_ref() {
        Type::Path(p) => {
            &p.path
                .segments
                .last()
                .ok_or_else(|| Error::new_spanned(p, "Unsupported 'impl' type"))?
                .ident
        }
        _ => return Err(Error::new_spanned(&input.self_ty, "Invalid type")),
    };

    let (iface_name, with_spawn) = {
        let (name, interface, spawn) = match T::parse_nested_metas(args)?.into() {
            TraitAttrs::New(new) => (new.name, new.interface, new.spawn),
            TraitAttrs::Old(old) => (old.name, old.interface, old.spawn),
        };

        let name =
            match (name, interface) {
                (Some(name), None) | (None, Some(name)) => name,
                (None, None) => format!("org.freedesktop.{ty}"),
                (Some(_), Some(_)) => return Err(syn::Error::new(
                    input.span(),
                    "`name` and `interface` attributes should not be specified at the same time",
                )),
            };

        (name, !spawn.unwrap_or(false))
    };

    // Store parsed information about each method
    let mut methods = vec![];
    for item in &mut input.items {
        let (method, is_signal) = match item {
            ImplItem::Fn(m) => (m, false),
            // Since signals do not have a function body, they don't parse as ImplItemFn…
            ImplItem::Verbatim(tokens) => {
                // … thus parse them ourselves and construct an ImplItemFn from that
                let decl = syn::parse2::<ImplItemSignal>(tokens.clone())?;
                let ImplItemSignal { attrs, vis, sig } = decl;
                *item = ImplItem::Fn(ImplItemFn {
                    attrs,
                    vis,
                    defaultness: None,
                    sig,
                    // This empty block will be replaced below.
                    block: parse_quote!({}),
                });
                match item {
                    ImplItem::Fn(m) => (m, true),
                    _ => unreachable!(),
                }
            }
            _ => continue,
        };

        let attrs = M::parse(&method.attrs)?.into();

        method.attrs.retain(|attr| {
            !attr.path().is_ident("zbus") && !attr.path().is_ident("dbus_interface")
        });

        if is_signal
            && !matches!(&attrs, MethodAttrs::Old(attrs) if attrs.signal)
            && !matches!(&attrs, MethodAttrs::New(attrs) if attrs.signal)
        {
            return Err(syn::Error::new_spanned(
                item,
                "methods that are not signals must have a body",
            ));
        }

        let cfg_attrs: Vec<_> = method
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("cfg"))
            .collect();

        let method_info = MethodInfo::new(&zbus, method, &attrs, &cfg_attrs)?;
        let attr_property = match attrs {
            MethodAttrs::Old(o) => o.property.map(|op| PropertyAttributes {
                emits_changed_signal: op.emits_changed_signal,
            }),
            MethodAttrs::New(n) => n.property,
        };
        if let Some(prop_attrs) = &attr_property {
            if method_info.method_type == MethodType::Property(PropertyType::NoInputs) {
                let emits_changed_signal = if let Some(s) = &prop_attrs.emits_changed_signal {
                    PropertyEmitsChangedSignal::parse(s, method.span())?
                } else {
                    PropertyEmitsChangedSignal::True
                };
                let mut property = Property::new();
                property.emits_changed_signal = emits_changed_signal;
                properties.insert(method_info.member_name.to_string(), property);
            } else if prop_attrs.emits_changed_signal.is_some() {
                return Err(syn::Error::new(
                    method.span(),
                    "`emits_changed_signal` cannot be specified on setters",
                ));
            }
        };
        methods.push((method, method_info));
    }

    for (method, method_info) in methods {
        let cfg_attrs: Vec<_> = method
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("cfg"))
            .collect();

        let MethodInfo {
            method_type,
            has_inputs,
            is_async,
            doc_comments,
            is_mut,
            method_await,
            typed_inputs,
            signal_context_arg,
            intro_args,
            is_result_output,
            args_from_msg,
            args_names,
            reply,
            member_name,
        } = method_info;

        let Signature {
            ident,
            inputs,
            output,
            ..
        } = &mut method.sig;

        clean_input_args(inputs);

        match method_type {
            MethodType::Signal => {
                introspect.extend(doc_comments);
                introspect.extend(introspect_signal(&member_name, &intro_args));
                let signal_context = signal_context_arg.unwrap().pat;

                method.block = parse_quote!({
                    #signal_context.connection().emit_signal(
                        #signal_context.destination(),
                        #signal_context.path(),
                        <#self_ty as #zbus::object_server::Interface>::name(),
                        #member_name,
                        &(#args_names),
                    )
                    .await
                });
            }
            MethodType::Property(_) => {
                let p = properties.get_mut(&member_name).ok_or(Error::new_spanned(
                    &member_name,
                    "Write-only properties aren't supported yet",
                ))?;

                let sk_member_name = case::snake_case(&member_name);
                let prop_changed_method_name = format_ident!("{sk_member_name}_changed");
                let prop_invalidate_method_name = format_ident!("{sk_member_name}_invalidate");

                p.doc_comments.extend(doc_comments);
                if has_inputs {
                    p.write = true;

                    let set_call = if is_result_output {
                        quote!(self.#ident(val)#method_await)
                    } else if is_async {
                        quote!(
                                #zbus::export::futures_util::future::FutureExt::map(
                                    self.#ident(val),
                                    ::std::result::Result::Ok,
                                )
                                .await
                        )
                    } else {
                        quote!(::std::result::Result::Ok(self.#ident(val)))
                    };

                    // * For reference arg, we convert from `&Value` (so `TryFrom<&Value<'_>>` is
                    //   required).
                    //
                    // * For argument type with lifetimes, we convert from `Value` (so
                    //   `TryFrom<Value<'_>>` is required).
                    //
                    // * For all other arg types, we convert the passed value to `OwnedValue` first
                    //   and then pass it as `Value` (so `TryFrom<OwnedValue>` is required).
                    let value_to_owned = quote! {
                        match ::zbus::Value::try_to_owned(value) {
                            ::std::result::Result::Ok(val) => ::zbus::Value::from(val),
                            ::std::result::Result::Err(e) => {
                                return ::std::result::Result::Err(
                                    ::std::convert::Into::into(#zbus::Error::Variant(::std::convert::Into::into(e)))
                                );
                            }
                        }
                    };
                    let value_arg = match &*typed_inputs
                    .first()
                    .ok_or_else(|| Error::new_spanned(&inputs, "Expected a value argument"))?
                    .ty
                {
                    Type::Reference(_) => quote!(value),
                    Type::Path(path) => path
                        .path
                        .segments
                        .first()
                        .map(|segment| match &segment.arguments {
                            PathArguments::AngleBracketed(angled) => angled
                                .args
                                .first()
                                .filter(|arg| matches!(arg, GenericArgument::Lifetime(_)))
                                .map(|_| quote!(match ::zbus::Value::try_clone(value) {
                                    ::std::result::Result::Ok(val) => val,
                                    ::std::result::Result::Err(e) => {
                                        return ::std::result::Result::Err(
                                            ::std::convert::Into::into(#zbus::Error::Variant(::std::convert::Into::into(e)))
                                        );
                                    }
                                }))
                                .unwrap_or_else(|| value_to_owned.clone()),
                            _ => value_to_owned.clone(),
                        })
                        .unwrap_or_else(|| value_to_owned.clone()),
                    _ => value_to_owned,
                };
                    let prop_changed_method = match p.emits_changed_signal {
                        PropertyEmitsChangedSignal::True => {
                            quote!({
                                self
                                    .#prop_changed_method_name(&signal_context)
                                    .await
                                    .map(|_| set_result)
                                    .map_err(Into::into)
                            })
                        }
                        PropertyEmitsChangedSignal::Invalidates => {
                            quote!({
                                self
                                    .#prop_invalidate_method_name(&signal_context)
                                    .await
                                    .map(|_| set_result)
                                    .map_err(Into::into)
                            })
                        }
                        PropertyEmitsChangedSignal::False | PropertyEmitsChangedSignal::Const => {
                            quote!({ Ok(()) })
                        }
                    };
                    let do_set = quote!({
                        let value = #value_arg;
                        match ::std::convert::TryInto::try_into(value) {
                            ::std::result::Result::Ok(val) => {
                                match #set_call {
                                    ::std::result::Result::Ok(set_result) => #prop_changed_method
                                    e => e,
                                }
                            }
                            ::std::result::Result::Err(e) => {
                                ::std::result::Result::Err(
                                    ::std::convert::Into::into(#zbus::Error::Variant(::std::convert::Into::into(e))),
                                )
                            }
                        }
                    });

                    if is_mut {
                        let q = quote!(
                            #(#cfg_attrs)*
                            #member_name => {
                                ::std::option::Option::Some((move || async move { #do_set }) ().await)
                            }
                        );
                        set_mut_dispatch.extend(q);

                        let q = quote!(
                            #(#cfg_attrs)*
                            #member_name => #zbus::object_server::DispatchResult::RequiresMut,
                        );
                        set_dispatch.extend(q);
                    } else {
                        let q = quote!(
                            #(#cfg_attrs)*
                            #member_name => {
                                #zbus::object_server::DispatchResult::Async(::std::boxed::Box::pin(async move {
                                    #do_set
                                }))
                            }
                        );
                        set_dispatch.extend(q);
                    }
                } else {
                    let is_fallible_property = is_result_output;

                    p.ty = Some(get_property_type(output)?);
                    p.read = true;
                    let value_convert = quote!(
                        <#zbus::OwnedValue as ::std::convert::TryFrom<_>>::try_from(
                            <#zbus::Value as ::std::convert::From<_>>::from(
                                value,
                            ),
                        )
                        .map_err(|e| #zbus::fdo::Error::Failed(e.to_string()))
                    );
                    let inner = if is_fallible_property {
                        quote!(self.#ident() #method_await .and_then(|value| #value_convert))
                    } else {
                        quote!({
                            let value = self.#ident()#method_await;
                            #value_convert
                        })
                    };

                    let q = quote!(
                        #(#cfg_attrs)*
                        #member_name => {
                            ::std::option::Option::Some(#inner)
                        },
                    );
                    get_dispatch.extend(q);

                    let q = if is_fallible_property {
                        quote!(if let Ok(prop) = self.#ident()#method_await {
                            props.insert(
                                ::std::string::ToString::to_string(#member_name),
                                <#zbus::OwnedValue as ::std::convert::TryFrom<_>>::try_from(
                                    <#zbus::Value as ::std::convert::From<_>>::from(
                                        prop,
                                    ),
                                )
                                .map_err(|e| #zbus::fdo::Error::Failed(e.to_string()))?,
                            );
                        })
                    } else {
                        quote!(props.insert(
                        ::std::string::ToString::to_string(#member_name),
                        <#zbus::OwnedValue as ::std::convert::TryFrom<_>>::try_from(
                            <#zbus::Value as ::std::convert::From<_>>::from(
                                self.#ident()#method_await,
                            ),
                        )
                        .map_err(|e| #zbus::fdo::Error::Failed(e.to_string()))?,
                    );)
                    };

                    get_all.extend(q);

                    let prop_value_handled = if is_fallible_property {
                        quote!(self.#ident()#method_await?)
                    } else {
                        quote!(self.#ident()#method_await)
                    };

                    let prop_changed_method = quote!(
                        pub async fn #prop_changed_method_name(
                            &self,
                            signal_context: &#zbus::object_server::SignalContext<'_>,
                        ) -> #zbus::Result<()> {
                            let mut changed = ::std::collections::HashMap::new();
                            let value = <#zbus::Value as ::std::convert::From<_>>::from(#prop_value_handled);
                            changed.insert(#member_name, &value);
                            #zbus::fdo::Properties::properties_changed(
                                signal_context,
                                #zbus::names::InterfaceName::from_static_str_unchecked(#iface_name),
                                &changed,
                                &[],
                            ).await
                        }
                    );

                    generated_signals.extend(prop_changed_method);

                    let prop_invalidate_method = quote!(
                        pub async fn #prop_invalidate_method_name(
                            &self,
                            signal_context: &#zbus::object_server::SignalContext<'_>,
                        ) -> #zbus::Result<()> {
                            #zbus::fdo::Properties::properties_changed(
                                signal_context,
                                #zbus::names::InterfaceName::from_static_str_unchecked(#iface_name),
                                &::std::collections::HashMap::new(),
                                &[#member_name],
                            ).await
                        }
                    );

                    generated_signals.extend(prop_invalidate_method);
                }
            }
            MethodType::Other => {
                introspect.extend(doc_comments);
                introspect.extend(introspect_method(&member_name, &intro_args));

                let m = quote! {
                    #(#cfg_attrs)*
                    #member_name => {
                        let future = async move {
                            #args_from_msg
                            let reply = self.#ident(#args_names)#method_await;
                            #reply
                        };
                        #zbus::object_server::DispatchResult::Async(::std::boxed::Box::pin(async move {
                            future.await
                        }))
                    },
                };

                if is_mut {
                    call_dispatch.extend(quote! {
                        #(#cfg_attrs)*
                        #member_name => #zbus::object_server::DispatchResult::RequiresMut,
                    });
                    call_mut_dispatch.extend(m);
                } else {
                    call_dispatch.extend(m);
                }
            }
        }
    }

    introspect_properties(&mut introspect, properties)?;

    let generics = &input.generics;
    let where_clause = &generics.where_clause;

    let generated_signals_impl = if generated_signals.is_empty() {
        quote!()
    } else {
        quote! {
            impl #generics #self_ty
            #where_clause
            {
                #generated_signals
            }
        }
    };

    Ok(quote! {
        #input

        #generated_signals_impl

        #[#zbus::export::async_trait::async_trait]
        impl #generics #zbus::object_server::Interface for #self_ty
        #where_clause
        {
            fn name() -> #zbus::names::InterfaceName<'static> {
                #zbus::names::InterfaceName::from_static_str_unchecked(#iface_name)
            }

            fn spawn_tasks_for_methods(&self) -> bool {
                #with_spawn
            }

            async fn get(
                &self,
                property_name: &str,
            ) -> ::std::option::Option<#zbus::fdo::Result<#zbus::OwnedValue>> {
                match property_name {
                    #get_dispatch
                    _ => ::std::option::Option::None,
                }
            }

            async fn get_all(
                &self,
            ) -> #zbus::fdo::Result<::std::collections::HashMap<
                ::std::string::String,
                #zbus::OwnedValue,
            >> {
                let mut props: ::std::collections::HashMap<
                    ::std::string::String,
                    #zbus::OwnedValue,
                > = ::std::collections::HashMap::new();
                #get_all
                Ok(props)
            }

            fn set<'call>(
                &'call self,
                property_name: &'call str,
                value: &'call #zbus::Value<'_>,
                signal_context: &'call #zbus::object_server::SignalContext<'_>,
            ) -> #zbus::object_server::DispatchResult<'call> {
                match property_name {
                    #set_dispatch
                    _ => #zbus::object_server::DispatchResult::NotFound,
                }
            }

            async fn set_mut(
                &mut self,
                property_name: &str,
                value: &#zbus::Value<'_>,
                signal_context: &#zbus::object_server::SignalContext<'_>,
            ) -> ::std::option::Option<#zbus::fdo::Result<()>> {
                match property_name {
                    #set_mut_dispatch
                    _ => ::std::option::Option::None,
                }
            }

            fn call<'call>(
                &'call self,
                s: &'call #zbus::ObjectServer,
                c: &'call #zbus::Connection,
                m: &'call #zbus::message::Message,
                name: #zbus::names::MemberName<'call>,
            ) -> #zbus::object_server::DispatchResult<'call> {
                match name.as_str() {
                    #call_dispatch
                    _ => #zbus::object_server::DispatchResult::NotFound,
                }
            }

            fn call_mut<'call>(
                &'call mut self,
                s: &'call #zbus::ObjectServer,
                c: &'call #zbus::Connection,
                m: &'call #zbus::message::Message,
                name: #zbus::names::MemberName<'call>,
            ) -> #zbus::object_server::DispatchResult<'call> {
                match name.as_str() {
                    #call_mut_dispatch
                    _ => #zbus::object_server::DispatchResult::NotFound,
                }
            }

            fn introspect_to_writer(&self, writer: &mut dyn ::std::fmt::Write, level: usize) {
                ::std::writeln!(
                    writer,
                    r#"{:indent$}<interface name="{}">"#,
                    "",
                    <Self as #zbus::object_server::Interface>::name(),
                    indent = level
                ).unwrap();
                {
                    use #zbus::Type;

                    let level = level + 2;
                    #introspect
                }
                ::std::writeln!(writer, r#"{:indent$}</interface>"#, "", indent = level).unwrap();
            }
        }
    })
}

fn get_args_from_inputs(
    inputs: &[PatType],
    zbus: &TokenStream,
) -> syn::Result<(TokenStream, TokenStream)> {
    if inputs.is_empty() {
        Ok((quote!(), quote!()))
    } else {
        let mut server_arg_decl = None;
        let mut conn_arg_decl = None;
        let mut header_arg_decl = None;
        let mut signal_context_arg_decl = None;
        let mut args_names = Vec::new();
        let mut tys = Vec::new();

        for input in inputs {
            let ArgAttributes {
                object_server,
                connection,
                header,
                signal_context,
            } = ArgAttributes::parse(&input.attrs)?;

            if object_server {
                if server_arg_decl.is_some() {
                    return Err(Error::new_spanned(
                        input,
                        "There can only be one object_server argument",
                    ));
                }

                let server_arg = &input.pat;
                server_arg_decl = Some(quote! { let #server_arg = &s; });
            } else if connection {
                if conn_arg_decl.is_some() {
                    return Err(Error::new_spanned(
                        input,
                        "There can only be one connection argument",
                    ));
                }

                let conn_arg = &input.pat;
                conn_arg_decl = Some(quote! { let #conn_arg = &c; });
            } else if header {
                if header_arg_decl.is_some() {
                    return Err(Error::new_spanned(
                        input,
                        "There can only be one header argument",
                    ));
                }

                let header_arg = &input.pat;

                header_arg_decl = Some(quote! {
                    let #header_arg = m.header();
                });
            } else if signal_context {
                if signal_context_arg_decl.is_some() {
                    return Err(Error::new_spanned(
                        input,
                        "There can only be one `signal_context` argument",
                    ));
                }

                let signal_context_arg = &input.pat;

                signal_context_arg_decl = Some(quote! {
                    let #signal_context_arg = match hdr.path() {
                        ::std::option::Option::Some(p) => {
                            #zbus::object_server::SignalContext::new(c, p).expect("Infallible conversion failed")
                        }
                        ::std::option::Option::None => {
                            let err = #zbus::fdo::Error::UnknownObject("Path Required".into());
                            return c.reply_dbus_error(&hdr, err).await;
                        }
                    };
                });
            } else {
                args_names.push(pat_ident(input).unwrap());
                tys.push(&input.ty);
            }
        }

        let args_from_msg = quote! {
            let hdr = m.header();
            let msg_body = m.body();

            #server_arg_decl

            #conn_arg_decl

            #header_arg_decl

            #signal_context_arg_decl

            let (#(#args_names),*): (#(#tys),*) =
                match msg_body.deserialize() {
                    ::std::result::Result::Ok(r) => r,
                    ::std::result::Result::Err(e) => {
                        let err = <#zbus::fdo::Error as ::std::convert::From<_>>::from(e);
                        return c.reply_dbus_error(&hdr, err).await;
                    }
                };
        };

        let all_args_names = inputs.iter().filter_map(pat_ident);
        let all_args_names = quote! { #(#all_args_names,)* };

        Ok((args_from_msg, all_args_names))
    }
}

fn clean_input_args(inputs: &mut Punctuated<FnArg, Token![,]>) {
    for input in inputs {
        if let FnArg::Typed(t) = input {
            t.attrs.retain(|attr| {
                !attr.path().is_ident("zbus") && !attr.path().is_ident("dbus_interface")
            });
        }
    }
}

fn introspect_signal(name: &str, args: &TokenStream) -> TokenStream {
    quote!(
        ::std::writeln!(writer, "{:indent$}<signal name=\"{}\">", "", #name, indent = level).unwrap();
        {
            let level = level + 2;
            #args
        }
        ::std::writeln!(writer, "{:indent$}</signal>", "", indent = level).unwrap();
    )
}

fn introspect_method(name: &str, args: &TokenStream) -> TokenStream {
    quote!(
        ::std::writeln!(writer, "{:indent$}<method name=\"{}\">", "", #name, indent = level).unwrap();
        {
            let level = level + 2;
            #args
        }
        ::std::writeln!(writer, "{:indent$}</method>", "", indent = level).unwrap();
    )
}

fn introspect_input_args<'i>(
    inputs: &'i [PatType],
    is_signal: bool,
    cfg_attrs: &'i [&'i syn::Attribute],
) -> impl Iterator<Item = TokenStream> + 'i {
    inputs
        .iter()
        .filter_map(move |pat_type @ PatType { ty, attrs, .. }| {
            let is_special_arg = attrs.iter().any(|attr| {
                if !attr.path().is_ident("zbus") && !attr.path().is_ident("dbus_interface") {
                    return false;
                }

                let Ok(list) = &attr.meta.require_list()  else {
                     return false;
                };
                let Ok(nested) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) else {
                    return false;
                };

                let res = nested.iter().any(|nested_meta| {
                    matches!(
                        nested_meta,
                        Meta::Path(path)
                        if path.is_ident("object_server") || path.is_ident("connection") || path.is_ident("header") || path.is_ident("signal_context")
                    )
                });

                res
            });
            if is_special_arg {
                return None;
            }

            let ident = pat_ident(pat_type).unwrap();
            let arg_name = quote!(#ident).to_string();
            let dir = if is_signal { "" } else { " direction=\"in\"" };
            Some(quote!(
                #(#cfg_attrs)*
                ::std::writeln!(writer, "{:indent$}<arg name=\"{}\" type=\"{}\"{}/>", "",
                         #arg_name, <#ty>::signature(), #dir, indent = level).unwrap();
            ))
        })
}

fn introspect_output_arg(
    ty: &Type,
    arg_name: Option<&String>,
    cfg_attrs: &[&syn::Attribute],
) -> TokenStream {
    let arg_name = match arg_name {
        Some(name) => format!("name=\"{name}\" "),
        None => String::from(""),
    };

    quote!(
        #(#cfg_attrs)*
        ::std::writeln!(writer, "{:indent$}<arg {}type=\"{}\" direction=\"out\"/>", "",
                 #arg_name, <#ty>::signature(), indent = level).unwrap();
    )
}

fn get_result_type(p: &TypePath) -> syn::Result<&Type> {
    if let PathArguments::AngleBracketed(AngleBracketedGenericArguments { args, .. }) = &p
        .path
        .segments
        .last()
        .ok_or_else(|| Error::new_spanned(p, "unsupported result type"))?
        .arguments
    {
        if let Some(syn::GenericArgument::Type(ty)) = args.first() {
            return Ok(ty);
        }
    }

    Err(Error::new_spanned(p, "unhandled Result return"))
}

fn introspect_add_output_args(
    args: &mut TokenStream,
    output: &ReturnType,
    arg_names: Option<&[String]>,
    cfg_attrs: &[&syn::Attribute],
) -> syn::Result<bool> {
    let mut is_result_output = false;

    if let ReturnType::Type(_, ty) = output {
        let mut ty = ty.as_ref();

        if let Type::Path(p) = ty {
            is_result_output = p
                .path
                .segments
                .last()
                .ok_or_else(|| Error::new_spanned(ty, "unsupported output type"))?
                .ident
                == "Result";
            if is_result_output {
                ty = get_result_type(p)?;
            }
        }

        if let Type::Tuple(t) = ty {
            if let Some(arg_names) = arg_names {
                if t.elems.len() != arg_names.len() {
                    // Turn into error
                    panic!("Number of out arg names different from out args specified")
                }
            }
            for i in 0..t.elems.len() {
                let name = arg_names.map(|names| &names[i]);
                args.extend(introspect_output_arg(&t.elems[i], name, cfg_attrs));
            }
        } else {
            args.extend(introspect_output_arg(ty, None, cfg_attrs));
        }
    }

    Ok(is_result_output)
}

fn get_property_type(output: &ReturnType) -> syn::Result<&Type> {
    if let ReturnType::Type(_, ty) = output {
        let ty = ty.as_ref();

        if let Type::Path(p) = ty {
            let is_result_output = p
                .path
                .segments
                .last()
                .ok_or_else(|| Error::new_spanned(ty, "unsupported property type"))?
                .ident
                == "Result";
            if is_result_output {
                return get_result_type(p);
            }
        }

        Ok(ty)
    } else {
        Err(Error::new_spanned(output, "Invalid property getter"))
    }
}

fn introspect_properties(
    introspection: &mut TokenStream,
    properties: BTreeMap<String, Property<'_>>,
) -> syn::Result<()> {
    for (name, prop) in properties {
        let access = if prop.read && prop.write {
            "readwrite"
        } else if prop.read {
            "read"
        } else if prop.write {
            "write"
        } else {
            return Err(Error::new_spanned(
                name,
                "property is neither readable nor writable",
            ));
        };
        let ty = prop.ty.ok_or_else(|| {
            Error::new_spanned(&name, "Write-only properties aren't supported yet")
        })?;

        let doc_comments = prop.doc_comments;
        if prop.emits_changed_signal == PropertyEmitsChangedSignal::True {
            introspection.extend(quote!(
                #doc_comments
                ::std::writeln!(
                    writer,
                    "{:indent$}<property name=\"{}\" type=\"{}\" access=\"{}\"/>",
                    "", #name, <#ty>::signature(), #access, indent = level,
                ).unwrap();
            ));
        } else {
            let emits_changed_signal = prop.emits_changed_signal.to_string();
            introspection.extend(quote!(
                #doc_comments
                ::std::writeln!(
                    writer,
                    "{:indent$}<property name=\"{}\" type=\"{}\" access=\"{}\">",
                    "", #name, <#ty>::signature(), #access, indent = level,
                ).unwrap();
                ::std::writeln!(
                    writer,
                    "{:indent$}<annotation name=\"org.freedesktop.DBus.Property.EmitsChangedSignal\" value=\"{}\"/>",
                    "", #emits_changed_signal, indent = level + 2,
                ).unwrap();
                ::std::writeln!(
                    writer,
                    "{:indent$}</property>", "", indent = level,
                ).unwrap();
            ));
        }
    }

    Ok(())
}

pub fn to_xml_docs(lines: Vec<String>) -> TokenStream {
    let mut docs = quote!();

    let mut lines: Vec<&str> = lines
        .iter()
        .skip_while(|s| is_blank(s))
        .flat_map(|s| s.split('\n'))
        .collect();

    while let Some(true) = lines.last().map(|s| is_blank(s)) {
        lines.pop();
    }

    if lines.is_empty() {
        return docs;
    }

    docs.extend(quote!(::std::writeln!(writer, "{:indent$}<!--", "", indent = level).unwrap();));
    for line in lines {
        if !line.is_empty() {
            docs.extend(
                quote!(::std::writeln!(writer, "{:indent$}{}", "", #line, indent = level).unwrap();),
            );
        } else {
            docs.extend(quote!(::std::writeln!(writer, "").unwrap();));
        }
    }
    docs.extend(quote!(::std::writeln!(writer, "{:indent$} -->", "", indent = level).unwrap();));

    docs
}

// Like ImplItemFn, but with a semicolon at the end instead of a body block
struct ImplItemSignal {
    attrs: Vec<Attribute>,
    vis: Visibility,
    sig: Signature,
}

impl Parse for ImplItemSignal {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let vis = input.parse()?;
        let sig = input.parse()?;
        let _: Token![;] = input.parse()?;

        Ok(ImplItemSignal { attrs, vis, sig })
    }
}
