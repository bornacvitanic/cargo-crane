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
        for line in self.lines() {
            println!("{line}");
        }
    }

    /// The plan as display lines — shared by the CLI output and the TUI.
    pub fn lines(&self) -> Vec<String> {
        let c = &self.closure;
        let mut out = Vec::new();
        out.push(format!(
            "extraction plan: {}::{} → new crate",
            self.source_crate, c.target
        ));
        out.push(format!("  new crate:   {}", self.new_crate));

        let also = c.also_moved();
        let suffix = if also.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = also.iter().map(|s| s.as_str()).collect();
            format!("  + {} (pulled in by coupling)", names.join(", "))
        };
        out.push(format!(
            "  modules ({}): {}{suffix}",
            c.modules.len(),
            c.target
        ));

        out.push(format!("  files ({}):", c.files.len()));
        for f in &c.files {
            out.push(format!("    {}", self.rel(f)));
        }

        if c.deps.is_empty() {
            out.push("  deps to move: (none — std only)".to_string());
        } else {
            out.push("  deps to move:".to_string());
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
                out.push(format!("    {} = \"{}\"{rename}{feats}", d.name, d.req));
            }
        }

        out.push(format!(
            "  parent refs: {} site(s) into the moved modules",
            c.outbound_sites
        ));

        if c.extractable() {
            if also.is_empty() {
                out.push("  coupling:    none — clean leaf ✓".to_string());
            } else {
                out.push(format!(
                    "  coupling:    self-contained — moves {} modules together ✓",
                    c.modules.len()
                ));
            }
            out.push(String::new());
            out.push("verdict: ready to lift ✓".to_string());
        } else {
            out.push("  coupling:    ⚠ cannot lift as-is:".to_string());
            for e in &c.escapes {
                out.push(format!("    references crate-root item `{e}`"));
            }
            out.push(String::new());
            out.push("verdict: blocked".to_string());
        }
        out
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
