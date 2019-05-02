//! This module contains the code that transforms a trait with the `tracer` attribute into the
//! infrastructure to perform tracing using a static, platform-specific implementation.
//!
//! The generated code is about 90% identical across all possible implementations, so it's shared.
//! All static targets, including `noop`, as well as the special case `disabled` target, use this
//! module.  When there is target-specific logic, it is selected based on the `BuildInfo` in effect
//! at the time of the code generation
use crate::build_rs::BuildInfo;
use crate::gen::common::{ProbeGeneratorBase, ProviderTraitGeneratorBase};
use crate::gen::r#static::native_code::{self, ProcessedProviderTrait};
use crate::spec::ProbeSpecification;
use crate::spec::ProviderSpecification;
use crate::TracersResult;
use crate::TracingImplementation;
use crate::{TracingTarget, TracingType};
use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use std::borrow::Cow;
use syn::parse_quote;
use syn::spanned::Spanned;

pub(crate) struct ProviderTraitGenerator<'bi> {
    build_info: Cow<'bi, BuildInfo>,
    spec: ProviderSpecification,
    processed_provider: Option<ProcessedProviderTrait>,
    probes: Vec<ProbeGenerator>,
}

impl<'bi> ProviderTraitGeneratorBase for ProviderTraitGenerator<'bi> {
    fn spec(&self) -> &ProviderSpecification {
        &self.spec
    }

    fn build_info(&self) -> &BuildInfo {
        &self.build_info
    }
}

impl<'bi> ProviderTraitGenerator<'bi> {
    pub fn new(
        build_info: &'bi BuildInfo,
        spec: ProviderSpecification,
    ) -> ProviderTraitGenerator<'bi> {
        //This implementation is specific to static tracing (of which `disabled` is merely a
        //special case)
        assert!(!build_info.implementation.is_dynamic());

        let mut build_info = Cow::Borrowed(build_info);

        //Attempt to load the processed provider trait info for this trait.  That's the state
        //information left behind from `build.rs` telling us where to find the generated C wrapper
        //and the generated Rust bindings for that wrapper.  This isn't generated for all targets,
        //and if generation fails it shouldn't cause a compile error but rather it should cause us
        //to fall back to the Disabled generator for this provider
        let processed_provider = if build_info.implementation.tracing_target().is_enabled() {
            match native_code::get_processed_provider_info(&spec) {
                Err(e) => {
                    eprintln!("Warning: {}", e);

                    //This needs to override the implementation from whatever it was to disabled
                    //because the code generation was unsuccessful
                    build_info.to_mut().implementation = TracingImplementation::Disabled;

                    None
                }
                Ok(processed_provider) => Some(processed_provider),
            }
        } else {
            //Else the implementation isn't 'real' it's `Disabled` so no need to look for the
            //processed provider info
            None
        };

