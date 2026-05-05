# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`bioconda2rpm` is a Rust CLI tool (edition 2024) that converts Bioconda recipes into Phoreus-style RPM artifacts. It manages a local mirror of the bioconda-recipes git repo, parses conda meta.yaml recipes, generates RPM SPEC files via Jinja2 templates, and orchestrates builds (SPEC -> SRPM -> RPM) inside Docker containers.

## Common Commands

```bash
# Build the project
cargo build

# Run checks
cargo check

# Run all tests
cargo test

# Run a specific test
cargo test <test_name>

# Run a single CLI subcommand (dev)
cargo run -- build samtools fastqc
```

## Architecture

Four modules in `src/`:

- **`main.rs`** — Entry point. Dispatches to one of five subcommands. Sets up signal handlers, progress UI, workspace dirs, and the build session lock.
- **`cli.rs`** — Clap-based argument parsing. Defines `Command` enum (`Build`, `Regression`, `GeneratePrioritySpecs`, `Recipes`, `Lookup`) and all arg structs with `effective_*` convenience methods.
- **`priority_specs.rs`** (~15K lines) — Core engine. Handles recipe parsing (meta.yaml + build.sh), dependency resolution, SPEC generation via minijinja, RPM/SRPM builds via docker, parallel build scheduling (rayon), KPI reporting, and Phoreus bootstrap for runtime toolchains (Python, R, Rust, Nim).
- **`build_lock.rs`** — File-lock-based workspace session management. Enables build forwarding (secondary processes submit to the active session queue) and runtime state lookup.
- **`recipe_repo.rs`** — Manages the local git mirror of `github.com/bioconda/bioconda-recipes`. Handles clone, fetch, checkout (branch/tag/commit/ref), and locking.
- **`ui.rs`** — In-terminal progress UI using crossterm + ratatui. Shows per-package build status, phase logs, and queue state.

## Key Design Patterns

- **Target ID**: `<container-image>-<arch>` slug (e.g. `phoreus-bioconda2rpm-build-almalinux-9.7-x86_64`) scopes all outputs under `targets/<target-id>/`.
- **Build stages**: Always SPEC -> SRPM -> RPM. Containerized in the selected profile image (almalinux-9.7/10.1, fedora-43).
- **Parallel policy**: `adaptive` attempts parallel builds first, falls back to serial. `serial` forces single-threaded.
- **Build forwarding**: If a build session is already active for the same target, subsequent `build` invocations forward their package lists to the running process instead of competing.
- **Progress system**: A global `ProgressSink` (OnceLock + Mutex) allows internal threads to emit structured `phase=... key=value` log lines to the UI or stdout.
- **Cancellation**: SIGINT handler sets an AtomicBool; callers check `CANCELLATION_REQUESTED` at natural breakpoints.

## Workspace Layout

```
~/bioconda2rpm/              -- topdir (default)
  bioconda-recipes/          -- managed git mirror of bioconda-recipes
  targets/<target-id>/
    SPECS/                   -- generated payload + meta SPEC files
    SOURCES/                 -- staged build.sh sources
    SRPMS/                   -- built source RPMs
    RPMS/                    -- built binary RPMs
    BAD_SPEC/                -- quarantined/unresolved packages
    reports/                 -- priority_spec_generation.{json,csv,md}
  .bioconda2rpm-artifacts.lock
  .bioconda2rpm-active-builds.json
  .bioconda2rpm-build-requests.jsonl
```

## Dependencies

- **clap** (derive) for CLI parsing
- **minijinja** for SPEC template rendering
- **rayon** for parallel package processing
- **git2** (vendored-libgit2) for recipe repo management
- **crossterm + ratatui** for in-terminal progress UI
- **serde/serde_yaml** for recipe metadata parsing
- **fs2** for file locking
- **ctrlc** for signal handling

## Notes

- `priority_specs.rs` is the largest file (~15K lines). It contains recipe parsing, SPEC generation, docker build orchestration, KPI tracking, and Phoreus bootstrap logic.
- The `scripts/conda_render_ir.py` helper is invoked to render conda meta.yaml via conda-build when the metadata adapter is in auto/conda mode.
- Build images are Dockerfiles in `containers/rpm-build-images/` (almalinux-9.7, almalinux-10.1, fedora-43).
- Tests are inline with `#[cfg(test)]` modules in `cli.rs` and `build_lock.rs`.
