//! Extract wire contracts from the pinned, unmodified Herdr source tree.
//!
//! Types and serde behavior are upstream code, not an independently maintained
//! schema. Runtime-only conversions are omitted; rustc resolves every retained
//! field and helper, so missing dependencies fail the build rather than degrade
//! to untyped JSON.
use quote::{ToTokens, quote};
use std::{env, fs, path::Path, process::Command};
use syn::{Item, visit_mut::VisitMut};

const REVISION: &str = "28360107c1bfab57fb0de99d74bfa48e74423ca8";
const ROOT: &str = "vendor/herdr";

fn source(path: &str) -> syn::File {
    let path = Path::new(ROOT).join(path);
    println!("cargo:rerun-if-changed={}", path.display());
    syn::parse_file(&fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}; run git submodule update --init vendor/herdr",
            path.display()
        )
    }))
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn is_test(item: &Item) -> bool {
    matches!(item, Item::Mod(m) if m.attrs.iter().any(|a| a.path().is_ident("cfg") && a.meta.to_token_stream().to_string().contains("test")))
}

/// Preserve schema files wholesale, expanding their module tree into OUT_DIR.
fn api(path: &str) -> Vec<Item> {
    source(path)
        .items
        .into_iter()
        .filter(|i| !is_test(i))
        .map(|mut item| {
            if let Item::Mod(m) = &mut item {
                assert!(m.content.is_none(), "unexpected inline schema module");
                m.content = Some((
                    Default::default(),
                    api(&format!("src/api/schema/{}.rs", m.ident)),
                ));
                m.semi = None;
            }
            item
        })
        .collect()
}

fn named_type(item: &Item) -> Option<String> {
    match item {
        Item::Struct(s) => Some(s.ident.to_string()),
        Item::Enum(e) => Some(e.ident.to_string()),
        Item::Type(t) => Some(t.ident.to_string()),
        _ => None,
    }
}

fn supporting_types(path: &str, names: &[&str]) -> Vec<Item> {
    let items: Vec<_> = source(path)
        .items
        .into_iter()
        .filter(|i| named_type(i).is_some_and(|n| names.contains(&n.as_str())))
        .collect();
    assert_eq!(
        items.len(),
        names.len(),
        "missing supporting contract in {path}"
    );
    items
}

struct Relocate;
impl VisitMut for Relocate {
    fn visit_item_use_mut(&mut self, item: &mut syn::ItemUse) {
        if let syn::UseTree::Path(path) = &mut item.tree
            && path.ident == "crate"
        {
            let tail = path.tree.clone();
            *path.tree = syn::UseTree::Path(syn::UsePath {
                ident: syn::parse_quote!(contracts),
                colon2_token: Default::default(),
                tree: tail,
            });
        }
        syn::visit_mut::visit_item_use_mut(self, item);
    }

    fn visit_path_mut(&mut self, path: &mut syn::Path) {
        let names: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if names.len() > 1 && names.first().is_some_and(|s| s == "crate") {
            let skip = match names.get(1).map(String::as_str) {
                Some("api") if names.get(2).is_some_and(|s| s == "schema") => 3,
                Some("config" | "agent_resume" | "input" | "popup_size") => 1,
                _ => panic!(
                    "new upstream contract dependency requires review: {}",
                    path.to_token_stream()
                ),
            };
            let mut relocated: syn::Path = syn::parse_quote!(crate::contracts);
            if skip == 3 {
                relocated.segments.push(syn::parse_quote!(api));
            }
            relocated
                .segments
                .extend(path.segments.iter().skip(skip).cloned());
            *path = relocated;
        }
        syn::visit_mut::visit_path_mut(self, path);
    }
}

