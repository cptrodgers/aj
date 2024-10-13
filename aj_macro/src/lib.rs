extern crate proc_macro;

use convert_case::{Case, Casing};
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, FnArg, ItemFn, Pat, PatType, ReturnType};

#[proc_macro_derive(BackgroundJob)]
pub fn background_job_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident;

    let expanded = quote! {
        impl BackgroundJob for #name {
            fn queue_name() -> &'static str {
                stringify!(#name)
            }

            fn job_builder(self) -> JobBuilder<Self> {
                let mut job_builder = JobBuilder::default();
                job_builder.data(self);

                job_builder
            }
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn job(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse the input function
    let input_fn = parse_macro_input!(item as ItemFn);
    let fn_name = &input_fn.sig.ident;
    let fn_inputs = &input_fn.sig.inputs;
    let fn_output = &input_fn.sig.output;

    // Check if the function is async
    let is_async = input_fn.sig.asyncness.is_some();

    // Generate struct name by capitalizing function name
    let job_struct_name = syn::Ident::new(
        &format!("Job{}", fn_name.to_string().to_case(Case::Pascal)),
        fn_name.span(),
    );

    // Extract input names and types
    let input_fields: Vec<_> = fn_inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(PatType { pat, ty, .. }) = arg {
                if let Pat::Ident(ident) = &**pat {
                    Some(quote! {
                        pub #ident: #ty
                    })
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // Extract the argument names to pass them later when calling the function
    let struct_as_args: Vec<_> = fn_inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(PatType { pat, .. }) = arg {
                if let Pat::Ident(ident) = &**pat {
                    Some(quote! { self.#ident })
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    let input_names: Vec<_> = fn_inputs
        .iter()
        .filter_map(|arg| {
            if let FnArg::Typed(PatType { pat, .. }) = arg {
                if let Pat::Ident(ident) = &**pat {
                    Some(quote! { #ident })
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    let fn_output_type = match fn_output {
        ReturnType::Default => quote! { () },
        ReturnType::Type(_, ty) => quote! { #ty },
    };

    let call_fn = if is_async {
        quote! {
            #fn_name(#(#struct_as_args),*).await
        }
    } else {
        quote! {
            #fn_name(#(#struct_as_args),*)
        }
    };

    let expanded = quote! {
        pub mod #fn_name {
            use aj::{BackgroundJob, JobBuilder, JobContext};

            // Define a new trait
            #[derive(
                Debug,
                Clone,
                BackgroundJob,
                aj::export::core::serde::Serialize,
                aj::export::core::serde::Deserialize,
            )]
            pub struct #job_struct_name {
                #(#input_fields),*
            }

            // Automatically implement the trait for the struct
            #[aj::async_trait]
            impl aj::Executable for #job_struct_name {
                type Output = #fn_output_type;

                async fn execute(&self, _context: &JobContext) -> Self::Output {
                    // Re-declare original function
                    #input_fn

                    #call_fn
                }
            }

            fn new(#fn_inputs) -> #job_struct_name {
                #job_struct_name {
                    #(#input_names),*
                }
            }

            pub async fn run(#fn_inputs) -> Result<String, aj::Error> {
                let msg = new(#(#input_names),*);
                let job_id = msg.job_builder().build()?.run().await?;
                Ok(job_id)
            }

            pub fn just_run(#fn_inputs) -> Result<(), aj::Error> {
                let msg = new(#(#input_names),*);
                msg.job_builder().build()?.just_run();

                Ok(())
            }
        }
    };

    TokenStream::from(expanded)
}
