//! Workspace + package discovery via `cargo metadata`. (This is the code most
//! likely to migrate into the shared `portside` core once a second tool needs
//! it — see the freight plan.) We pull the target package's crate root, source
//! dir, and declared dependencies, which the analysis then works against.

use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use crate::cli::Config;

/// A workspace member we can operate on.
pub struct Package {
    pub name: String,
    /// The crate root file (`src/lib.rs` or `src/main.rs`).
    pub crate_root: PathBuf,
    /// The directory the crate root lives in (`src/`).
    pub src_dir: PathBuf,
    /// Declared dependencies (normal/dev/build).
    pub deps: Vec<Dep>,
}

/// One declared dependency, enough to decide whether the moved code needs it
/// and to reproduce it in the new crate's manifest later.
pub struct Dep {
    pub name: String,
    pub rename: Option<String>,
    pub req: String,
    pub features: Vec<String>,
    pub normal: bool,
}

impl Dep {
    /// The identifier this dep is referred to by in source (rename wins; else
    /// the package name with dashes turned into underscores).
    pub fn extern_ident(&self) -> String {
        self.rename
            .clone()
            .unwrap_or_else(|| self.name.replace('-', "_"))
    }
}

pub struct Workspace {
    pub root: PathBuf,
    pub packages: Vec<Package>,
}

impl Workspace {
    pub fn find(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }

    pub fn member_names(&self) -> String {
        let mut names: Vec<&str> = self.packages.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        names.join(", ")
    }
}

/// Run cargo once and build the workspace view.
pub fn load(cfg: &Config) -> Result<Workspace, String> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--format-version", "1", "--no-deps"]);
    if let Some(mp) = &cfg.manifest_path {
        cmd.arg("--manifest-path").arg(mp);
    }
    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "cargo-crane needs Cargo, but `cargo` was not found on PATH.".to_string()
        } else {
            format!("failed to run cargo metadata: {e}")
        }
    })?;
    if !output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "cargo-crane must run inside a Cargo workspace.\n  cargo metadata: {msg}"
        ));
    }

    let meta: Metadata = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("failed to parse cargo metadata: {e}"))?;

    let packages = meta.packages.iter().filter_map(package_from_meta).collect();

    Ok(Workspace {
        root: PathBuf::from(meta.workspace_root),
        packages,
    })
}

fn package_from_meta(p: &MetaPackage) -> Option<Package> {
    let crate_root = pick_crate_root(&p.targets)?;
    let src_dir = crate_root.parent()?.to_path_buf();
    let deps = p
        .dependencies
        .iter()
        .map(|d| Dep {
            name: d.name.clone(),
            rename: d.rename.clone(),
            req: d.req.clone(),
            features: d.features.clone(),
            normal: d.kind.is_none(), // dev/build deps carry a "kind"; normal is null
        })
        .collect();
    Some(Package {
        name: p.name.clone(),
        crate_root,
        src_dir,
        deps,
    })
}

/// The crate root: the lib target if there is one, else the first bin target.
fn pick_crate_root(targets: &[MetaTarget]) -> Option<PathBuf> {
    let is = |t: &&MetaTarget, kind: &str| t.kind.iter().any(|k| k == kind);
    let lib = targets.iter().find(|t| is(t, "lib"));
    let bin = targets.iter().find(|t| is(t, "bin"));
    lib.or(bin).map(|t| PathBuf::from(&t.src_path))
}

// --- cargo metadata JSON (only the fields we use) -----------------------

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetaPackage>,
    workspace_root: String,
}

#[derive(Deserialize)]
struct MetaPackage {
    name: String,
    targets: Vec<MetaTarget>,
    #[serde(default)]
    dependencies: Vec<MetaDependency>,
}

#[derive(Deserialize)]
struct MetaTarget {
    kind: Vec<String>,
    src_path: String,
}

#[derive(Deserialize)]
struct MetaDependency {
    name: String,
    req: String,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    rename: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}
