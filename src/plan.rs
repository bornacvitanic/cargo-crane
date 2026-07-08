//! Render a [`Closure`] as a human- (and script-) readable extraction plan.

use std::path::{Path, PathBuf};

use crate::closure::Closure;
use crate::workspace::{Package, Workspace};

pub struct Plan {
    pub source_crate: String,
    pub new_crate: String,
    pub closure: Closure,
    root: PathBuf,
}

pub fn build(ws: &Workspace, pkg: &Package, closure: Closure) -> Plan {
    Plan {
        source_crate: pkg.name.clone(),
        new_crate: format!("{}-{}", pkg.name, closure.target),
        closure,
        root: ws.root.clone(),
    }
}

impl Plan {
    pub fn print(&self) {
        let c = &self.closure;
        println!(
            "extraction plan: {}::{} → new crate",
            self.source_crate, c.target
        );
        println!("  new crate:      {}", self.new_crate);

        print!("  modules ({}):   {}", c.modules.len(), c.target);
        let also = c.also_moved();
        if !also.is_empty() {
            let names: Vec<&str> = also.iter().map(|s| s.as_str()).collect();
            print!("  + {} (pulled in by coupling)", names.join(", "));
        }
        println!();

        println!("  files ({}):", c.files.len());
        for f in &c.files {
            println!("    {}", self.rel(f));
        }

        if c.deps.is_empty() {
            println!("  deps to move:   (none — std only)");
        } else {
            println!("  deps to move:");
            for d in &c.deps {
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
            "  parent refs:    {} site(s) into the moved modules (a re-export shim keeps them working)",
            c.outbound_sites
        );

        if c.extractable() {
            if c.also_moved().is_empty() {
                println!("  coupling:       none — clean leaf, safe to lift ✓");
            } else {
                println!(
                    "  coupling:       self-contained — moves {} modules together ✓",
                    c.modules.len()
                );
            }
            println!();
            println!("verdict: ready to lift — run again with --apply.");
        } else {
            println!("  coupling:       ⚠ cannot lift as-is:");
            for e in &c.escapes {
                println!("    references crate-root item `{e}` (would need a dependency cycle)");
            }
            for m in &c.multi_file {
                println!("    module `{m}` is multi-file (v0 handles single-file modules only)");
            }
            println!();
            println!("verdict: blocked — see above.");
        }
    }

    pub fn print_plain(&self) {
        let c = &self.closure;
        println!("crate\t{}", self.source_crate);
        println!("target\t{}", c.target);
        println!("new_crate\t{}", self.new_crate);
        for m in &c.modules {
            println!("module\t{m}");
        }
        for f in &c.files {
            println!("file\t{}", self.rel(f));
        }
        for d in &c.deps {
            println!("dep\t{}\t{}", d.name, d.req);
        }
        println!("outbound_sites\t{}", c.outbound_sites);
        for e in &c.escapes {
            println!("escape\t{e}");
        }
        for m in &c.multi_file {
            println!("multi_file\t{m}");
        }
        println!(
            "verdict\t{}",
            if c.extractable() {
                "extractable"
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
