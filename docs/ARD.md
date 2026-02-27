# bioconda2rpm Product ARD

Version: 0.1
Date: February 27, 2026
Status: Draft (decision-aligned baseline)

## 1. Objective

Define the target architecture for `bioconda2rpm` as an enterprise-capable, dependency-aware Bioconda-to-RPM conversion system.

## 2. Architecture Overview

The product is structured as layered components:

1. CLI Layer
- CLAP-based command parsing and validated runtime configuration.
- Primary command: `build`.

2. Recipe Discovery Layer
- Resolves package recipe location under user-provided recipe root.
- Applies versioned-subdirectory selection rule (highest version).
- Resolves overlap from priority input tools to Bioconda recipe directories.

3. Rendering Layer
- Full Jinja rendering of `meta.yaml` into normalized recipe metadata.

4. Dependency Graph Layer
- Extracts `build/host/run` relationships.
- Applies configurable policy with default `build+host+run` closure.

5. Packaging Layer
- Maps recipes to single-SPEC Phoreus naming profile.
- Expands `outputs:` into discrete RPM packages.

6. Build Execution Layer
- Runs stage-selected build steps (`spec`/`srpm`/`rpm`) in containers.
- Default container mode: ephemeral per build.
- For generated priority specs, execution is strictly ordered per spec as:
  - SPEC generation
  - SRPM build (`rpmbuild -bs`) in container
  - RPM rebuild from SRPM (`rpmbuild --rebuild`) in container
- Container image is provided at runtime via CLI flag.

7. Compliance and Quarantine Layer
- SPDX normalization and policy evaluation.
- Quarantines unresolved dependencies and non-compliant packages.
- Applies architecture-compatibility classification from build logs (for example, `amd64_only`) and records this in failure reporting.

8. Reporting Layer
- Emits JSON, CSV, Markdown summaries plus console logs.

9. Priority Selection Layer
- Reads `tools.csv` and ranks requested tools by `RPM Priority Score`.
- Provides deterministic top-N tool set for parallel SPEC generation.

## 3. Runtime Boundaries

- Inputs:
  - External Bioconda recipe root (required)
  - Package name
  - Optional build output topdir override (`--topdir`)
  - Optional quarantine directory override (`--bad-spec-dir`)

- Outputs:
  - SPEC/SRPM/RPM artifacts in default topdir `~/bioconda2rpm` or user override
  - Quarantine artifacts in default `<topdir>/BAD_SPEC` or user override
  - Run reports in external report directory (or `<topdir>/reports`)

- Constraint:
  - No build artifacts or recipe staging shall default into the crate workspace.

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
