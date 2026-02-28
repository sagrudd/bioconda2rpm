# bioconda2rpm Redesign Plan (Toward >99% First-Pass Builds)

Date: 2026-02-28
Status: Proposed major-direction pivot

## 1. Why the current strategy will not reach >99%

Current implementation is strong on containerization and reporting, but build behavior is still dominated by incremental per-package heuristics inside a single large Rust module (`src/priority_specs.rs`).

Observed indicators:
- `src/priority_specs.rs` is ~7.6k lines and contains many package-specific branches and build-script rewrites.
- `/Users/stephen/bioconda2rpm/BAD_SPEC` currently has many quarantined packages, including cascades caused by unresolved dependency naming and ecosystem translation issues.
- Failure patterns repeat across ecosystems (Python, Perl, C/C++) and are not solved by adding more one-off patches.

Conclusion: we need a model-driven pipeline aligned to Bioconda semantics, not more local heuristics.

## 2. What Bioconda does that we should mirror

From `bioconda-docs`, `bioconda-utils`, and `bioconda-common`:
- Recipe rendering is conda-build-driven (`api.render`) with variants/selectors resolved before scheduling.
- Builds are DAG-driven over `build + host + run` dependency metadata.
- Build/test are separated and isolated; failures are recorded with structured context.
- Configuration is centralized and deterministic (strict channels, pinned toolchain setup).

Implication for bioconda2rpm:
- We should consume a conda-faithful rendered recipe model first, then translate to RPM.
- Dependency planning must use rendered metadata as source-of-truth.

## 3. Proposed architecture pivot

### A) Add a conda-faithful render layer as the only metadata ingress

Introduce a dedicated metadata adapter that renders Bioconda recipes through conda-build semantics and emits a normalized JSON IR consumed by Rust.

Required properties:
- selector-aware (`# [linux]`, arch selectors, etc.)
- variant-aware
- outputs-aware
- includes `source` entries (URL, checksum, patches)
- includes rendered `requirements` per output
- includes build script source (`build.sh` or `build/script`)

This replaces custom partial rendering paths as primary behavior.

### B) Replace ad-hoc dependency mapping with policy tables + provider resolvers

Introduce explicit dependency provider resolution pipeline:
1. Phoreus-local provider (already-built artifacts)
2. Core distro provider (dnf repo metadata)
3. Bioconda package provider (queue for build)
4. Quarantine provider (final unresolved)

Each dependency decision becomes a structured event with reason codes.

### C) Ecosystem packagers (plugin style)

Move ecosystem behavior out of generic spec synthesis into explicit handlers:
- Generic compiled (autotools/cmake/make)
- Python (Phoreus Python + venv policy)
- Perl
- R (Phoreus R policy)
- Rust (Phoreus Rust policy)
- Java

Each handler:
- owns canonical spec sections for that ecosystem
- provides deterministic build/install wrappers
- exposes dependency translation map

### D) Build-orchestration gates

For each target package closure:
1. Render and normalize metadata
2. Plan DAG with dependency closure and provider assignment
3. Generate SPEC(s) from IR (no per-package hard-coded edits)
4. Build SRPM in container
5. Resolve BuildRequires from provider pipeline
6. Rebuild RPM from SRPM
7. Run minimal runtime sanity checks

### E) Quarantine taxonomy (non-blocking but explicit)

Standardize quarantine categories:
- `arch_incompatible`
- `metadata_unresolved`
- `source_unavailable`
- `toolchain_failure`
- `policy_violation`
- `translator_gap`

This allows deterministic retry strategy and measurable burn-down.

## 4. Acceptance criteria and metrics

Primary KPI:
- first-pass success >= 99% for target package set on supported architecture.

Supporting KPIs:
- unresolved dependency rate < 0.5%
- translator-gap quarantine rate < 0.3%
- heuristic branch count trend down each sprint
- median build planning time stable as package count grows

Definition of "first-pass success":
- package builds via `SPEC -> SRPM -> RPM` in configured container
- no manual spec edits
- no rerun with custom per-package flags

## 5. Implementation phases

### Phase 1: Foundation (highest leverage)
- Implement conda-faithful render adapter + JSON IR.
- Implement provider resolver skeleton with explicit reason codes.
- Preserve existing CLI surface (`bioconda2rpm build <tool>`).

### Phase 2: Translator hardening
- Move dependency name normalization to data-driven tables.
- Add ecosystem handlers (Python, Perl, R, Rust first).
- Remove equivalent ad-hoc code paths from monolith.

### Phase 3: Reliability gates
- Add deterministic preflight checks and category-specific fast-fail.
- Add build telemetry (progress heartbeat, per-phase elapsed).
- Add regression suite over curated package corpus.

### Phase 4: >99% validation campaign
- Run large package batch on aarch64 and x86_64.
- Track failures by taxonomy and burn down by category.
- Freeze heuristics; only allow policy-table or handler-level fixes.

## 6. Immediate code-direction changes

1. Stop adding new package-specific shell rewrites unless a policy handler does not yet exist.
2. Treat per-package fixes as temporary compatibility shims with explicit retirement issue.
3. Route all new dependency decisions through reason-coded resolver events.
4. Ensure every failure is classified into the quarantine taxonomy.

## 7. Scope decisions needed from product owner

1. What is the denominator for the 99% metric in phase-1 validation?
   - top N priority tools
   - all requested tools in campaign
   - full Bioconda linux-aarch64 subset
   - Decision (2026-02-28): full Bioconda linux-aarch64 subset

2. For 99% KPI, do we exclude packages flagged as architecture-incompatible upstream?
   - Decision (2026-02-28): yes, exclude architecture-incompatible packages from denominator
   - Deployment clarification (2026-02-28): development campaign currently runs on `aarch64`; production target is `amd64` (`x86_64`).

3. Should the conda-faithful render adapter be implemented as:
   - embedded Python helper shipped with this repo
   - external service/tool invocation

4. Should we gate merge to `main` on a fixed regression corpus build pass-rate threshold?
   - Decision (2026-02-28): yes, hard gate at 99% (arch-adjusted denominator)

5. Do you want strict "no new per-package heuristic branches" from this point, except temporary shims explicitly tagged for removal?
   - Decision (2026-02-28): yes, strict policy with mandatory retirement issue tags
