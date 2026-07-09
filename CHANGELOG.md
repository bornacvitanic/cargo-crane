# Changelog

All notable changes to this project will be documented in this file.

## [0.2.0] - 2026-07-09

### Updates

- Update cargo-crane to discover the workspace via the shared portside core

Replace the hand-rolled `cargo metadata` parsing with the published `portside`
crate (workspace discovery), dropping the serde / serde_json dependencies.
Behaviour is unchanged — cargo-crane is the first tool to build on the shared
core.

## [0.1.0] - 2026-07-09

### Features

- Add cargo-crane: lift a module out of a crate into its own crate

Point it at `<package>::<module>` and it works out the move-together closure
(transitively pulling in sibling modules referenced via `crate::`/`super::`),
which external dependencies the moved code needs, and whether the lift is a
clean leaf, a self-contained multi-module set, or blocked by a reference back
into the parent crate. `--apply` performs the extraction: it creates the new
crate (preserving each moved module's name so paths keep resolving), promotes
`pub(crate)` to `pub` across the new boundary, adds a re-export shim in the
parent, and wires the path dependency and workspace member — refusing unless
the tree is a clean git repo so the change is easy to undo. Run with no target
for an interactive TUI. Single-file and directory modules are both supported.
