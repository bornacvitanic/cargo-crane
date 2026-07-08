//! The apply phase: turn an extractable [`Closure`] into real edits, using
//! preserve-as-modules mode — the moved modules keep their names inside the new
//! crate, so their `crate::<mod>` / `super::<mod>` paths need no rewriting.
//!
//! It creates the new crate (a `lib.rs` of `pub mod` declarations, a manifest
//! with the union of moved deps), moves each module file across, and in the
//! parent replaces each moved `mod <m>;` with either a `use <new-crate>::<m>;`
//! re-export (if the parent still references it) or nothing (if it doesn't),
//! then adds the path dependency and registers the new workspace member.
//!
//! v0 requires a self-contained closure (no escapes to crate-root items); it
//! handles single- and multi-file (directory) modules and promotes
//! `pub(crate)` → `pub` so parent references reach items across the new crate
//! boundary. The string transforms are pure and unit-tested.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::analyze;
use crate::plan::Plan;
use crate::workspace::{Package, Workspace};

pub fn apply(
    ws: &Workspace,
    pkg: &Package,
    plan: &Plan,
    allow_dirty: bool,
) -> Result<Vec<String>, String> {
    let c = &plan.closure;
    if !c.extractable() {
        return Err(
            "this module can't be lifted as-is: it references crate-root items (see the plan's \
             coupling section), which would require a dependency cycle"
                .into(),
        );
    }

    // Refuse to write into a tree that isn't a clean git repo, so the change is
    // always a `git restore` away from undone. Check both the workspace repo and
    // the member's own repo (they differ when the member is a submodule).
    if !allow_dirty {
        ensure_clean(&ws.root)?;
        if pkg.manifest_dir != ws.root {
            ensure_clean(&pkg.manifest_dir)?;
        }
    }

    let new_dir = ws.root.join(&plan.new_crate);
    if new_dir.exists() {
        return Err(format!(
            "{} already exists — refusing to overwrite",
            new_dir.display()
        ));
    }
    let ident = plan.new_crate.replace('-', "_");
    let moved_set: BTreeSet<PathBuf> = c.files.iter().cloned().collect();
    let mut log = Vec::new();

    // 1) New crate skeleton: a lib.rs of `pub mod` lines.
    fs::create_dir_all(new_dir.join("src")).map_err(io("create new crate dir"))?;
    let lib: String = c
        .modules
        .iter()
        .map(|m| format!("pub mod {m};\n"))
        .collect();
    fs::write(new_dir.join("src").join("lib.rs"), lib).map_err(io("write lib.rs"))?;
    let manifest = new_manifest(
        &plan.new_crate,
        &pkg.edition,
        pkg.license.as_deref(),
        &c.deps,
    );
    fs::write(new_dir.join("Cargo.toml"), manifest).map_err(io("write new Cargo.toml"))?;
    log.push(format!(
        "created crate {}/ ({})",
        plan.new_crate,
        c.modules.join(", ")
    ));

    // 2) Move every module file across, preserving its path under src/ (so
    //    directory modules keep their layout) and promoting pub(crate) → pub.
    for f in &c.files {
        let relp = f
            .strip_prefix(&pkg.src_dir)
            .map_err(|_| format!("{} is not under {}", f.display(), pkg.src_dir.display()))?;
        let dest = new_dir.join("src").join(relp);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(io("create module dir"))?;
        }
        let body = fs::read_to_string(f).map_err(io("read module source"))?;
        fs::write(&dest, promote_visibility(&body)).map_err(io("write module"))?;
        fs::remove_file(f).map_err(io("remove module file"))?;
    }
    // Clean up now-empty directory-module folders left behind.
    for m in &c.modules {
        let dir = pkg.src_dir.join(m);
        if dir.is_dir() {
            prune_empty_dirs(&dir);
        }
    }
    log.push(format!(
        "moved {} file(s) into the new crate",
        c.files.len()
    ));

    // 3) Parent crate root: shim the modules the parent still uses; drop the rest.
    let mut root_src = fs::read_to_string(&pkg.crate_root).map_err(io("read crate root"))?;
    for m in &c.modules {
        let referenced = analyze::parent_references(pkg, &moved_set, m);
        root_src = edit_root(&root_src, m, &ident, referenced)?;
        log.push(if referenced {
            format!("shimmed `mod {m}` → `use {ident}::{m}` in the crate root")
        } else {
            format!("removed `mod {m}` from the crate root (unused after the move)")
        });
    }
    fs::write(&pkg.crate_root, root_src).map_err(io("write crate root"))?;

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

