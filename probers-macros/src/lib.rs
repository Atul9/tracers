extern crate proc_macro;

//We have to use the `proc_macro` types for the actual macro impl, but everywhere else we'll use
//`proc_macro2` for better testability
use probers_codegen::proc_macros::{init_provider_impl, probe_impl, prober_impl};
use proc_macro::TokenStream as CompilerTokenStream;
use proc_macro2::Span;
use proc_macro2::TokenStream;
use proc_macro_hack::proc_macro_hack;
use quote::quote_spanned;
use syn::{parse_macro_input, ItemTrait};

#[proc_macro_hack]
pub fn probe(input: CompilerTokenStream) -> CompilerTokenStream {
    match probe_impl(TokenStream::from(input)) {
        Ok(stream) => stream,
        Err(err) => report_error(&err.message, err.span),
    }
    .into()
}

#[proc_macro_hack]
pub fn init_provider(input: CompilerTokenStream) -> CompilerTokenStream {
    let input = parse_macro_input!(input as syn::TypePath);

    match init_provider_impl(input) {
        Ok(stream) => stream,
        Err(err) => report_error(&err.message, err.span),
    }
    .into()
}

#[proc_macro_attribute]
pub fn prober(_attr: CompilerTokenStream, item: CompilerTokenStream) -> CompilerTokenStream {
    // In our case this attribute can only be applied to a trait.  If it's not a trait, this line
    // will cause what looks to the user like a compile error complaining that it expected a trait.
    let input = parse_macro_input!(item as ItemTrait);

    match prober_impl(input) {
        Ok(stream) => stream,
        Err(err) => report_error(&err.message, err.span),
    }
    .into()
}

/// Reports a compile error in our macro, which is then reported to the user via the
/// `compile_error!` macro injected into the token stream.  Cool idea stolen from
/// https://internals.rust-lang.org/t/custom-error-diagnostics-with-procedural-macros-on-almost-stable-rust/8113
fn report_error(msg: &str, span: Span) -> TokenStream {
    //NB: When the unstable feature `proc_macro_diagnostic` is stabilized, use that instead of this
    //hack
    //
    //span.unwrap().error(msg).emit();
    //TokenStream::new()
    quote_spanned! {span=>
        compile_error! { #msg }
    }
}
