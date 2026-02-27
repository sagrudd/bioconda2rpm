# bioconda2rpm Product SRS

Version: 0.1
Date: February 27, 2026
Status: Draft (decision-aligned baseline)

## 1. Purpose

This document defines functional and non-functional requirements for `bioconda2rpm`, a Rust CLI that converts Bioconda recipes into Phoreus-style RPM artifacts, including dependency-aware builds.

## 2. Scope

In scope:
- Parse Bioconda recipes from a user-provided recipe root.
- Render `meta.yaml` using full Jinja evaluation.
- Resolve recipe dependencies per configurable policy.
- Generate and build RPM artifacts by stage (`spec`, `srpm`, `rpm`).
- Run builds in containers (ephemeral by default).
- Emit machine-readable and human-readable reports.

Out of scope (initial baseline):
- Repository publishing/signing.
- Runtime functional validation beyond package build success.

## 3. Functional Requirements

FR-001 CLI entrypoint
- The tool shall expose `bioconda2rpm build <package>` as the primary command.

FR-002 Build stage control
- The tool shall support stage selection: `spec`, `srpm`, `rpm`.
- Default stage shall be `rpm`.

FR-003 Dependency closure control
- Dependency resolution shall be enabled by default.
- The tool shall provide an opt-out flag (`--no-deps`).
- Dependency policy shall be configurable.
- Default dependency policy shall include `build + host + run`.

FR-004 Recipe rendering
- `meta.yaml` rendering shall use full Jinja support.
- `source.patches` entries from recipe metadata shall be staged and applied during SPEC `%prep`.

FR-005 Multi-output recipes
- Recipes defining `outputs:` shall produce discrete RPM outputs for all declared outputs.

FR-006 Versioned recipe directories
- If versioned subdirectories exist for a recipe, the highest versioned subdirectory shall be selected.

FR-007 Missing dependency handling
- Default behavior shall quarantine unresolved packages and continue building the resolvable subset.

FR-008 Container execution model
- Default build execution shall use an ephemeral container per run.

FR-009 Architecture behavior
- Default target architecture shall be host architecture.
- Arch-specific build logic shall be segregated in a single SPEC via `%ifarch` conditionals.
- The system shall maintain one SPEC per software package.

FR-010 Artifact/output location
- The CLI shall default build artifact output to `~/bioconda2rpm` when `--topdir` is omitted.
- The CLI shall create default output directories when missing.
- The default quarantine path shall be `<topdir>/BAD_SPEC`.
- The CLI shall allow overriding topdir and quarantine paths.
- The crate repository shall not be used as default build artifact location.

FR-011 Reporting
- Each run shall emit:
  - Console logs
  - JSON summary
  - CSV report
  - Markdown summary
  - Per-package dependency graph reports (JSON + Markdown) capturing dependency resolution source and status.

FR-012 Naming profile
- Default naming profile shall follow Phoreus:
  - Payload: `phoreus-<tool>-<version>`
  - Meta/default: `phoreus-<tool>`

FR-013 Compliance policy
- The workflow shall normalize licenses to SPDX identifiers and run policy checks.
- Packages failing license compliance checks shall be quarantined.

FR-014 Priority SPEC generation workflow
- The system shall support generating SPEC pairs for top-priority tools from a user-provided `tools.csv`.
- Priority selection shall use `RPM Priority Score` ordering.
- Overlap resolution against Bioconda recipes shall be automated.
- SPEC generation shall be parallelizable via worker configuration.
- Generated SPEC content shall be derived programmatically from Bioconda metadata (`meta.yaml` and `build.sh`) without reusing pre-generated external SPEC files.

FR-015 Containerized build chain
- For generated SPECs, the build order shall always be `SPEC -> SRPM -> RPM`.
- SRPM generation shall execute inside a user-selected container image.
- RPM generation shall rebuild from the generated SRPM (not direct SPEC-to-RPM).
- The CLI shall expose a container image flag for this selection.

FR-016 Architecture restriction capture policy
- When build logs indicate architecture-intrinsic incompatibility (for example, `emmintrin.h` missing on `aarch64`), the system shall classify the result as an architecture restriction (for example, `amd64_only`).
- The classification shall be recorded in run reports and quarantine notes/reasons.
- Architecture restrictions shall not block processing of other packages in the same run.

FR-017 Build dependency tolerance and sourcing policy
- During containerized SRPM->RPM rebuild, the system shall preflight `BuildRequires` and attempt resolution in this order:
  1. Already installed packages in the build container.
  2. Locally produced RPM artifacts under `<topdir>/RPMS`.
  3. Enabled distribution/core repositories.
- The workflow shall tolerate unavailable/auxiliary repositories by using package-manager settings that avoid hard failure from missing optional repos.
- Generated payload RPMs shall provide the plain software identifier (for example `samtools`) so downstream package builds can consume locally produced RPMs.
- If any dependency remains unresolved, the package shall be quarantined and unresolved dependencies shall be recorded in reports.

FR-018 Version freshness and metapackage update policy
- For `build <tool>`, if the requested Bioconda payload version is already present in local artifacts, the command shall report the package as up-to-date and skip rebuild.
- If the requested Bioconda payload version is newer than the latest local payload artifact, the payload shall be rebuilt.
- When a newer payload is rebuilt, the corresponding default/meta package version shall be incremented and rewired to the new payload version.
- Successful/up-to-date outcomes shall clear stale package-specific quarantine notes in `BAD_SPEC`.

FR-019 Python dependency isolation policy
- Python application recipes shall be packaged as hermetic virtual environments under `/usr/local/phoreus/<tool>/<version>/venv`.
- Python application detection shall include both metadata signals (`requirements`, `noarch: python`, recipe build script fields) and staged build script inspection for Python install patterns.
- Python dependency resolution for these recipes shall occur inside the venv using lockfile/hash workflow (`pip-compile --generate-hashes`, then `pip install --require-hashes`).
- Generated RPM specs for Python applications shall not emit shared python library RPM dependencies (for example `Requires: jinja2`); runtime dependencies shall be limited to `phoreus` and the selected `phoreus-python-*` interpreter package.

FR-020 R runtime and dependency isolation policy
- The system shall provide a bootstrap runtime package `phoreus-r-4.5.2` for R-dependent recipe builds.
- R ecosystem dependencies (`r`, `r-base`, `r-*`, `bioconductor-*`) shall map to the Phoreus R runtime package rather than distro `R-*` packages.
- R ecosystem dependencies shall never be converted into Python lock requirements for `pip-compile`.
- For R project recipes, generated SPECs shall configure isolated R library roots (`R_LIBS_USER`) under the Phoreus tool prefix and perform lock restoration when `renv.lock` is present.

FR-021 Precompiled binary preference policy
- The system shall support package-specific policy overrides that force consumption of upstream precompiled binary artefacts when upstream documentation recommends this path.
- For packages under this policy, source-based de novo builds shall be bypassed.
- If upstream precompiled binaries do not exist for the active target architecture, the package shall be quarantined with architecture-policy metadata.

## 4. Non-Functional Requirements

NFR-001 Reproducibility
- Builds shall run in controlled container environments.

NFR-002 Determinism
- Given fixed recipe input and source state, repeated runs shall produce consistent decisions and outputs.

NFR-003 Traceability
- Quarantine, resolution, and build decisions shall be recorded in structured reports, including dependency resolution graphs with source attribution (`installed`, `local_rpm`, `repo`, `unresolved`).

NFR-004 Maintainability
- CLI behavior shall be covered by unit tests for parsing defaults and overrides.

NFR-005 Enterprise-readiness
- The architecture shall support future integration with dedicated per-architecture runners.
