//! Per-module source analysis feeding the move-together closure.
//!
//! For one module we collect, by leading path segment: the external crates it
//! uses (deps the new crate needs), the sibling *modules* it references
//! (`crate::<mod>` / `super::<mod>` where the name is a top-level module — these
//! must move with it), and any *escapes* — `crate::<item>` / `super::<item>`
//! that point at something which is not a module (a crate-root item). Escapes
//! are what genuinely block a lift: they'd need a dependency back-edge into the
//! parent and thus a cycle. `self`/`std`/`core`/`alloc`/`Self` are ignored.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

use crate::module;
use crate::workspace::Package;

/// A dependency the moved code needs, ready to reproduce in a new manifest.
pub struct ResolvedDep {
    pub name: String,
    pub rename: Option<String>,
    pub req: String,
    pub features: Vec<String>,
}

/// What one module's source reveals.
pub struct ModuleFacts {
    pub files: Vec<PathBuf>,
    pub single_file: bool,
    /// External-crate identifiers referenced.
    pub candidates: BTreeSet<String>,
    /// Sibling top-level modules referenced (must move together).
    pub module_refs: BTreeSet<String>,
    /// References to crate-root items — the real blockers.
    pub escapes: BTreeSet<String>,
}

/// The crate's top-level module names, inferred from `src/`: every `X.rs`
/// (other than the crate root) and every subdirectory is a module `X`.
pub fn top_level_modules(pkg: &Package) -> BTreeSet<String> {
    let root_stem = pkg
        .crate_root
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let mut mods = BTreeSet::new();
    let Ok(entries) = fs::read_dir(&pkg.src_dir) else {
        return mods;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(n) = path.file_name().and_then(|s| s.to_str()) {
                mods.insert(n.to_string());
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if stem != root_stem && stem != "mod" {
                    mods.insert(stem.to_string());
                }
            }
        }
    }
    mods
}

