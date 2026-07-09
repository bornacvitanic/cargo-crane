[![Rust](https://github.com/bornacvitanic/cargo-crane/actions/workflows/rust.yml/badge.svg)](https://github.com/bornacvitanic/cargo-crane/actions/workflows/rust.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Crates.io](https://img.shields.io/crates/v/cargo-crane.svg)](https://crates.io/crates/cargo-crane)
[![Download](https://img.shields.io/badge/download-releases-blue.svg)](https://github.com/bornacvitanic/cargo-crane/releases)

# cargo-crane

`cargo-crane` lifts a module out of a crate into a brand-new crate — the manual, error-prone refactor of moving files, rewriting paths, splitting dependencies, and wiring up the new crate, done for you. Point it at a module and it works out exactly what has to move and whether the lift is safe; then `--apply` performs it and leaves the workspace compiling.

It discovers the workspace at runtime with `cargo metadata`, so it works in any Cargo workspace with no setup.

## What it does

Given `<package>::<module>`, cargo-crane computes the **move-together closure**: starting from the module you name, it transitively pulls in every sibling module that module reaches into via `crate::` / `super::`. If the closure is self-contained, it can be lifted; if the module reaches back into a *crate-root item* (which would require a dependency cycle), the lift is reported as blocked, with the exact reference to fix.

When you apply, it:

- **creates the new crate** — a `lib.rs` of `pub mod` declarations plus a `Cargo.toml` carrying just the dependencies the moved code actually uses (with their features);
- **moves the module files** across, preserving directory layout, so every `crate::`/`super::` path inside them still resolves — no fragile path rewriting;
- **promotes `pub(crate)` to `pub`** so the parent can still reach items across the new crate boundary;
- **shims the parent** — each moved `mod x;` becomes `use <new-crate>::x;` (or is dropped if unused), so existing `crate::x::…` paths keep working;
- **wires it up** — adds the path dependency to the parent and registers the new crate as a workspace member.

## Features

- **Safe by default** — without `--apply` it only analyses and prints a plan (a dry-run). `--apply` refuses to write unless the tree is a clean git repository, so any extraction is a `git restore` away from undone (`--allow-dirty` overrides).
- **Coupling aware** — distinguishes references that can move together from references to crate-root items that genuinely can't, instead of silently producing broken code.
- **Single-file and directory modules** — handles both `foo.rs` and `foo/mod.rs` + submodules.
- **Interactive TUI** — run with no arguments to browse every module in the workspace, preview each one's plan live, and lift the selected one.
- **Scriptable** — `--list` prints the plan as plain text.

## Installation

```sh
cargo install cargo-crane
```

This installs the `cargo-crane` binary, which Cargo exposes as the subcommand `cargo crane`.

## Usage

```sh
cargo crane                         # interactive browser (TUI)
cargo crane cargo-bay::discover     # print the extraction plan (dry-run)
cargo crane cargo-bay::discover --apply
```

### Options

```
--apply               Perform the extraction (writes files)
--allow-dirty         Skip the clean-git-tree check that --apply requires
--manifest-path <P>   Use the workspace that contains this Cargo.toml
--list                Print the extraction plan as plain text
-h, --help            Show help
```

### TUI keys

| Keys | Action |
|------|--------|
| `↑`/`↓` · `j`/`k` | move selection |
| `Enter` · `a` | lift the selected module |
| `q` · `Esc` | quit |

## How it works

- **Discovery** is a single `cargo metadata` call: the workspace root, each member's crate root and source dir, and its declared dependencies.
- **Analysis** parses the module's source with [`syn`](https://crates.io/crates/syn) and classifies every leading path segment — external crate (a dependency to move), sibling module (moves together), crate-root item (a blocker), or `std`/`self` (ignored). A reference to a module found inside a macro body is caught by a textual fallback, since macro token streams aren't parsed.
- **Apply** keeps the moved modules named as they were, which is why no path inside them needs rewriting; the only edits are in the parent crate root, its manifest, and the workspace manifest.

## Limitations

- A module that references a crate-*root* item can't be lifted without breaking a dependency cycle — cargo-crane reports it rather than attempting it.
- Only `pub(crate)` visibility is promoted (not `pub(super)` / `pub(in …)`).
- Coupling that appears only inside macro bodies may be missed by the closure analysis — always run `cargo check` after `--apply` (cargo-crane reminds you).

## Contributing

Contributions are welcome — please open an issue or a pull request.

## License

Licensed under the MIT License — see [LICENSE](LICENSE.md).

## Contact

- **Email**: [borna.cvitanic@gmail.com](mailto:borna.cvitanic@gmail.com)
- **GitHub Issues**: [GitHub Issues Page](https://github.com/bornacvitanic/cargo-crane/issues)