/// Require `dir` to be inside a git repository with no uncommitted changes.
fn ensure_clean(dir: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|_| {
            "git was not found, so a clean tree can't be verified — re-run with --allow-dirty to \
             proceed anyway"
                .to_string()
        })?;
    if !output.status.success() {
        return Err(format!(
            "{} is not inside a git repository, so the extraction can't be easily undone — \
             re-run with --allow-dirty to proceed anyway",
            dir.display()
        ));
    }
    if !output.stdout.is_empty() {
        return Err(format!(
            "{} has uncommitted changes — commit or stash them first so the extraction is easy to \
             undo, or re-run with --allow-dirty",
            dir.display()
        ));
    }
    Ok(())
}

// --- pure transforms (unit-tested) --------------------------------------

/// Replace a parent `mod <module>;` (or `pub mod`) with a re-export of the new
/// crate's module (preserving `pub`), or remove the line entirely when the
/// parent no longer references it.
fn edit_root(src: &str, module: &str, ident: &str, referenced: bool) -> Result<String, String> {
    for (is_pub, decl) in [
        (true, format!("pub mod {module};")),
        (false, format!("mod {module};")),
    ] {
        if let Some(pos) = src.find(&decl) {
            let end = pos + decl.len();
            if referenced {
                let kw = if is_pub { "pub use" } else { "use" };
                let shim = format!("{kw} {ident}::{module};");
                return Ok(format!("{}{}{}", &src[..pos], shim, &src[end..]));
            }
            // Drop the declaration and one trailing newline.
            let mut e = end;
            if src[e..].starts_with('\n') {
                e += 1;
            }
            return Ok(format!("{}{}", &src[..pos], &src[e..]));
        }
    }
    Err(format!("could not find `mod {module};` in the crate root"))
}

/// Promote `pub(crate)` items to `pub`: inside the parent they were visible
/// crate-wide, but the parent now reaches them across a crate boundary where
/// `pub(crate)` is private. Over-promoting is harmless.
fn promote_visibility(src: &str) -> String {
    src.replace("pub(crate)", "pub")
}

/// Recursively remove empty directories under (and including) `dir`.
fn prune_empty_dirs(dir: &Path) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                prune_empty_dirs(&path);
            }
        }
    }
    let empty = fs::read_dir(dir)
        .map(|mut it| it.next().is_none())
        .unwrap_or(false);
    if empty {
        let _ = fs::remove_dir(dir);
    }
}

/// Build a fresh Cargo.toml for the extracted crate.
fn new_manifest(
    name: &str,
    edition: &str,
    license: Option<&str>,
    deps: &[analyze::ResolvedDep],
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

/// Insert `<dep_name> = { path = "<rel_path>" }` into `[dependencies]`,
/// preserving the rest of the file's formatting.
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
    fn edit_root_shims_referenced_module() {
        let out = edit_root("mod cli;\nmod theme;\n", "theme", "cargo_bay_theme", true).unwrap();
        assert!(out.contains("use cargo_bay_theme::theme;"));
        assert!(out.contains("mod cli;"));
        assert!(!out.contains("mod theme;"));
    }

    #[test]
    fn edit_root_preserves_pub() {
        let out = edit_root("pub mod theme;\n", "theme", "x", true).unwrap();
        assert_eq!(out, "pub use x::theme;\n");
    }

    #[test]
    fn edit_root_removes_unreferenced_module() {
        let out = edit_root("mod a;\nmod helper;\nmod b;\n", "helper", "x", false).unwrap();
        assert_eq!(out, "mod a;\nmod b;\n");
    }

    #[test]
    fn edit_root_errors_when_absent() {
        assert!(edit_root("fn main() {}", "theme", "x", true).is_err());
    }

    #[test]
    fn promotes_pub_crate_to_pub() {
        let out = promote_visibility("pub(crate) fn f() {}\npub(crate) mod m;\npub struct S;");
        assert_eq!(out, "pub fn f() {}\npub mod m;\npub struct S;");
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
        assert!(dep.contains("serde = \"1\""));

        let mem = add_member("[workspace]\nmembers = [\"app\"]\n", "demo-thing").unwrap();
        assert!(mem.contains("\"demo-thing\""));
        assert_eq!(add_member(&mem, "demo-thing").unwrap(), mem);
    }

    #[test]
    fn relative_dep_path_for_siblings() {
        assert_eq!(
            relative_dep_path(Path::new("/ws/cargo-bay"), Path::new("/ws/cargo-bay-theme")),
            "../cargo-bay-theme"
        );
    }
}
