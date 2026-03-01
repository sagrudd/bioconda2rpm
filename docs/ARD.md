# bioconda2rpm Product ARD

Version: 0.1
Date: February 27, 2026
Status: Draft (decision-aligned baseline)

## 1. Objective

Define the target architecture for `bioconda2rpm` as an enterprise-capable, dependency-aware Bioconda-to-RPM conversion system.

## 1a. Normative Governance References

This architecture is constrained by the following governance artifacts:
- [Module Governance Charter](../MODULE_GOVERNANCE_CHARTER.md)
- [RPM Charter](./RPM_charter.md)
- [Python RPM Charter](./Python_RPM_charter.md)
- [R RPM Charter](./R_RPM_charter.md)
- [Rust RPM Charter](./Rust_RPM_charter.md)

## 2. Architecture Overview

The product is structured as layered components:

1. CLI Layer
- CLAP-based command parsing and validated runtime configuration.
- Primary command: `build`.
- Includes `recipes` management command for explicit Bioconda mirror operations.

2. Recipe Discovery Layer
- Resolves package recipe location under managed default root or user-provided override.
- Manages first-run repository bootstrap by cloning Bioconda recipes into `<topdir>/bioconda-recipes` when absent.
- Supports sync and explicit ref checkout (branch/tag/commit) for managed repository state control.
- Applies versioned-subdirectory selection rule (highest version).
- Resolves overlap from priority input tools to Bioconda recipe directories.

3. Rendering Layer
- Full Jinja rendering of `meta.yaml` into normalized recipe metadata.

4. Dependency Graph Layer
- Extracts `build/host/run` relationships.
- Applies configurable policy with default `build+host+run` closure.
- Records build-time dependency preflight outcomes with source attribution:
  - `installed` (already present in container)
  - `local_rpm` (reused from `<topdir>/targets/<target-id>/RPMS`, with legacy `<topdir>/RPMS` compatibility reads)
  - `repo` (resolved from distribution repositories)
  - `unresolved` (quarantined with reason)

5. Packaging Layer
- Maps recipes to single-SPEC Phoreus naming profile.
- Keeps one canonical recipe-derived SPEC/SOURCE set under `<topdir>/SPECS` and `<topdir>/SOURCES` shared across OS targets.
- Expands `outputs:` into discrete RPM packages.
- For Python application recipes, enforces hermetic venv packaging:
  - venv rooted at `/usr/local/phoreus/<tool>/<version>/venv`
  - dependency lock/install inside venv with hash-verified pip workflow
  - avoids shared python RPM dependency coupling.
- For R ecosystem dependencies, routes dependency mapping through a Phoreus R interpreter package (`phoreus-r-4.5.2`) rather than shared distro R module dependencies.
- For R project recipes, configures isolated R library trees under the tool prefix and supports `renv.lock` restoration in build-time setup.
- For Rust ecosystem dependencies, routes dependency mapping through a pinned Phoreus Rust toolchain package (`phoreus-rust-1.92`, Rust `1.92.0`) rather than distro Rust toolchain packages.
- Rust/Cargo execution is rooted in `/usr/local/phoreus/rust/1.92` and follows global build concurrency policy (serial/adaptive) with deterministic single-core fallback.
- For Nim ecosystem dependencies, routes dependency mapping through a Phoreus Nim runtime package (`phoreus-nim-2.2`) rather than distro Nim package names.
- Supports policy-driven precompiled-binary overrides for selected packages (for example, `k8`) to bypass fragile source bootstrap chains when upstream recommends prebuilt artefacts.

6. Build Execution Layer
- Runs stage-selected build steps (`spec`/`srpm`/`rpm`) in containers.
- Default container mode: ephemeral per build.
- Production `build <tool>` path executes dependency-first:
  - discover Bioconda dependency closure
  - build dependency packages first
  - build requested package last
  - enforce per-package `SPEC -> SRPM -> RPM` chain
- Multi-root build invocations use a dependency-gated queue scheduler:
  - each queued node represents a package payload/meta build unit
  - worker parallelism is bounded by queue worker count
  - node dispatch waits for successful completion of dependency nodes
  - one authoritative process owns a workspace lock for each `--topdir`
  - secondary local `build` invocations submit package names into the authoritative queue through a lock-coordinated request file
  - forwarded package requests inherit authoritative force-rebuild policy
- For generated priority specs, execution is strictly ordered per spec as:
  - SPEC generation
  - SRPM build (`rpmbuild -bs`) in container
  - RPM rebuild from SRPM (`rpmbuild --rebuild`) in container
