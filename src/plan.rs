//! Turn an [`Analysis`] into a human- (and script-) readable extraction plan.
//! v0 stops at the plan; the apply phase (move files, write the new manifest,
//! add the re-export shim, register the workspace member) plugs in here next.

use std::path::{Path, PathBuf};

use crate::analyze::Analysis;
use crate::module::ModuleSource;
use crate::workspace::{Package, Workspace};

pub struct Plan {
    pub source_crate: String,
    pub module: String,
    /// New crate name suggestion: `<source>-<module>`.
    pub new_crate: String,
    pub files: Vec<PathBuf>,
    pub analysis: Analysis,
    root: PathBuf,
}

pub fn build(ws: &Workspace, pkg: &Package, module: &ModuleSource, analysis: Analysis) -> Plan {
    Plan {
        source_crate: pkg.name.clone(),
        module: module.name.clone(),
        new_crate: format!("{}-{}", pkg.name, module.name),
        files: module.files.clone(),
        analysis,
        root: ws.root.clone(),
    }
}

impl Plan {
    /// The decorated, default output.
    pub fn print(&self) {
        let clean = self.analysis.is_clean_leaf();
        println!(
            "extraction plan: {}::{} → new crate",
            self.source_crate, self.module
        );
        println!("  new crate:      {}", self.new_crate);

        println!("  files ({}):", self.files.len());
        for f in &self.files {
            println!("    {}", self.rel(f));
        }

        if self.analysis.deps_used.is_empty() {
            println!("  deps to move:   (none — std only)");
        } else {
            println!("  deps to move:");
            for d in &self.analysis.deps_used {
                let feats = if d.features.is_empty() {
                    String::new()
                } else {
                    format!("  features = [{}]", d.features.join(", "))
                };
                let rename = d
                    .rename
                    .as_ref()
                    .map(|r| format!(" (as {r})"))
                    .unwrap_or_default();
                println!("    {} = \"{}\"{rename}{feats}", d.name, d.req);
            }
        }

        println!(
            "  parent refs:    {} site(s) use crate::{} (a re-export shim keeps them working)",
            self.analysis.outbound_sites, self.module
        );

        if clean {
            println!("  coupling:       none — clean leaf module, safe to lift ✓");
        } else {
            println!("  coupling:       ⚠ reaches back into the parent — lifting would need these to move too or would create a cycle:");
            for r in &self.analysis.inbound {
                println!("    uses {r}");
            }
            if self.analysis.super_refs > 0 {
                println!(
                    "    {} `super::` reference(s) out of the module",
                    self.analysis.super_refs
                );
            }
        }

        println!();
        if clean {
            println!("verdict: ready to lift (apply phase not implemented in v0 — dry-run only).");
        } else {
            println!("verdict: blocked — resolve the coupling above before lifting.");
        }
    }

    /// Plain, greppable output for `--list`.
    pub fn print_plain(&self) {
        println!("crate\t{}", self.source_crate);
        println!("module\t{}", self.module);
        println!("new_crate\t{}", self.new_crate);
        for f in &self.files {
            println!("file\t{}", self.rel(f));
        }
        for d in &self.analysis.deps_used {
            println!("dep\t{}\t{}", d.name, d.req);
        }
        println!("outbound_sites\t{}", self.analysis.outbound_sites);
        for r in &self.analysis.inbound {
            println!("inbound\t{r}");
        }
        println!("super_refs\t{}", self.analysis.super_refs);
        println!(
            "verdict\t{}",
            if self.analysis.is_clean_leaf() {
                "clean-leaf"
            } else {
                "blocked"
            }
        );
    }

    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}
