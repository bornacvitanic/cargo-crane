//! The analysis core: read the module's source and work out (1) which external
//! crates it actually uses (the deps the new crate will need), (2) whether it
//! reaches back into the parent crate (inbound coupling — the thing that would
//! create a dependency cycle and block a clean lift), and (3) how many places
//! in the parent refer to the module (outbound sites a re-export shim must keep
//! working).
//!
//! We classify by the leading path segment: `crate::<self>` is an internal
//! self-reference (fine); `crate::<other>` / `super::` escape the module and
//! are flagged; `std`/`core`/`alloc`/`self`/`Self` are ignored; anything else
//! is a candidate external crate, later intersected with the parent's declared
//! dependencies (so local type/function names fall away).

use std::collections::BTreeSet;
use std::fs;

use syn::visit::{self, Visit};

use crate::module::ModuleSource;
use crate::workspace::{Dep, Package};

pub struct Analysis {
    /// Declared deps the moved code actually references.
    pub deps_used: Vec<ResolvedDep>,
    /// `crate::<other>::…` references into the rest of the parent crate.
    pub inbound: Vec<String>,
    /// Count of `super::` references out of the module.
    pub super_refs: usize,
    /// Count of references to this module from elsewhere in the parent crate.
    pub outbound_sites: usize,
}

pub struct ResolvedDep {
    pub name: String,
    pub rename: Option<String>,
    pub req: String,
    pub features: Vec<String>,
}

impl Analysis {
    /// A module with no inbound coupling can be lifted without introducing a
    /// dependency cycle — the v0 happy path.
    pub fn is_clean_leaf(&self) -> bool {
        self.inbound.is_empty() && self.super_refs == 0
    }
}

pub fn analyze(pkg: &Package, module: &ModuleSource) -> Result<Analysis, String> {
    // --- pass 1: scan the moved code ---
    let mut refs = Refs::new(module.name.clone());
    for path in &module.files {
        let src = fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let ast = syn::parse_file(&src)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        refs.visit_file(&ast);
    }

    // External deps: candidates ∩ the parent's declared (normal) deps.
    let deps_used = pkg
        .deps
        .iter()
        .filter(|d| d.normal && refs.candidates.contains(&d.extern_ident()))
        .map(resolve_dep)
        .collect();

    // --- pass 2: count references to the module from the rest of the crate ---
    let mut outbound = Outbound::new(module.name.clone());
    for entry in crate_files(pkg) {
        if module.files.contains(&entry) {
            continue; // skip the module's own files
        }
        if let Ok(src) = fs::read_to_string(&entry) {
            if let Ok(ast) = syn::parse_file(&src) {
                outbound.visit_file(&ast);
            }
        }
    }

    Ok(Analysis {
        deps_used,
        inbound: refs.inbound.into_iter().collect(),
        super_refs: refs.super_refs,
        outbound_sites: outbound.count,
    })
}

fn resolve_dep(d: &Dep) -> ResolvedDep {
    ResolvedDep {
        name: d.name.clone(),
        rename: d.rename.clone(),
        req: d.req.clone(),
        features: d.features.clone(),
    }
}

/// Every `.rs` file in the crate's source dir (for the outbound scan).
fn crate_files(pkg: &Package) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    collect_rs(&pkg.src_dir, &mut out);
    out
}

fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Classify one leading-segment sequence, updating the running tallies.
fn classify(
    idents: &[String],
    self_module: &str,
    candidates: &mut BTreeSet<String>,
    inbound: &mut BTreeSet<String>,
    super_refs: &mut usize,
) {
    let Some(first) = idents.first() else {
        return;
    };
    match first.as_str() {
        "crate" => match idents.get(1) {
            Some(m) if m == self_module => {} // internal self-reference — fine
            Some(_) => {
                inbound.insert(idents.join("::"));
            }
            None => {}
        },
        "super" => *super_refs += 1,
        "self" | "std" | "core" | "alloc" | "Self" => {}
        other => {
            candidates.insert(other.to_string());
        }
    }
}

/// Collects external-crate candidates + coupling from the moved code.
struct Refs {
    self_module: String,
    candidates: BTreeSet<String>,
    inbound: BTreeSet<String>,
    super_refs: usize,
}

