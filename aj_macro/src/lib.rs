extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

#[proc_macro_derive(BackgroundJob, attributes(aj))]
pub fn background_job_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;

    let expanded = quote! {
        impl BackgroundJob for #name {
            fn queue_name() -> &'static str {
                stringify!(#name)
            }
        }
    };

    TokenStream::from(expanded)
}
