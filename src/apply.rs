//! The apply phase: turn a clean-leaf [`Plan`] into real edits. It creates the
//! new crate (its `lib.rs` from the module, a `Cargo.toml` with the moved
//! deps), removes the module file from the parent, swaps the `mod` declaration
//! for a `pub use <new-crate> as <module>;` re-export shim (so the parent's
//! `crate::<module>::…` paths keep resolving), adds the path dependency, and
//! registers the new workspace member.
//!
//! v0 handles the tractable case only: a single-file, clean-leaf module (no
//! inbound coupling). The string transforms are pure functions so they can be
//! unit-tested without touching the filesystem.

use std::fs;
use std::path::Path;

use crate::plan::Plan;
use crate::workspace::{Package, Workspace};

pub fn apply(ws: &Workspace, pkg: &Package, plan: &Plan) -> Result<Vec<String>, String> {
    if !plan.analysis.is_clean_leaf() {
        return Err(
            "v0 --apply only handles clean-leaf modules; this one reaches back into its parent \
             (see the coupling in the plan)"
                .into(),
        );
    }
    if plan.files.len() != 1 {
        return Err("v0 --apply only handles single-file modules".into());
    }

    let module_file = &plan.files[0];
    let new_dir = ws.root.join(&plan.new_crate);
    if new_dir.exists() {
        return Err(format!(
            "{} already exists — refusing to overwrite",
            new_dir.display()
        ));
    }
    let extern_ident = plan.new_crate.replace('-', "_");
    let mut log = Vec::new();

    // 1) Create the new crate from the module's source.
    fs::create_dir_all(new_dir.join("src")).map_err(io("create new crate dir"))?;
    let module_src = fs::read_to_string(module_file).map_err(io("read module source"))?;
    let lib = rewrite_lib_body(&module_src, &plan.module);
    fs::write(new_dir.join("src").join("lib.rs"), lib).map_err(io("write lib.rs"))?;
    let manifest = new_manifest(
        &plan.new_crate,
        &pkg.edition,
        pkg.license.as_deref(),
        &plan.analysis.deps_used,
    );
    fs::write(new_dir.join("Cargo.toml"), manifest).map_err(io("write new Cargo.toml"))?;
    log.push(format!("created crate {}/", plan.new_crate));

    // 2) Remove the module file from the parent.
    fs::remove_file(module_file).map_err(io("remove module file"))?;
    log.push(format!("moved {} out of the parent", rel(ws, module_file)));

    // 3) Replace `mod <module>;` with a re-export shim in the parent crate root.
    let root_src = fs::read_to_string(&pkg.crate_root).map_err(io("read crate root"))?;
    let shimmed = shim_root(&root_src, &plan.module, &extern_ident)?;
    fs::write(&pkg.crate_root, shimmed).map_err(io("write crate root"))?;
    log.push(format!(
        "shimmed `mod {}` → `use {extern_ident} as {}` in {}",
        plan.module,
        plan.module,
        rel(ws, &pkg.crate_root)
    ));

    // 4) Add the path dependency to the parent manifest.
    let parent_manifest = pkg.manifest_path();
    let parent_src = fs::read_to_string(&parent_manifest).map_err(io("read parent Cargo.toml"))?;
    let rel_path = relative_dep_path(&pkg.manifest_dir, &new_dir);
    let updated = add_dep_path(&parent_src, &plan.new_crate, &rel_path)?;
    fs::write(&parent_manifest, updated).map_err(io("write parent Cargo.toml"))?;
    log.push(format!(
        "added `{} = {{ path = \"{rel_path}\" }}` to {}",
        plan.new_crate,
        rel(ws, &parent_manifest)
    ));

    // 5) Register the new crate as a workspace member.
    let root_manifest = ws.root.join("Cargo.toml");
    let root_toml = fs::read_to_string(&root_manifest).map_err(io("read workspace Cargo.toml"))?;
    let updated_root = add_member(&root_toml, &plan.new_crate)?;
    fs::write(&root_manifest, updated_root).map_err(io("write workspace Cargo.toml"))?;
    log.push(format!(
        "registered `{}` as a workspace member",
        plan.new_crate
    ));

    Ok(log)
}

// --- pure transforms (unit-tested) --------------------------------------

/// Rewrite the module body for life as a crate root: `crate::<module>::` was a
/// self-reference into this module, and is now just `crate::`.
fn rewrite_lib_body(src: &str, module: &str) -> String {
    src.replace(&format!("crate::{module}::"), "crate::")
}

/// Replace the parent's `mod <module>;` (or `pub mod <module>;`) with a
/// re-export of the new crate under the same name, so existing
/// `crate::<module>::…` paths keep working untouched.
fn shim_root(root_src: &str, module: &str, extern_ident: &str) -> Result<String, String> {
    let candidates = [
        (
            format!("pub mod {module};"),
            format!("pub use {extern_ident} as {module};"),
        ),
        (
            format!("mod {module};"),
            format!("use {extern_ident} as {module};"),
        ),
    ];
    for (decl, shim) in candidates {
        if let Some(pos) = root_src.find(&decl) {
            let mut out = String::with_capacity(root_src.len());
            out.push_str(&root_src[..pos]);
            out.push_str(&shim);
            out.push_str(&root_src[pos + decl.len()..]);
            return Ok(out);
        }
    }
    Err(format!(
        "could not find `mod {module};` in the crate root to replace with a shim"
    ))
}

