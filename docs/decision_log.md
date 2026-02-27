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
