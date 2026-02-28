# bioconda2rpm Decision Log

## 2026-02-27

### Q1 - Phase 1 output target
- Default target: build `SPEC + SRPM + binary RPM`.
- Must be configurable via CLI so users can select lower-output modes.
- Long-term direction: support `SPEC-only` mode as sufficient for some workflows.

### Planned CLI behavior (from Q1)
- Expose an option to select output stage, e.g.:
  - `spec`
  - `srpm`
  - `rpm`
- Initial default should map to `rpm` (full build through binary RPM).

### Q2 - Dependency closure default
- Dependency closure policy must be configurable via CLI.
- Default policy: include `build + host + run` dependency sets.
- Tool should support switching policies for stricter or lighter closure behavior.

### Q3 - `meta.yaml` templating/rendering strategy
- Use full Jinja rendering support (enterprise-grade behavior), not partial templating.
- Build/runtime environment is expected to be containerized (Docker/Podman) for repeatability.
- Packaging workflow should support bootstrap/install of build prerequisites required for RPM generation.

### Q4 - Default container execution model
- Default container strategy: ephemeral container per build (clean execution environment each run).

### Q5 - Default RPM naming/layout profile
- Default naming and layout follows Phoreus convention:
  - versioned payload: `phoreus-<tool>-<version>`
  - meta/default package: `phoreus-<tool>`

### Q6 - Handling `outputs:` multi-package recipes
- For recipes that define `outputs:`, build all outputs as discrete RPMs.

### Q7 - Version selection in versioned recipe subdirectories
- When versioned subdirectories are present, always select the highest versioned subdirectory.

### Q8 - Missing/unresolvable dependency behavior
- Default behavior: quarantine unresolved packages and continue building the resolvable subset.

### Q9 - Primary CLI command shape
- Primary command: `bioconda2rpm build <package>`.
- Dependency resolution is enabled by default.
- CLI must provide a flag to disable dependency expansion when needed.

### Q10 - Target architecture behavior and SPEC policy
- Default build target is host architecture only.
- Expected operational model: dedicated runners/hosts per architecture.
- Requirement: architecture-specific build instructions must be isolated within a single SPEC using `%ifarch`-style conditionals.
- Requirement: one SPEC file per software package (no per-arch SPEC duplication).

### Q11 - Artifact/output location policy
- Build artifacts must not be written into this software crate repository.
- Bioconda recipe working data must not taint this crate folder.
- Artifact/output location must be explicitly provided by the user (no default in-repo topdir).

### Q12 - Default run reporting outputs
- Default reporting output set:
  - console logs
  - machine-readable JSON summary
  - CSV report
  - Markdown summary report

### Q13 - Licensing/compliance policy
- Default compliance behavior must include SPDX normalization and license policy checks.
- Packages failing license normalization/policy checks are quarantined.

### Q14 - Implementation phase priority
- Phase priority is documentation and CLI skeleton first.
- Parser, dependency resolver, and build pipeline follow after baseline docs/CLI contracts are stable.

### Q15 - Immediate delivery scope
- Deliver both in one pass:
  - Rust crate scaffold + CLAP command structure
  - Requirements documentation pack aligned to decisions
- Include initial unit-test scaffolding with the CLI skeleton.

### 2026-02-27 Amendment - Default topdir and BAD_SPEC location
- Default topdir set to `~/bioconda2rpm` when user does not pass `--topdir`.
- Default BAD_SPEC quarantine directory set to `<topdir>/BAD_SPEC`.
- Tool must create default topdir and BAD_SPEC directories if they do not exist.
- Users can override both with `--topdir` and `--bad-spec-dir`.

### 2026-02-27 Amendment - Containerized build sequence
- Build flow for generated specs must always run in order:
  - SPEC generation
  - SRPM build
  - RPM rebuild from SRPM
- SRPM and RPM stages run inside a user-selected container image.
- CLI now exposes container image selection for this workflow.
- Current reference image in use: `dropworm_dev_almalinux_9_5:0.1.2`.

### 2026-02-27 Amendment - Architecture restriction policy
- Architecture/toolchain incompatibility should be classified explicitly, not treated as a global rollout blocker.
- Example: missing x86 SIMD headers (`emmintrin.h`) on `aarch64` is classified as `amd64_only`.
- Classification is recorded in failure reasons and build logs for traceable compatibility reporting.

### 2026-02-27 Amendment - Phoreus Rust runtime policy
- Introduce a pinned Rust runtime bootstrap package: `phoreus-rust-1.92` (Rust `1.92.0`).
- Rust ecosystem recipe dependencies (`rust`, `rustc`, `cargo`, `rustup`, `rust-*`, `cargo-*`) map to `phoreus-rust-1.92` rather than distro toolchain RPMs.
- Generated payload specs must route Rust/Cargo through `/usr/local/phoreus/rust/1.92`.
- Cargo execution policy remains deterministic and single-core by default.

### 2026-02-27 Amendment - Phoreus Nim runtime policy
- Introduce a Nim runtime bootstrap package: `phoreus-nim-2.2`.
- Nim ecosystem dependencies (`nim`, `nimble`, `nim-*`) map to `phoreus-nim-2.2` rather than distro Nim package names.
- Generated payload specs must route Nim/Nimble through `/usr/local/phoreus/nim/2.2` and isolate nimble state under payload prefix.
- Selector handling distinguishes Linux `aarch64` from `arm64` (macOS) to avoid dropping valid Linux build dependencies.

## 2026-02-28

### Redesign KPI denominator decision
- The >99% first-pass success KPI denominator is defined as the full Bioconda `linux-aarch64` buildable subset.
- This denominator will be used for phase-1 validation reporting and gating.

### Redesign KPI architecture exclusion decision
- The >99% KPI excludes architecture-incompatible packages from the denominator.
- Architecture incompatibility is determined from classified build outcomes (for example `arch_policy=amd64_only` on `aarch64` campaigns).

### Architecture deployment clarification
- Current development campaign architecture is `aarch64`.
- Production deployment target architecture is `amd64` (`x86_64`) on dedicated hosts.
- `build --arch` defines target architecture semantics for metadata/render and classification decisions.

### Metadata adapter strictness policy
- Development profile uses metadata adapter `auto` by default.
- Production profile enforces metadata adapter `conda` for deterministic conda-build semantics.

### Merge gating policy
- Merges to `main` must be blocked when arch-adjusted first-pass success is below `99%`.
- Tooling support is provided through KPI gate options on `build`:
  - `--kpi-gate`
  - `--kpi-min-success-rate` (default `99.0`)

### Heuristic governance policy
- New package-specific heuristics are disallowed unless explicitly temporary.
- Any package-specific heuristic must carry `HEURISTIC-TEMP(issue=...)` with a retirement issue identifier.
- Source-policy tests enforce this requirement in `src/priority_specs.rs`.

### Regression corpus policy
- PR merge gate runs against a top-N priority corpus.
- Nightly regression runs against the full corpus.
- Implemented via `bioconda2rpm regression --mode pr --top-n <N>` and `--mode nightly`.
- Optional curated corpus file support added via `--software-list <path>` (newline-delimited), overriding mode/top-N selection.
