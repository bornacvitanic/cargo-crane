//! Tiny hand-rolled arg parser (no clap — a handful of flags), matching the
//! freight suite convention. Invocable directly (`cargo-crane`) or as a cargo
//! subcommand (`cargo crane`), where cargo injects `crane` as the first arg.

use std::path::PathBuf;

pub struct Config {
    /// The module to lift, as `<package>::<module>` (e.g. `cargo-bay::discover`).
    pub target: String,
    /// Point cargo metadata at a specific workspace instead of the cwd.
    pub manifest_path: Option<PathBuf>,
    /// Print the plan as plain text (no decoration) — the scriptable mode.
    pub list: bool,
    /// Actually perform the extraction (v0: clean-leaf, single-file only)
    /// instead of just printing the plan.
    pub apply: bool,
}

pub enum Cli {
    Run(Config),
    Help,
    Error(String),
}

const USAGE: &str = "\
cargo-crane — lift a module out into its own crate

USAGE:
    cargo crane <package>::<module> [OPTIONS]   (or: cargo-crane <package>::<module>)

EXAMPLE:
    cargo crane cargo-bay::discover     # analyse lifting cargo-bay's `discover` module

OPTIONS:
    --apply               Perform the extraction (v0: clean-leaf, single-file only)
    --manifest-path <P>   Use the workspace that contains this Cargo.toml
    --list                Print the extraction plan as plain text
    -h, --help            Show this help

Without --apply, cargo-crane only analyses the extraction and prints a plan
(dry-run). --apply writes files, so run it on a clean git tree.
";

pub fn usage() -> &'static str {
    USAGE
}

pub fn parse<I: Iterator<Item = String>>(mut args: I) -> Cli {
    args.next(); // executable name

    // `cargo crane ...` re-invokes us as `cargo-crane crane ...`; drop that token.
    let mut next = args.next();
    if next.as_deref() == Some("crane") {
        next = args.next();
    }

    let mut target: Option<String> = None;
    let mut manifest_path = None;
    let mut list = false;
    let mut apply = false;

    let mut cur = next;
    while let Some(arg) = cur {
        match arg.as_str() {
            "-h" | "--help" => return Cli::Help,
            "--list" => list = true,
            "--apply" => apply = true,
            "--manifest-path" => match args.next() {
                Some(v) => manifest_path = Some(v.into()),
                None => return Cli::Error("--manifest-path needs a value".into()),
            },
            other if other.starts_with('-') => {
                return Cli::Error(format!("unknown argument: {other}"))
            }
            other => {
                if target.is_some() {
                    return Cli::Error(format!("unexpected extra argument: {other}"));
                }
                target = Some(other.to_string());
            }
        }
        cur = args.next();
    }

    match target {
        Some(target) => Cli::Run(Config {
            target,
            manifest_path,
            list,
            apply,
        }),
        None => Cli::Error("missing <package>::<module> to extract".into()),
    }
}

/// Split a `pkg::module` target into its two halves.
pub fn split_target(target: &str) -> Result<(&str, &str), String> {
    match target.split_once("::") {
        Some((pkg, module)) if !pkg.is_empty() && !module.is_empty() => Ok((pkg, module)),
        _ => Err(format!(
            "target must be `<package>::<module>` (got `{target}`)"
        )),
    }
}
