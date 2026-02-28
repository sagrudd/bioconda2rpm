# Rust RPM Charter

Version: 0.1  
Date: February 27, 2026  
Status: Active

## 1. Purpose

This charter defines the authoritative policy for Rust toolchain provisioning and Cargo crate handling in `bioconda2rpm`.

## 2. Scope

In scope:
- Rust runtime bootstrap package generation.
- Rust dependency normalization from Bioconda recipe metadata.
- Rust/Cargo build-time environment requirements in generated SPEC files.

Out of scope (current phase):
- Private crates.io mirror and air-gapped crate repository governance.
- Cryptographic signing policy for Cargo index/content.

## 3. Canonical Runtime

The canonical Rust runtime for Phoreus builds is:
- Package name: `phoreus-rust-1.92`
- Rust version: `1.92.0`
- Install prefix: `/usr/local/phoreus/rust/1.92`

All Rust-capable payload builds MUST use this runtime and MUST NOT rely on system Rust or system Cargo binaries.

## 4. Bootstrap Policy

`bioconda2rpm` SHALL provide an on-demand bootstrap path for `phoreus-rust-1.92`.

Bootstrap implementation requirements:
- Toolchain installation is performed via upstream `rustup-init` with `--profile minimal`.
- Toolchain selection is pinned to `1.92.0`.
- Supported bootstrap architectures: `x86_64`, `aarch64`.
- Unsupported architectures fail deterministically with an explicit policy error.

## 5. Rust Dependency Mapping Policy

The following dependency forms are treated as Rust-ecosystem and map to `phoreus-rust-1.92`:
- `rust`
- `rustc`
- `cargo`
- `rustup`
- `rust-*`
- `cargo-*`

Rules:
- Mapping applies to both `BuildRequires` and runtime `Requires` normalization.
- Rust ecosystem dependencies are runtime-provided by the Phoreus Rust package and are not expanded as Bioconda dependency-follow targets.

## 6. SPEC Runtime Environment Policy

For Rust-dependent recipes, generated payload SPEC files MUST:
- Export `PHOREUS_RUST_PREFIX=/usr/local/phoreus/rust/1.92`.
- Verify `rustc` and `cargo` executables exist under `PHOREUS_RUST_PREFIX/bin`.
- Fail fast with deterministic exit code when runtime is missing.
- Export cargo/rustup state roots under the payload prefix:
  - `CARGO_HOME=$PREFIX/.cargo`
  - `RUSTUP_HOME=$PREFIX/.rustup`

## 7. Cargo Build Determinism Policy

For enterprise repeatability, Cargo execution MUST follow orchestrator concurrency policy:
- `serial` policy: `CARGO_BUILD_JOBS=1`
- `adaptive` policy: initial job count from orchestrator `--build-jobs`, with automatic single-core retry on failure
- `CARGO_INCREMENTAL=0`

Temporary build products SHOULD be isolated per build invocation via `CARGO_TARGET_DIR`.

## 8. Cross-Language Interaction Policy

When a recipe requires both Python and Rust build chains:
- Python execution remains bound to `phoreus-python-*` policy.
- Rust execution remains bound to `phoreus-rust-1.92` policy.
- System language runtimes MUST NOT be used as fallback.

## 9. Failure and Reporting Policy

Rust policy failures MUST be explicit and traceable:
- Missing Phoreus Rust runtime: deterministic policy failure.
- Unsupported bootstrap architecture: deterministic policy failure.
- Dependency resolution artifacts MUST capture unresolved Rust-related requirements in dependency graph reports.

## 10. Compliance Requirement

Any Rust-related packaging behavior that diverges from this charter is non-compliant and requires an explicit ADR/decision-log amendment before rollout.
