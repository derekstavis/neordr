//! Reuse the pinned Herdr screen detector without its process engine or updater.
//! Matching/regions/manifests are upstream code. Only manifest loading is limited
//! to bundled rules; no network fetch or Herdr user configuration is involved.
use quote::{ToTokens, quote};
use std::{env, fs, path::Path};
use syn::{Item, visit_mut::VisitMut};

fn source(path: &str) -> syn::File {
    let path = Path::new("vendor/herdr/src/detect").join(path);
    println!("cargo:rerun-if-changed={}", path.display());
    syn::parse_file(&fs::read_to_string(path).unwrap()).unwrap()
}
fn name(item: &Item) -> String {
    match item {
        Item::Fn(v) => v.sig.ident.to_string(),
        Item::Struct(v) => v.ident.to_string(),
        Item::Enum(v) => v.ident.to_string(),
        Item::Const(v) => v.ident.to_string(),
        Item::Impl(v) => v.self_ty.to_token_stream().to_string(),
        _ => String::new(),
    }
}
struct Includes;
impl VisitMut for Includes {
    fn visit_macro_mut(&mut self, m: &mut syn::Macro) {
        if m.path.is_ident("include_str") {
            let relative: syn::LitStr = syn::parse2(m.tokens.clone()).unwrap();
            let path = Path::new("vendor/herdr/src/detect").join(relative.value());
            println!("cargo:rerun-if-changed={}", path.display());
            let absolute = fs::canonicalize(path)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            m.tokens = quote!(#absolute);
        }
    }
}
pub fn generate() {
    let names = [
        "Agent",
        "AgentState",
        "AgentDetection",
        "agent_label",
        "parse_agent_label",
        "parse_canonical_agent_label",
        "lookup_agent",
        "is_muse_versioned_binary",
        "normalized_agent_lookup_name",
        "path_basename",
    ];
    let core: Vec<_> = source("mod.rs")
        .items
        .into_iter()
        .filter(|i| names.contains(&name(i).as_str()))
        .collect();
    for n in names {
        assert!(
            core.iter().any(|i| name(i) == n),
            "missing detector item {n}"
        );
    }
    let versions: Vec<_> = source("manifest_update.rs")
        .items
        .into_iter()
        .filter(|i| {
            [
                "ManifestVersion",
                "AgentRemoteStatus",
                "MANIFEST_ENGINE_VERSION",
            ]
            .contains(&name(i).as_str())
        })
        .collect();
    let mut manifest = source("manifest.rs");
    manifest
        .items
        .retain(|i| !matches!(i, Item::Mod(m) if m.ident == "tests"));
    for i in &mut manifest.items {
        if let Item::Fn(f) = i {
            match f.sig.ident.to_string().as_str() {
                "load_manifest_uncached" => {
                    f.block = syn::parse_quote!({
                        Some(bundled_loaded_manifest(
                            agent,
                            bundled_manifest(agent)?,
                            None,
                            None,
                            false,
                        ))
                    })
                }
                "read_remote_manifest" | "override_path" | "remote_update_status" => {
                    // These paths are deliberately unavailable to the adapter.
                    for arg in &mut f.sig.inputs {
                        if let syn::FnArg::Typed(a) = arg {
                            a.pat = syn::parse_quote!(_);
                        }
                    }
                    f.block = syn::parse_quote!({ None });
                }
                _ => {}
            }
        }
    }
    Includes.visit_file_mut(&mut manifest);
    let items = manifest.items;
    let file: syn::File = syn::parse2(quote! {
        #(#core)*
        mod manifest_update {
            use std::{cmp::Ordering, fmt};
            use serde::{Deserialize, Serialize};
            #(#versions)*
        }
        pub mod manifest { #(#items)* }
    })
    .unwrap();
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("detection.rs"),
        prettyplease::unparse(&file),
    )
    .unwrap();
}