impl Refs {
    fn new(self_module: String) -> Self {
        Self {
            self_module,
            candidates: BTreeSet::new(),
            inbound: BTreeSet::new(),
            super_refs: 0,
        }
    }
}

impl<'ast> Visit<'ast> for Refs {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        let idents: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        classify(
            &idents,
            &self.self_module,
            &mut self.candidates,
            &mut self.inbound,
            &mut self.super_refs,
        );
        visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut prefix = Vec::new();
        use_prefix(&item.tree, &mut prefix);
        classify(
            &prefix,
            &self.self_module,
            &mut self.candidates,
            &mut self.inbound,
            &mut self.super_refs,
        );
        visit::visit_item_use(self, item);
    }
}

/// The leading identifier chain of a `use` tree, stopping at the first group
/// or glob (e.g. `use crate::cli::Config` → [crate, cli, Config]).
fn use_prefix(tree: &syn::UseTree, out: &mut Vec<String>) {
    match tree {
        syn::UseTree::Path(p) => {
            out.push(p.ident.to_string());
            use_prefix(&p.tree, out);
        }
        syn::UseTree::Name(n) => out.push(n.ident.to_string()),
        syn::UseTree::Rename(r) => out.push(r.ident.to_string()),
        syn::UseTree::Glob(_) | syn::UseTree::Group(_) => {}
    }
}

/// Counts references to `module` from the rest of the crate: `crate::module::…`
/// or a bare `module::…` (after a `use crate::module`).
struct Outbound {
    module: String,
    count: usize,
}

impl Outbound {
    fn new(module: String) -> Self {
        Self { module, count: 0 }
    }

    /// Does a leading-segment chain refer to this module — `crate::module::…`
    /// or a bare `module::…`?
    fn hits(&self, idents: &[String]) -> bool {
        match idents.first().map(String::as_str) {
            Some("crate") => idents.get(1).is_some_and(|m| *m == self.module),
            Some(first) => first == self.module,
            None => false,
        }
    }
}

impl<'ast> Visit<'ast> for Outbound {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        let seg: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if self.hits(&seg) {
            self.count += 1;
        }
        visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut prefix = Vec::new();
        use_prefix(&item.tree, &mut prefix);
        if self.hits(&prefix) {
            self.count += 1;
        }
        visit::visit_item_use(self, item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs_of(src: &str, self_module: &str) -> Refs {
        let file = syn::parse_file(src).expect("test source parses");
        let mut r = Refs::new(self_module.to_string());
        r.visit_file(&file);
        r
    }

    fn outbound_of(src: &str, module: &str) -> usize {
        let file = syn::parse_file(src).expect("test source parses");
        let mut o = Outbound::new(module.to_string());
        o.visit_file(&file);
        o.count
    }

    #[test]
    fn detects_externs_via_use_and_path() {
        let r = refs_of(
            "use serde::Deserialize;\nfn f() { let _ = serde_json::to_string(&0); }",
            "theme",
        );
        assert!(r.candidates.contains("serde"));
        assert!(r.candidates.contains("serde_json"));
    }

    #[test]
    fn self_reference_is_internal_not_inbound() {
        let r = refs_of("fn f() -> crate::theme::Palette { todo!() }", "theme");
        assert!(r.inbound.is_empty(), "crate::theme is the module itself");
    }

    #[test]
    fn flags_inbound_coupling_to_a_sibling() {
        let r = refs_of(
            "use crate::cli::Config;\nfn f(_: crate::cli::Config) {}",
            "theme",
        );
        assert!(r.inbound.contains("crate::cli::Config"));
    }

    #[test]
    fn counts_super_references() {
        let r = refs_of("fn f() { super::helper(); }", "theme");
        assert_eq!(r.super_refs, 1);
    }

    #[test]
    fn std_and_keywords_are_ignored() {
        let r = refs_of("use std::fmt;\nfn f() { let _: self::X; }", "theme");
        assert!(!r.candidates.contains("std"));
        assert!(!r.candidates.contains("self"));
    }

    #[test]
    fn outbound_counts_uses_and_paths() {
        // 1 use + bare `theme::` + `crate::theme::` = 3 sites.
        let n = outbound_of(
            "use crate::theme::Palette;\nfn f() { theme::color(); crate::theme::x(); }",
            "theme",
        );
        assert_eq!(n, 3);
    }
}