        //Consume this provider spec and separate out the probe specs, each of which we want to
        //wrap in our own ProbeGenerator
        let (spec, probes) = spec.separate_probes();
        let probes: Vec<_> = probes.into_iter().map(ProbeGenerator::new).collect();
        ProviderTraitGenerator {
            build_info,
            spec,
            processed_provider,
            probes,
        }
    }

    pub fn generate(&self) -> TracersResult<TokenStream> {
        // Re-generate this trait as a struct with our probing implementation in it
        let tracer_struct = self.generate_tracer_struct()?;

        // Generate a module which will `use` all of the `ProbeArgType` impls and compile-time
        // verify all probe arg types have a suitable implementaiton
        let impl_mod = self.generate_impl_mod();

        let span = self.spec.item_trait().span();
        Ok(quote_spanned! {span=>
            #tracer_struct

            #impl_mod
        })
    }

    /// A provider is described by the user as a `trait`, with methods corresponding to probes.
    /// However it's actually implemented as a `struct` with no member fields, with static methods
    /// implementing the probes.  Thus, given as input the `trait`, we produce a `struct` of the same
    /// name whose implementation actually performs the firing of the probes.
    fn generate_tracer_struct(&self) -> TracersResult<TokenStream> {
        // From the probe specifications, generate the corresponding methods that will be on the probe
        // struct.
        let mod_name = self.get_provider_impl_mod_name();
        let struct_type_name = self.get_provider_impl_struct_type_name();
        let struct_type_path: syn::Path = parse_quote! { #mod_name::#struct_type_name };
        let mut probe_methods: Vec<TokenStream> = Vec::new();
        for probe in self.probes.iter() {
            probe_methods.push(probe.generate_trait_methods(self, &struct_type_path)?);
        }

        // Re-generate the trait method that we took as input, with the modifications to support
        // probing
        // This includes constructing documentation for this trait, using whatever doc strings are already applied by
        // the user, plus a section of our own that has information about the provider and how it
        // translates into the various implementations.
        let attrs = &self.spec.item_trait().attrs;
        let span = self.spec.item_trait().span();
        let ident = &self.spec.item_trait().ident;
        let vis = &self.spec.item_trait().vis;

        let trait_doc_comment = self.generate_trait_comment();

        let try_init_decl = self.generate_try_init_decl();

        //the __try_init_provider returns a Result.  In this static implementation, we'll hard-code
        //a successful result, with a string containing some metadata about the generated provider.
        //Only dynamic implementations can actually fail to initialize, which doesn't apply here
        let provider_name = self.spec.name();

        let implementation = match self.build_info.implementation.tracing_target() {
            TracingTarget::Disabled => TracingType::Disabled.as_ref().to_string(),
            TracingTarget::NoOp | TracingTarget::Stap => format!(
                "{}/{}",
                self.build_info.implementation.tracing_type().as_ref(),
                self.build_info.implementation.as_ref()
            ),
        };
        let version = env!("CARGO_PKG_VERSION");

        let result = quote_spanned! {span=>
            #(#attrs)*
            #trait_doc_comment
            #vis struct #ident;

            impl #ident {
                #(#probe_methods)*

                #try_init_decl {
                    Ok(concat!(#provider_name, "::", #implementation, "::", #version))
                }
            }
        };

        Ok(result)
    }

    fn generate_impl_mod(&self) -> TokenStream {
        let span = self.spec.item_trait().span();
        match self.build_info.implementation.tracing_target() {
            TracingTarget::Disabled => {
                //When tracing is disabled we can't assume the `tracers::runtime` is available so
                //there is no implementation module in that case
                quote! {}
            }
            TracingTarget::NoOp => {
                //Generate a module that has some code to use our `ProbeArgType` trait to verify at
                //compile time that every probe argument has a corresponding C representation.
                let mod_name = self.get_provider_impl_mod_name();
                let struct_type_name = self.get_provider_impl_struct_type_name();

                quote_spanned! {span=>
                    mod #mod_name {
                        use tracers::runtime::ProbeArgType;

                        pub(super) struct #struct_type_name<T: ProbeArgType<T>> {
                            _t: ::std::marker::PhantomData<T>,
                        }

                        impl<T: ProbeArgType<T>> #struct_type_name<T> {
                            #[allow(dead_code)]
                            pub fn wrap(arg: T) -> <T as ProbeArgType<T>>::WrapperType {
                                ::tracers::runtime::wrap::<T>(arg)
                            }
                        }
                    }
                }
            }
            TracingTarget::Stap => {
                //The implementations which depend upon a generated C++ wrapper library work a bit
                //differently than `NoOp`.  The implementation mod will declare `extern` functions
                //for each wrapper function, and also `extern static` variables for each probe's
                //semaphore.  That's the dtrace/stap term for a 16 bit unsigned int that is
                //initially `0` and set to non-zero when a probe is enabled.  A critical part of
                //our high-performance design is the use of this semaphore to detect when a probe
                //is enabled with nothing more than a mem read.
                //
                //There is no impl struct for the real implementations
                let mod_name = self.get_provider_impl_mod_name();
                let processed_provider = self
                    .processed_provider
                    .as_ref()
                    .expect("stap requires successful codegen");
                let lib_name = processed_provider
                    .lib_path
                    .file_stem()
                    .expect("expected valid lib file name")
                    .to_str()
                    .expect("lib file name is not a valid Rust string");

                let native_declarations = self
                    .probes
                    .iter()
                    .map(|p| p.generate_native_declaration(&self));

                quote_spanned! {span=>
                    mod #mod_name {
                        #[link(name = #lib_name)]
                        extern "C" {
                            #(#native_declarations)*
                        }
                    }
                }
            }
        }
    }
}

pub(super) struct ProbeGenerator {
    spec: ProbeSpecification,
}

impl ProbeGeneratorBase for ProbeGenerator {
    fn spec(&self) -> &ProbeSpecification {
        &self.spec
    }
}

impl ProbeGenerator {
    pub fn new(spec: ProbeSpecification) -> ProbeGenerator {
        ProbeGenerator { spec }
    }

