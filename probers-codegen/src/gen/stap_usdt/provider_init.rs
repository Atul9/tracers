//! Generates code to explicitly initialize a provider at runtime

use crate::probe;
use crate::probe_call::ProbeCall;
use crate::provider;
use crate::provider::ProviderSpecification;
use crate::provider_init::ProviderInitSpecification;
use crate::{ProberError, ProberResult};
use heck::{ShoutySnakeCase, SnakeCase};
use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use std::borrow::BorrowMut;
use std::fmt::Display;
use syn::parse_quote;
use syn::spanned::Spanned;
use syn::{Ident, ItemTrait};

pub(super) fn generate_provider_init(init: ProviderInitSpecification) -> ProberResult<TokenStream> {
    //This couldn't be simpler.  We must assume the caller provided a valid provider trait.  If
    //they didn't this will fail at compile time in a fairly obvious way.
    //
    //So we just generate code to call the init function that the provider trait generator will
    //have already generated on the trait itself.
    let provider = init.provider;
    let span = provider.span();
    Ok(quote_spanned! {span=>
        #provider::__try_init_provider()
    })
}