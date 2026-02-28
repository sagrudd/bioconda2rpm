# bioconda2rpm

Rust CLI to convert Bioconda recipes into Phoreus-style RPM artifacts.

## Baseline CLI

```bash
cargo run -- build <package> --recipe-root <path/to/bioconda-recipes/recipes>
```

## Priority SPEC Generation (Parallel)

Generate Phoreus payload/meta SPEC pairs for top-priority tools directly from Bioconda metadata:

```bash
cargo run -- generate-priority-specs \
  --recipe-root ../bioconda-recipes/recipes \
  --tools-csv ../software_query/tools.csv \
  --container-image dropworm_dev_almalinux_9_5:0.1.2 \
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

Behavior:
- Build chain is always: `SPEC -> SRPM -> RPM`.
- SRPM and RPM build stages run inside the specified container image.
- One canonical SPEC set is reused across OS targets; build outputs are target-scoped.

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