pub fn generate() {
    println!("cargo:rerun-if-changed=build_support/contracts.rs");
    println!("cargo:rerun-if-changed=.gitmodules");
    let head_path = Command::new("git")
        .args(["-C", ROOT, "rev-parse", "--git-path", "HEAD"])
        .output()
        .expect("git is required to verify the Herdr source pin");
    if head_path.status.success() {
        println!(
            "cargo:rerun-if-changed={}",
            String::from_utf8(head_path.stdout).unwrap().trim()
        );
    }
    let revision = Command::new("git")
        .args(["-C", ROOT, "rev-parse", "HEAD"])
        .output()
        .expect("git is required to verify the Herdr source pin");
    assert!(
        revision.status.success(),
        "initialize Herdr with git submodule update --init vendor/herdr"
    );
    assert_eq!(
        String::from_utf8(revision.stdout).unwrap().trim(),
        REVISION,
        "Herdr revision does not match the reviewed schema pin"
    );
    let clean = Command::new("git")
        .args(["-C", ROOT, "diff", "--quiet", "HEAD", "--", "src"])
        .status()
        .unwrap();
    assert!(
        clean.success(),
        "Herdr source must be unmodified; update the upstream revision instead of copying or patching types"
    );

    let api = api("src/api/schema.rs");
    let methods = api
        .iter()
        .find_map(|i| match i {
            Item::Enum(e) if e.ident == "Method" => Some(e),
            _ => None,
        })
        .expect("upstream Method enum missing");
    let mut method_arms = Vec::new();
    let mut method_names = Vec::new();
    for variant in &methods.variants {
        let mut name = None;
        let mut skipped = false;
        for attr in &variant.attrs {
            if attr.path().is_ident("serde") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rename") {
                        name = Some(meta.value()?.parse::<syn::LitStr>()?);
                    } else if meta.path.is_ident("skip") {
                        skipped = true;
                    } else {
                        return Err(meta.error("new Method serde metadata needs review"));
                    }
                    Ok(())
                })
                .expect("unsupported Method metadata");
            }
        }
        let variant_name = &variant.ident;
        if skipped {
            method_arms.push(quote!(api::Method::#variant_name(..) => None,));
        } else {
            let name = name.expect("every public Method must have an explicit upstream wire name");
            method_arms.push(quote!(api::Method::#variant_name(..) => Some(#name),));
            method_names.push(name);
        }
    }
    let wire: Vec<_> = source("src/protocol/wire.rs")
        .items
        .into_iter()
        .filter(|i| match i {
            Item::Struct(_) | Item::Enum(_) | Item::Type(_) | Item::Const(_) | Item::Use(_) => true,
            Item::Fn(f) => f.sig.ident == "deserialize_client_shell_agent_status",
            Item::Impl(i) => i
                .trait_
                .as_ref()
                .is_some_and(|(_, p, _)| p.is_ident("Clone")),
            _ => false,
        })
        .collect();
    let endpoint: Vec<_> = source("src/protocol/endpoint.rs")
        .items
        .into_iter()
        .filter(|i| match i {
            Item::Struct(_) | Item::Enum(_) | Item::Type(_) | Item::Const(_) => true,
            Item::Fn(f) => f.sig.ident == "default_true",
            Item::Impl(i) => i.self_ty.to_token_stream().to_string() == "EndpointClientHello",
            _ => false,
        })
        .collect();
    let terminal_control: Vec<_> = source("src/client/terminal_sessions.rs")
        .items
        .into_iter()
        .filter_map(|mut item| {
            match &mut item {
                Item::Enum(e) if e.ident.to_string().starts_with("TerminalControl") => {
                    e.vis = syn::parse_quote!(pub);
                }
                Item::Fn(f) if f.sig.ident == "terminal_control_command_from_json" => {
                    f.vis = syn::parse_quote!(pub);
                }
                _ => return None,
            }
            Some(item)
        })
        .collect();
    assert_eq!(
        terminal_control.len(),
        4,
        "upstream terminal control contract changed"
    );
    let config = supporting_types(
        "src/config/model.rs",
        &["ToastHerdrPosition", "ConfigReloadStatus"],
    );
    let input = supporting_types("src/input/model.rs", &["WindowsKeyRecord"]);
    let resume = supporting_types("src/agent_resume.rs", &["AgentSessionRefKind"]);
    let popup: Vec<_> = source("src/popup_size.rs")
        .items
        .into_iter()
        .filter(|i| match i {
            Item::Enum(e) => e.ident == "PopupSize",
            Item::Impl(i) => i.self_ty.to_token_stream().to_string() == "PopupSize",
            Item::Use(u) => !u.to_token_stream().to_string().contains("ratatui"),
            _ => false,
        })
        .collect();
    let mut output: syn::File = syn::parse2(quote! {
        pub const UPSTREAM_REVISION: &str = #REVISION;
        /// Wire name from upstream serde metadata; internal-only variants have none.
        pub fn method_name(method: &api::Method) -> Option<&'static str> {
            match method { #(#method_arms)* }
        }
        pub fn method_names() -> &'static [&'static str] { &[#(#method_names),*] }
        pub mod api { #(#api)* }
        pub mod wire { #(#wire)* }
        pub mod endpoint {
            use serde::{Deserialize, Serialize};
            use super::wire::ClientSurfaceSize;
            #(#endpoint)*
        }
        pub mod terminal_control {
            use base64::Engine;
            use super::wire::{ClientMessage, AttachScrollDirection, AttachScrollSource};
            #(#terminal_control)*
        }
        pub mod config { use serde::{Deserialize, Serialize}; #(#config)* }
        pub mod input { use serde::{Deserialize, Serialize}; #(#input)* }
        pub mod agent_resume { use serde::{Deserialize, Serialize}; #(#resume)* }
        pub mod popup_size { #(#popup)* }
    })
    .unwrap();
    Relocate.visit_file_mut(&mut output);
    fs::write(
        Path::new(&env::var_os("OUT_DIR").unwrap()).join("herdr_contracts.rs"),
        prettyplease::unparse(&output),
    )
    .unwrap();
}