- Container image is provided at runtime via CLI flag.
- Build concurrency is policy-driven:
  - `serial`: initial single-core execution
  - `adaptive`: configured parallel first attempt with automatic serial retry on failure
  - successful serial retries are recorded in target-scoped stability cache (`reports/build_stability.json`)
- Before SRPM rebuild, `BuildRequires` are preflight-resolved with tolerant sourcing:
  - already installed packages
  - local RPM artifact reuse
  - repository install with unavailable-repo tolerance settings

7. Compliance and Quarantine Layer
- SPDX normalization and policy evaluation.
- Quarantines unresolved dependencies and non-compliant packages.
- Applies architecture-compatibility classification from build logs (for example, `amd64_only`) and records this in failure reporting.

8. Reporting Layer
- Emits JSON, CSV, Markdown summaries plus console logs.
- Emits per-package dependency graph artifacts under target scope (`targets/<target-id>/reports/dependency_graphs/*.json` and `*.md`) for auditability.

9. Priority Selection Layer
- Reads `tools.csv` and ranks requested tools by `RPM Priority Score`.
- Provides deterministic top-N tool set for parallel SPEC generation.

## 3. Runtime Boundaries

- Inputs:
  - Optional external Bioconda recipe root override
  - Otherwise managed default Bioconda recipe root at `<topdir>/bioconda-recipes/recipes`
  - Package name
  - Optional build output topdir override (`--topdir`)
  - Optional quarantine directory override (`--bad-spec-dir`)

- Outputs:
  - `<target-id>` is a deterministic slug derived from container image and target architecture.
  - Canonical generated SPEC/SOURCE artifacts in default topdir `~/bioconda2rpm/SPECS` and `~/bioconda2rpm/SOURCES` (or user override)
  - Target-scoped SRPM/RPM artifacts in `<topdir>/targets/<target-id>/SRPMS` and `<topdir>/targets/<target-id>/RPMS`
  - Target-scoped quarantine artifacts in default `<topdir>/targets/<target-id>/BAD_SPEC` or user override
  - Target-scoped reports in external report directory (or `<topdir>/targets/<target-id>/reports`), including dependency graph artifacts

- Constraint:
  - No build artifacts or recipe staging shall default into the crate workspace.
  - Container runtime must use controlled build profiles only (`almalinux-9.7`, `almalinux-10.1`, `fedora-43`), defaulting to `almalinux-9.7`.
  - Missing local container image for a selected profile is auto-built from `containers/rpm-build-images/` before package build stages run.
  - Managed Bioconda recipe synchronization/checkout must be implemented in-process and must not require a system `git` binary.

## 4. Platform and Build Strategy

- Default target architecture: host architecture.
- Future scale-out expected via dedicated architecture-specific runners.
- Arch-specific behavior remains in one SPEC via `%ifarch` sections.

## 5. Failure Policy

- Missing dependencies: quarantine by default; continue resolvable subset.
- Compliance failure: quarantine affected packages.
- Stage behavior is explicit and user-controlled.

## 6. Initial Implementation Baseline

Current baseline includes:
- Rust crate scaffold.
- CLAP contract for `build` command and key policy flags.
- Unit tests covering default and override parsing behavior.

## 7. Planned Next Increments

1. Implement recipe discovery and versioned-subdirectory selection.
2. Integrate Jinja rendering engine for `meta.yaml`.
3. Build dependency graph resolution and quarantine flow.
4. Generate SPEC files using Phoreus profile.
5. Integrate containerized `rpmbuild` execution and report emission.

## 8. Reliability Validation Scope

- Reliability KPI denominator for redesign validation is the full Bioconda `linux-aarch64` buildable subset.
- First-pass success target on this denominator is `>=99%`.
- Architecture-incompatible packages are excluded from denominator calculations for this KPI.
- Merge validation is expected to apply a hard gate at `>=99%` using arch-adjusted KPI outputs from `build` reports/exit status.

## 9. Heuristic Governance

- Package-specific heuristics are controlled exceptions, not a default implementation pattern.
- Any retained package-specific heuristic must be tagged with `HEURISTIC-TEMP(issue=...)` and associated retirement tracking.
- Build-time tests enforce that untagged package-specific heuristic blocks are rejected.

## 10. Regression Campaign Architecture

- Introduces `regression` command with corpus modes:
  - `pr`: top-N from `tools.csv`
  - `nightly`: full `tools.csv` corpus
- Campaign execution reuses `build` pipeline per tool under a production-aligned profile.
- Campaign reports emit JSON/CSV/Markdown and enforce campaign-level arch-adjusted KPI thresholds.
