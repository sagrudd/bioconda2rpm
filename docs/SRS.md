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
- The CLI shall require explicit external output paths for build artifacts.
- The crate repository shall not be used as default build artifact location.

FR-011 Reporting
- Each run shall emit:
  - Console logs
  - JSON summary
  - CSV report
  - Markdown summary

FR-012 Naming profile
- Default naming profile shall follow Phoreus:
  - Payload: `phoreus-<tool>-<version>`
  - Meta/default: `phoreus-<tool>`

FR-013 Compliance policy
- The workflow shall normalize licenses to SPDX identifiers and run policy checks.
- Packages failing license compliance checks shall be quarantined.

## 4. Non-Functional Requirements

NFR-001 Reproducibility
- Builds shall run in controlled container environments.

NFR-002 Determinism
- Given fixed recipe input and source state, repeated runs shall produce consistent decisions and outputs.

NFR-003 Traceability
- Quarantine, resolution, and build decisions shall be recorded in structured reports.

NFR-004 Maintainability
- CLI behavior shall be covered by unit tests for parsing defaults and overrides.

NFR-005 Enterprise-readiness
- The architecture shall support future integration with dedicated per-architecture runners.
