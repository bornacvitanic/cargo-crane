//! Workspace + package discovery, backed by the shared [`portside`] core. We
//! map its model into the shape the rest of cargo-crane works against: members
//! that have a crate root, with their source dir and declared dependencies.

use std::path::PathBuf;

use crate::cli::Config;

/// A workspace member we can operate on.
pub struct Package {
    pub name: String,
    /// The directory holding the crate's `Cargo.toml`.
    pub manifest_dir: PathBuf,
    /// The crate root file (`src/lib.rs` or `src/main.rs`).
    pub crate_root: PathBuf,
    /// The directory the crate root lives in (`src/`).
    pub src_dir: PathBuf,
    /// The crate's Rust edition (reproduced in an extracted crate).
    pub edition: String,
    /// The crate's license, if declared (reproduced in an extracted crate).
    pub license: Option<String>,
    /// Declared dependencies (normal/dev/build).
    pub deps: Vec<Dep>,
}

impl Package {
    pub fn manifest_path(&self) -> PathBuf {
        self.manifest_dir.join("Cargo.toml")
    }
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

/// Discover the workspace via `portside` and adapt it to cargo-crane's view.
pub fn load(cfg: &Config) -> Result<Workspace, String> {
    let ws = portside::load(&portside::LoadOptions {
        manifest_path: cfg.manifest_path.clone(),
        resolve: false,
    })
    .map_err(|e| e.to_string())?;

    let packages = ws.members.iter().filter_map(package_from).collect();
    Ok(Workspace {
        root: ws.root,
        packages,
    })
}

/// Adapt a portside member — skipping any without a crate root (nothing we
/// could lift a module out of).
fn package_from(p: &portside::Package) -> Option<Package> {
    let crate_root = p.crate_root()?.to_path_buf();
    let src_dir = p.src_dir()?.to_path_buf();
    Some(Package {
        name: p.name.clone(),
        manifest_dir: p.manifest_dir.clone(),
        crate_root,
        src_dir,
        edition: p.edition.clone(),
        license: p.license.clone(),
        deps: p.dependencies.iter().map(dep_from).collect(),
    })
}

fn dep_from(d: &portside::Dependency) -> Dep {
    Dep {
        name: d.name.clone(),
        rename: d.rename.clone(),
        req: d.req.clone(),
        features: d.features.clone(),
        normal: d.is_normal(),
    }
}
