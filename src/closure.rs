//! The move-together closure: starting from the target module, transitively
//! pull in every sibling module it references (and that they reference), so the
//! extracted set is self-contained. Inside the new crate the moved modules keep
//! their names, so all their `crate::<mod>` / `super::<mod>` paths still resolve
//! — no internal rewriting needed. What can't be resolved that way (references
//! to crate-root items, or multi-file modules v0 can't move) is recorded and
//! blocks the lift.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;

use crate::analyze::{self, ResolvedDep};
use crate::workspace::Package;

pub struct Closure {
    pub target: String,
    /// Every module that must move, sorted; includes the target.
    pub modules: Vec<String>,
    /// All files across those modules.
    pub files: Vec<PathBuf>,
    /// External deps the moved code needs.
    pub deps: Vec<ResolvedDep>,
    /// References to crate-root items — genuine blockers.
    pub escapes: BTreeSet<String>,
    /// How many places in the rest of the crate reference the moved modules.
    pub outbound_sites: usize,
}

impl Closure {
    pub fn extractable(&self) -> bool {
        self.escapes.is_empty()
    }

    /// Modules dragged in by coupling (everything but the target).
    pub fn also_moved(&self) -> Vec<&String> {
        self.modules.iter().filter(|m| **m != self.target).collect()
    }
}

pub fn compute(pkg: &Package, target: &str) -> Result<Closure, String> {
    let tops = analyze::top_level_modules(pkg);

    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    seen.insert(target.to_string());
    queue.push_back(target.to_string());

    let mut files = Vec::new();
    let mut candidates = BTreeSet::new();
    let mut escapes = BTreeSet::new();

    while let Some(m) = queue.pop_front() {
        let facts = analyze::analyze_module(pkg, &m, &tops)?;
        files.extend(facts.files);
        candidates.extend(facts.candidates);
        escapes.extend(facts.escapes);
        for r in facts.module_refs {
            if seen.insert(r.clone()) {
                queue.push_back(r);
            }
        }
    }

    let mut modules: Vec<String> = seen.iter().cloned().collect();
    modules.sort();

    let moved_set: BTreeSet<PathBuf> = files.iter().cloned().collect();
    let outbound_sites = analyze::count_outbound(pkg, &moved_set, &seen);
    let deps = analyze::resolve_deps(pkg, &candidates);

    Ok(Closure {
        target: target.to_string(),
        modules,
        files,
        deps,
        escapes,
        outbound_sites,
    })
}