    pub fn generate_trait_methods(
        &self,
        provider: &ProviderTraitGenerator,
        struct_type_path: &syn::Path,
    ) -> TracersResult<TokenStream> {
        let vis = &self.spec.vis;
        let original_method = self.spec.original_method.sig.clone();

        let method_body = self.generate_probe_method_body(&provider, struct_type_path)?;

        //Keep the original probe method, but mark it deprecated with a helpful message so that if the
        //user calls the probe method directly they will at least be reminded that they should use the
        //macro instead.
        let deprecation_attribute = self.generate_probe_deprecation_attribute(&provider.spec);

        //Keep any attributes that were on the original method, and add `doc` attributes at the end
        //to provide some more information about the generated probe mechanics
        let attrs = &self.spec.original_method.attrs;
        let probe_doc_comment = self.generate_probe_doc_comment(&provider.spec);

        // Note that we don't put an #[allow(dead_code)] attribute on the original method, because
        // the user declared that method.  If it's not being used, let the compiler warn them about
        // it just like it would any other unused method.  The methods we generate, however, won't
        // be directly visible to the user and thus should not cause a warning if left un-called
        let span = original_method.span();
        Ok(quote_spanned! {span=>
            #(#attrs)*
            #probe_doc_comment
            #deprecation_attribute
            #vis #original_method {
                #method_body
            }
        })
    }

    fn generate_probe_method_body(
        &self,
        provider: &ProviderTraitGenerator,
        struct_type_path: &syn::Path,
    ) -> TracersResult<TokenStream> {
        let span = self.spec.original_method.span();
        // Generate the body of the original method.  This will have the same args as the trait
        // method declared by the caller, but we will provide an actual implementation.

        // * In the case of a `real` implementation, we'll wrap each arg in its `ProbeArgType` wrapper
        //   and pass to the C functions that actually fire the probe.
        //
        // * In the case of a `noop` implementation, there is no C function underneath so we call
        // the `wrap` method on the implementation struct which causes the compiler to verify there
        // is a `ProbeArgType` for each argument's type, but at runtime does not actually do
        // anything.
        //
        // * In the case of a `disabled` implementation, the function won't do anything at all.
        // We'll just assign all of the args in a `let _ = $ARGNAME` statement so that the compiler
        // doens't warn about unused arguments.
        match provider.build_info.implementation.tracing_target() {
            target @ TracingTarget::Disabled | target @ TracingTarget::NoOp => {
                //Either `noop` or `disabled`.  In both cases it's just some per-argument statements
                let args_type_assertions = self.spec.args.iter().map(|arg| {
                    let span = arg.syn_typ().span();
                    let arg_name = arg.ident();
                    if target == TracingTarget::Disabled {
                        //This is a `disabled` implementation
                        //There is no `wrap` method to do any kind of compile time type assertion,
                        //just perform a pro-forma 'use' of each arg so it doesn't trigger a warning about an
                        //unused argument
                        quote_spanned! {span=>
                            let _ = #arg_name;
                        }
                    } else {
                        //This is a `noop`  implementation
                        //Perform type assertions in the form of calls to the generated `wrap` method on the
                        //impl struct, which uses generic trickery to cause the compiler to ensure each arg
                        //type has a valid `ProbeArgType` implementation
                        quote_spanned! {span=>
                            #struct_type_path::wrap(#arg_name);
                        }
                    }
                });

                Ok(quote_spanned! {span=>
                    if false {
                        #(#args_type_assertions)*
                    }
                })
            }
            TracingTarget::Stap => {
                //This is a `real` impl with a G wrapper underneath
                //The implementation is in the impl mod, with each probe as a function named the
                //same as the original probe method declaration, but taking as arguments the C
                //version of each parameter (although obviously declared as the Rust equivalent).
                //
                //Thus, there's no practical need for this method, other than to ensure if a user
                //mis-uses the probing library and tries to call the probe method directly, it
                //actually works (but they will still get a warning as this is not a very
                //performant way to fire probes)
                let trait_name = provider.spec.ident();
                let probe_name = &self.spec.method_name;
                let args = self.spec.args.iter().map(|arg| {
                    let arg_name = arg.ident();

                    quote! { #arg_name }
                });

                Ok(quote_spanned! {span=>
                        probe!(#trait_name::#probe_name(#(#args),*))
                })
            }
        }
    }

    fn generate_native_declaration(&self, provider: &ProviderTraitGenerator) -> TokenStream {
        //Because of limitations in the tracing system, the name of the provider needs to
        //be fairly simple (no punctuation for example).  So we use the name of the trait,
        //converted to snake case.  Thus it's theoretically possible for there to be name
        //collisions.  That's why the name of the native library and the wrapper functions
        //are namespaced with a hash of the trait's source code, so if there is a
        //collision they will be disambiguated by the different implementation.  And, if
        //two crates happen to have the same exact provider trait declaration, then they'll
        //be treated as the same for tracing purposes.
        //
        //The only exception is the semaphore, because the C tracing macros make
        //assumptions about its name based on the provider and probe names.  Fortunately
        //even if there is a collission here, it won't result in any UB; it just means a
        //probe might think it's enabled when it's not, leading to a slightly inefficient
        //call into the wrapper function which will end up being a no-op
        let provider_name = provider.spec.name();
        let provider_name_with_hash = provider.spec.name_with_hash();
        let native_func_name = format!("{}_{}", provider_name_with_hash, self.spec.name);
        let func_ident = &self.spec.method_name;
        let native_semaphore_name = format!("{}_{}_semaphore", provider_name, self.spec.name);
        let semaphore_name = format!("{}_semaphore", self.spec.name);
        let semaphore_ident = syn::Ident::new(&semaphore_name, self.spec.original_method.span());

        let args = self.spec.args.iter().map(|arg| {
            let arg_name = arg.ident();
            let rust_typ = syn::Ident::new(
                arg.arg_type_info().get_rust_type_str(),
                arg.syn_typ().span(),
            );

            let span = arg.ident().span();
            quote_spanned! {span=>
                #arg_name: #rust_typ
            }
        });

        let span = self.spec.original_method.span();
        quote_spanned! {span=>
            #[link(name = #native_func_name)]
            pub fn  #func_ident( #(#args)* );

            #[link(name = #native_semaphore_name)]
            #[link_section = ".probes"]
            pub static #semaphore_ident: u16;
        }
    }
}

/// It's quite difficult to meaningfully test code generators that use the `quote` crate.  These
/// tests exercise the code with various test cases, and verify that the generator doesn't fail or
/// panic.  But they do not verify that the generated code will compile.
///
/// The integration tests and examples in the `tracers` parent crate do that.
#[cfg(test)]
mod test {
    use super::*;
    use crate::testdata;
    use crate::TracingImplementation;

    #[test]
    fn generate_works_on_valid_traits() {
        for test_case in testdata::get_test_provider_traits(|c: &testdata::TestProviderTrait| {
            c.expected_error.is_none()
        })
        .into_iter()
        {
            for implementation in vec![
                TracingImplementation::Disabled,
                TracingImplementation::StaticNoOp,
            ]
            .into_iter()
            {
                let item_trait = test_case.get_item_trait();
                let spec = ProviderSpecification::from_trait(&item_trait).expect(&format!(
                    "Failed to create specification from test trait '{}'",
                    test_case.description
                ));

                let build_info = BuildInfo::new(implementation);
                let generator = ProviderTraitGenerator::new(&build_info, spec);
                generator.generate().expect(&format!(
                    "Failed to generate test trait '{}'",
                    test_case.description
                ));
            }
        }
    }

    #[test]
    fn falls_back_to_disabled_on_error() {
        //If the native wrapper generation in `build.rs` failed, should fall back to `Disabled` no
        //matter what implementation was requested.  Since this test doesn't bother trying to
        //simulate the build-time code generation, it's guaranteed that there will be no
        //ProcessedProviderTrait for any of the provider traits, and thus the fallback logic should
        //be triggered
        for test_case in testdata::get_test_provider_traits(|c: &testdata::TestProviderTrait| {
            c.expected_error.is_none()
        })
        .into_iter()
        {
            for implementation in vec![TracingImplementation::StaticStap].into_iter() {
                let item_trait = test_case.get_item_trait();
                let spec = ProviderSpecification::from_trait(&item_trait).expect(&format!(
                    "Failed to create specification from test trait '{}'",
                    test_case.description
                ));

                let build_info = BuildInfo::new(implementation);
                let generator = ProviderTraitGenerator::new(&build_info, spec);
                assert_eq!(
                    TracingImplementation::Disabled,
                    generator.build_info.implementation
                );
                generator.generate().expect(&format!(
                    "Failed to generate test trait '{}'",
                    test_case.description
                ));
            }
        }
    }
}
