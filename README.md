# bioconda2rpm

Rust CLI to convert Bioconda recipes into Phoreus-style RPM artifacts.

## Baseline CLI

```bash
cargo run -- build <package...> --recipe-root <path/to/bioconda-recipes/recipes>
```

Batch queue example:

```bash
cargo run -- build bbmap samtools blast fastqc \
  --recipe-root ../bioconda-recipes/recipes \
  --parallel-policy adaptive \
  --build-jobs 4
```

## Priority SPEC Generation (Parallel)

Generate Phoreus payload/meta SPEC pairs for top-priority tools directly from Bioconda metadata:

```bash
cargo run -- generate-priority-specs \
  --recipe-root ../bioconda-recipes/recipes \
  --tools-csv ../software_query/tools.csv \
  --container-profile almalinux-9.7 \
  --top-n 10 \
  --workers 6
```

Outputs are written under `--topdir` (default `~/bioconda2rpm`):
- `SPECS/` generated payload + meta SPEC files
- `SOURCES/` staged `build.sh` sources from Bioconda recipes
- `targets/<target-id>/SRPMS/` generated source RPMs for the selected build target
- `targets/<target-id>/RPMS/` generated binary RPMs for the selected build target
- `targets/<target-id>/reports/priority_spec_generation.{json,csv,md}`
- `targets/<target-id>/BAD_SPEC/` quarantine notes for unresolved/invalid entries

`<target-id>` is derived from `<container-image>-<target-arch>` using a sanitized stable slug.  
`<container-image>` is resolved from controlled `--container-profile` values:
- `almalinux-9.7` (default) -> `phoreus/bioconda2rpm-build:almalinux-9.7`
- `almalinux-10.1` -> `phoreus/bioconda2rpm-build:almalinux-10.1`
- `fedora-43` -> `phoreus/bioconda2rpm-build:fedora-43`

Behavior:
- Build chain is always: `SPEC -> SRPM -> RPM`.
- SRPM and RPM build stages run inside the selected controlled container profile image.
- If the selected image is missing locally, `bioconda2rpm` builds it automatically from `containers/rpm-build-images/`.
- One canonical SPEC set is reused across OS targets; build outputs are target-scoped.
- Build concurrency is policy-driven:
  - `--parallel-policy adaptive` (default): parallel first attempt, automatic serial retry, learned stability cache
  - `--parallel-policy serial`: force single-core builds

Defaults when omitted:
- `--topdir` -> `~/bioconda2rpm`
- `BAD_SPEC` quarantine -> `~/bioconda2rpm/targets/<target-id>/BAD_SPEC`
- reports -> `~/bioconda2rpm/targets/<target-id>/reports`

`--topdir` and `--bad-spec-dir` can be provided to override these paths.

## Documentation

- `docs/RPM_charter.md`
- `docs/Python_RPM_charter.md`
- `docs/R_RPM_charter.md`
- `docs/Rust_RPM_charter.md`
- `docs/USERGUIDE.md`
- `docs/SRS.md`
- `docs/ARD.md`
- `docs/CLI_contract.md`
- `docs/dependency_policy.md`
- `docs/decision_log.md`

## Container Build Images

Reference multi-arch (`amd64` + `arm64`) build image Dockerfiles are in:

- `containers/rpm-build-images/`