/// Build a fresh Cargo.toml for the extracted crate.
fn new_manifest(
    name: &str,
    edition: &str,
    license: Option<&str>,
    deps: &[crate::analyze::ResolvedDep],
) -> String {
    let mut s = String::new();
    s.push_str("[package]\n");
    s.push_str(&format!("name = \"{name}\"\n"));
    s.push_str("version = \"0.1.0\"\n");
    s.push_str(&format!("edition = \"{edition}\"\n"));
    if let Some(l) = license {
        s.push_str(&format!("license = \"{l}\"\n"));
    }
    s.push_str("\n[dependencies]\n");
    for d in deps {
        let req = d.req.trim_start_matches('^');
        if d.features.is_empty() {
            s.push_str(&format!("{} = \"{req}\"\n", d.name));
        } else {
            let feats = d
                .features
                .iter()
                .map(|f| format!("\"{f}\""))
                .collect::<Vec<_>>()
                .join(", ");
            s.push_str(&format!(
                "{} = {{ version = \"{req}\", features = [{feats}] }}\n",
                d.name
            ));
        }
    }
    s
}

/// Insert `<dep_name> = { path = "<rel_path>" }` into a manifest's
/// `[dependencies]`, preserving the rest of the file's formatting.
fn add_dep_path(manifest_src: &str, dep_name: &str, rel_path: &str) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = manifest_src
        .parse()
        .map_err(|e| format!("parse Cargo.toml: {e}"))?;
    let mut table = toml_edit::InlineTable::new();
    table.insert("path", rel_path.into());
    doc["dependencies"][dep_name] = toml_edit::Item::Value(toml_edit::Value::InlineTable(table));
    Ok(doc.to_string())
}

/// Add `member` to `workspace.members` (idempotently), preserving formatting.
fn add_member(root_src: &str, member: &str) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = root_src
        .parse()
        .map_err(|e| format!("parse workspace Cargo.toml: {e}"))?;
    let members = doc["workspace"]["members"]
        .as_array_mut()
        .ok_or("workspace.members is not an array")?;
    if !members.iter().any(|v| v.as_str() == Some(member)) {
        members.push(member);
    }
    Ok(doc.to_string())
}

// --- small path helpers -------------------------------------------------

/// A relative path from `from` to `to`, both under a shared root (forward
/// slashes, which Cargo accepts on every platform).
fn relative_dep_path(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".to_string());
    }
    for c in &to[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    parts.join("/")
}

fn rel(ws: &Workspace, path: &Path) -> String {
    path.strip_prefix(&ws.root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn io(ctx: &'static str) -> impl Fn(std::io::Error) -> String {
    move |e| format!("{ctx}: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::ResolvedDep;

    #[test]
    fn shim_replaces_plain_mod() {
        let out = shim_root(
            "mod cli;\nmod theme;\nmod ui;\n",
            "theme",
            "cargo_bay_theme",
        )
        .unwrap();
        assert!(out.contains("use cargo_bay_theme as theme;"));
        assert!(!out.contains("mod theme;"));
        assert!(out.contains("mod cli;") && out.contains("mod ui;"));
    }

    #[test]
    fn shim_preserves_pub() {
        let out = shim_root("pub mod theme;\n", "theme", "x").unwrap();
        assert_eq!(out, "pub use x as theme;\n");
    }

    #[test]
    fn shim_errors_when_absent() {
        assert!(shim_root("fn main() {}", "theme", "x").is_err());
    }

    #[test]
    fn lib_body_drops_self_module_prefix() {
        let out = rewrite_lib_body("let x = crate::theme::ACCENT;", "theme");
        assert_eq!(out, "let x = crate::ACCENT;");
    }

    #[test]
    fn manifest_renders_deps_with_features() {
        let deps = vec![
            ResolvedDep {
                name: "serde".into(),
                rename: None,
                req: "^1".into(),
                features: vec!["derive".into()],
            },
            ResolvedDep {
                name: "ratatui".into(),
                rename: None,
                req: "^0.29".into(),
                features: vec![],
            },
        ];
        let m = new_manifest("demo", "2021", Some("MIT"), &deps);
        assert!(m.contains("name = \"demo\""));
        assert!(m.contains("edition = \"2021\""));
        assert!(m.contains("license = \"MIT\""));
        assert!(m.contains("serde = { version = \"1\", features = [\"derive\"] }"));
        assert!(m.contains("ratatui = \"0.29\""));
    }

    #[test]
    fn add_dep_and_member_edit_manifests() {
        let dep = add_dep_path(
            "[dependencies]\nserde = \"1\"\n",
            "demo-thing",
            "../demo-thing",
        )
        .unwrap();
        assert!(dep.contains("demo-thing = { path = \"../demo-thing\" }"));
        assert!(dep.contains("serde = \"1\"")); // untouched

        let mem = add_member("[workspace]\nmembers = [\"app\"]\n", "demo-thing").unwrap();
        assert!(mem.contains("\"demo-thing\""));
        // idempotent
        assert_eq!(add_member(&mem, "demo-thing").unwrap(), mem);
    }

    #[test]
    fn relative_dep_path_for_siblings() {
        let from = Path::new("/ws/cargo-bay");
        let to = Path::new("/ws/cargo-bay-theme");
        assert_eq!(relative_dep_path(from, to), "../cargo-bay-theme");
    }
}
