//! Resolve a top-level module name to the source files that make it up.
//!
//! Rust maps a `mod foo;` declaration to `foo.rs`, or `foo.rs` plus a `foo/`
//! directory of submodules, or `foo/mod.rs` plus `foo/`, or an inline
//! `mod foo { … }` block. v0 supports the file-backed forms for a single
//! top-level module; inline modules and nested paths are reported as
//! unsupported rather than mangled.

use std::fs;
use std::path::{Path, PathBuf};

use crate::workspace::Package;

pub struct ModuleSource {
    pub name: String,
    /// Every `.rs` file that belongs to the module subtree.
    pub files: Vec<PathBuf>,
}

pub fn resolve(pkg: &Package, module: &str) -> Result<ModuleSource, String> {
    if module.contains("::") {
        return Err(format!(
            "v0 only supports top-level modules; `{module}` is nested"
        ));
    }

    let file = pkg.src_dir.join(format!("{module}.rs"));
    let dir = pkg.src_dir.join(module);
    let mod_rs = dir.join("mod.rs");

    let mut files = Vec::new();
    if file.is_file() {
        files.push(file.clone());
    }
    if mod_rs.is_file() {
        files.push(mod_rs.clone());
    }
    if dir.is_dir() {
        collect_rs(&dir, &mut files);
    }

    if files.is_empty() {
        // Distinguish "declared inline" from "doesn't exist" for a better error.
        if declared_inline(&pkg.crate_root, module) {
            return Err(format!(
                "`{module}` is an inline `mod {module} {{ … }}` — v0 can't lift inline modules yet"
            ));
        }
        return Err(format!(
            "no module `{module}` found under {}",
            pkg.src_dir.display()
        ));
    }

    files.sort();
    files.dedup();

    Ok(ModuleSource {
        name: module.to_string(),
        files,
    })
}

/// Recursively collect `.rs` files under a directory.
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

/// Does the crate root declare `mod <module> { … }` inline (a body, not `;`)?
fn declared_inline(crate_root: &Path, module: &str) -> bool {
    let Ok(src) = fs::read_to_string(crate_root) else {
        return false;
    };
    let Ok(file) = syn::parse_file(&src) else {
        return false;
    };
    file.items
        .iter()
        .any(|item| matches!(item, syn::Item::Mod(m) if m.ident == module && m.content.is_some()))
}