/// Analyse a single module against the set of known top-level module names.
pub fn analyze_module(
    pkg: &Package,
    name: &str,
    tops: &BTreeSet<String>,
) -> Result<ModuleFacts, String> {
    let src = module::resolve(pkg, name)?;
    let single_file = src.files.len() == 1;
    let mut refs = Refs::new(name.to_string(), tops.clone());
    for path in &src.files {
        let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let ast = syn::parse_file(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        refs.visit_file(&ast);
    }
    Ok(ModuleFacts {
        files: src.files,
        single_file,
        candidates: refs.candidates,
        module_refs: refs.module_refs,
        escapes: refs.escapes,
    })
}

/// Intersect external-crate candidates with the parent's declared normal deps.
pub fn resolve_deps(pkg: &Package, candidates: &BTreeSet<String>) -> Vec<ResolvedDep> {
    pkg.deps
        .iter()
        .filter(|d| d.normal && candidates.contains(&d.extern_ident()))
        .map(|d| ResolvedDep {
            name: d.name.clone(),
            rename: d.rename.clone(),
            req: d.req.clone(),
            features: d.features.clone(),
        })
        .collect()
}

/// Count references from the crate's non-moved files to any module in `modules`.
pub fn count_outbound(
    pkg: &Package,
    moved: &BTreeSet<PathBuf>,
    modules: &BTreeSet<String>,
) -> usize {
    let mut out = Outbound {
        modules: modules.clone(),
        count: 0,
    };
    let mut files = Vec::new();
    collect_rs(&pkg.src_dir, &mut files);
    for f in files {
        if moved.contains(&f) {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&f) {
            if let Ok(ast) = syn::parse_file(&text) {
                out.visit_file(&ast);
            }
        }
    }
    out.count
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
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

/// Does the rest of the crate (non-moved files) still reference `module`? Uses
/// the AST outbound count, then falls back to a textual boundary scan — syn
/// doesn't parse macro token streams, so `println!("{}", module::x())` would
/// otherwise look unreferenced. Over-approximating here is safe: it only means
/// keeping a re-export shim we might not have strictly needed.
pub fn parent_references(pkg: &Package, moved: &BTreeSet<PathBuf>, module: &str) -> bool {
    let one: BTreeSet<String> = [module.to_string()].into_iter().collect();
    if count_outbound(pkg, moved, &one) > 0 {
        return true;
    }
    let needle = format!("{module}::");
    let mut files = Vec::new();
    collect_rs(&pkg.src_dir, &mut files);
    for f in files {
        if moved.contains(&f) {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&f) {
            if text_references(&text, &needle) {
                return true;
            }
        }
    }
    false
}

/// True if `needle` (`<module>::`) appears at an identifier boundary — so
/// `thing::` matches but `something::` doesn't.
fn text_references(text: &str, needle: &str) -> bool {
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(rel) = text[start..].find(needle) {
        let pos = start + rel;
        let ident_before =
            pos > 0 && (bytes[pos - 1].is_ascii_alphanumeric() || bytes[pos - 1] == b'_');
        if !ident_before {
            return true;
        }
        start = pos + 1;
    }
    false
}

/// Gathers module/extern/escape references from one module's source.
struct Refs {
    self_module: String,
    tops: BTreeSet<String>,
    candidates: BTreeSet<String>,
    module_refs: BTreeSet<String>,
    escapes: BTreeSet<String>,
}

impl Refs {
    fn new(self_module: String, tops: BTreeSet<String>) -> Self {
        Self {
            self_module,
            tops,
            candidates: BTreeSet::new(),
            module_refs: BTreeSet::new(),
            escapes: BTreeSet::new(),
        }
    }

    fn classify(&mut self, idents: &[String]) {
        let Some(first) = idents.first() else {
            return;
        };
        match first.as_str() {
            // For a top-level module, `super::` and `crate::` both name a
            // sibling of the module at the crate root.
            "crate" | "super" => match idents.get(1) {
                Some(a) if a == &self.self_module => {}
                Some(a) if self.tops.contains(a) => {
                    self.module_refs.insert(a.clone());
                }
                Some(a) => {
                    self.escapes.insert(format!("{first}::{a}"));
                }
                None => {
                    self.escapes.insert(first.clone());
                }
            },
            "self" | "std" | "core" | "alloc" | "Self" => {}
            other => {
                self.candidates.insert(other.to_string());
            }
        }
    }
}

impl<'ast> Visit<'ast> for Refs {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        let idents: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        self.classify(&idents);
        visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut prefix = Vec::new();
        use_prefix(&item.tree, &mut prefix);
        self.classify(&prefix);
        visit::visit_item_use(self, item);
    }
}

/// The leading identifier chain of a `use` tree, stopping at the first group or
/// glob (e.g. `use crate::cli::Config` → [crate, cli, Config]).
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

/// Counts references to any of `modules` — `crate::<m>::…` or bare `<m>::…`.
struct Outbound {
    modules: BTreeSet<String>,
    count: usize,
}

impl Outbound {
    fn hits(&self, idents: &[String]) -> bool {
        match idents.first().map(String::as_str) {
            Some("crate") => idents.get(1).is_some_and(|m| self.modules.contains(m)),
            Some(first) => self.modules.contains(first),
            None => false,
        }
    }
}

impl<'ast> Visit<'ast> for Outbound {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        let idents: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if self.hits(&idents) {
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

    fn refs_of(src: &str, self_module: &str, tops: &[&str]) -> Refs {
        let file = syn::parse_file(src).expect("test source parses");
        let tops: BTreeSet<String> = tops.iter().map(|s| s.to_string()).collect();
        let mut r = Refs::new(self_module.to_string(), tops);
        r.visit_file(&file);
        r
    }

    #[test]
    fn detects_externs() {
        let r = refs_of(
            "use serde::Deserialize;\nfn f() { let _ = serde_json::to_string(&0); }",
            "theme",
            &[],
        );
        assert!(r.candidates.contains("serde"));
        assert!(r.candidates.contains("serde_json"));
    }

    #[test]
    fn sibling_module_reference_is_a_move_together() {
        let r = refs_of("use crate::cli::Config;", "discover", &["cli", "discover"]);
        assert!(r.module_refs.contains("cli"));
        assert!(r.escapes.is_empty());
    }

    #[test]
    fn reference_to_root_item_is_an_escape() {
        let r = refs_of(
            "fn f() -> crate::RootThing {}",
            "discover",
            &["cli", "discover"],
        );
        assert!(r.escapes.contains("crate::RootThing"));
        assert!(r.module_refs.is_empty());
    }

    #[test]
    fn super_names_a_sibling_at_the_root() {
        let r = refs_of(
            "fn f() { super::cli::go(); }",
            "discover",
            &["cli", "discover"],
        );
        assert!(r.module_refs.contains("cli"));
    }

    #[test]
    fn self_reference_is_neither() {
        let r = refs_of("fn f() -> crate::discover::T {}", "discover", &["discover"]);
        assert!(r.module_refs.is_empty());
        assert!(r.escapes.is_empty());
    }

    #[test]
    fn text_scan_respects_identifier_boundary() {
        assert!(text_references(
            "println!(\"{}\", thing::value())",
            "thing::"
        ));
        assert!(text_references("let x = crate::thing::Y;", "thing::"));
        assert!(!text_references("let x = something::Y;", "thing::"));
        assert!(!text_references("no references here", "thing::"));
    }
}
