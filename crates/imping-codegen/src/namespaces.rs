//! The namespace tree: one Rust module per IR namespace, holding the types,
//! functions and globals of that namespace.
//!
//! Every namespace is registered before any module is named, so names can be
//! claimed in a fixed order within each scope: the namespace's types first,
//! then its child modules, then its functions and globals.

use crate::emit::doc;
use crate::names::{self, Scope, doc_name, ident};
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashMap;

/// One namespace: its scope, its items, and its child namespaces.
pub(crate) struct Namespace {
    ir_name: String,
    rust_name: String,
    /// The names claimed in this namespace's module.
    pub(crate) scope: Scope,
    /// The items emitted into this namespace's module, in order.
    pub(crate) items: Vec<TokenStream>,
    children: Vec<Namespace>,
    index: HashMap<String, usize>,
}

impl Namespace {
    /// The bindings' module root, with `scope` as its scope.
    pub(crate) fn root(scope: Scope) -> Self {
        Self::new(String::new(), scope)
    }

    fn new(ir_name: String, scope: Scope) -> Self {
        Self {
            ir_name,
            rust_name: String::new(),
            scope,
            items: Vec::new(),
            children: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Registers the namespace at `path` below this one, and any namespaces
    /// leading to it. Children keep the order of their first registration.
    pub(crate) fn register(&mut self, path: &[String]) {
        let Some((first, rest)) = path.split_first() else {
            return;
        };
        let index = match self.index.get(first) {
            Some(&index) => index,
            None => {
                self.children
                    .push(Namespace::new(first.clone(), Scope::default()));
                self.index.insert(first.clone(), self.children.len() - 1);
                self.children.len() - 1
            }
        };
        self.children[index].register(rest);
    }

    /// Claims the module name of every namespace below this one, each in its
    /// parent's scope, in registration order.
    pub(crate) fn name_modules(&mut self) {
        for child in &mut self.children {
            child.rust_name = self
                .scope
                .claim_numbered(&names::snake_case(&child.ir_name));
            child.name_modules();
        }
    }

    /// The registered namespace at `path` below this one.
    ///
    /// # Panics
    ///
    /// If `path` was never registered, which is an internal error.
    pub(crate) fn at(&mut self, path: &[String]) -> &mut Namespace {
        let Some((first, rest)) = path.split_first() else {
            return self;
        };
        let index = *self
            .index
            .get(first)
            .unwrap_or_else(|| panic!("namespace `{first}` was not registered"));
        self.children[index].at(rest)
    }

    /// The Rust module names leading from this namespace to the one at `path`.
    ///
    /// # Panics
    ///
    /// If `path` was never registered, which is an internal error.
    pub(crate) fn module_path(&self, path: &[String]) -> Vec<String> {
        let mut modules = Vec::with_capacity(path.len());
        let mut current = self;
        for segment in path {
            let index = *current
                .index
                .get(segment)
                .unwrap_or_else(|| panic!("namespace `{segment}` was not registered"));
            current = &current.children[index];
            modules.push(current.rust_name.clone());
        }
        modules
    }

    /// The root's items followed by one `pub mod` per child namespace.
    pub(crate) fn into_tokens(self) -> TokenStream {
        let items = self.items;
        let children = self
            .children
            .into_iter()
            .map(|child| child.into_module(&[]));
        quote! {
            #(#items)*
            #(#children)*
        }
    }

    /// The namespace as a `pub mod`, below the namespace `parent`.
    fn into_module(self, parent: &[String]) -> TokenStream {
        let mut path = parent.to_vec();
        path.push(self.ir_name);
        let doc = doc(format!("The `{}` namespace.", doc_name(&path.join("::"))));
        let name = ident(&self.rust_name);
        let items = self.items;
        let children = self
            .children
            .into_iter()
            .map(|child| child.into_module(&path));
        quote! {
            #doc
            pub mod #name {
                #(#items)*
                #(#children)*
            }
        }
    }
}
