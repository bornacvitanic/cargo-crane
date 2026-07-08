//! `cargo-crane` — lift a module out of a crate into a brand-new crate.
//!
//! v0 is the analysis half: point it at `<package>::<module>` and it works out
//! the cut set (which files move), which dependencies the moved code needs,
//! whether the module reaches back into its parent (coupling that would create
//! a dependency cycle), and how many call sites a re-export shim must cover —
//! then prints the plan. The apply phase (actually moving files and rewriting
//! manifests/paths) plugs into `plan.rs` next.

mod analyze;
mod apply;
mod cli;
mod closure;
mod module;
mod plan;
mod tui;
mod workspace;

use cli::Cli;

fn main() {
    let cfg = match cli::parse(std::env::args()) {
        Cli::Help => {
            print!("{}", cli::usage());
            return;
        }
        Cli::Error(e) => {
            eprintln!("error: {e}\n\n{}", cli::usage());
            std::process::exit(2);
        }
        Cli::Run(cfg) => cfg,
    };

    if let Err(e) = run(cfg) {
        eprintln!("cargo-crane: {e}");
        std::process::exit(1);
    }
}

fn run(cfg: cli::Config) -> Result<(), String> {
    let ws = workspace::load(&cfg)?;

    // No target → open the interactive browser.
    let Some(target) = cfg.target.clone() else {
        if cfg.list || cfg.apply {
            return Err("--list / --apply need a <package>::<module> target".into());
        }
        return tui::run(&ws, cfg.allow_dirty);
    };

    let (pkg_name, module_name) = cli::split_target(&target)?;
    let pkg = ws.find(pkg_name).ok_or_else(|| {
        format!(
            "no workspace member named `{pkg_name}` (members: {})",
            ws.member_names()
        )
    })?;

    // Resolve the module up front for a clean "not found" error before the
    // (more involved) closure walk.
    module::resolve(pkg, module_name)?;
    let closure = closure::compute(pkg, module_name)?;
    let plan = plan::build(&ws, pkg, closure);

    if cfg.apply {
        let log = apply::apply(&ws, pkg, &plan, cfg.allow_dirty)?;
        println!(
            "extracted {}::{} → {}",
            plan.source_crate, plan.closure.target, plan.new_crate
        );
        for line in &log {
            println!("  ✓ {line}");
        }
        println!("\nnext: run `cargo check` to verify the workspace still builds.");
        println!(
            "undo:  `git restore .` (and delete {}/) to revert.",
            plan.new_crate
        );
    } else if cfg.list {
        plan.print_plain();
    } else {
        plan.print();
    }
    Ok(())
}
