# bioconda2rpm CLI Contract (Baseline)

## Primary Command

```bash
bioconda2rpm build <package> --recipe-root <path>
```

Production expectation:
- `build` is the canonical end-user command.
- Build order is dependency-first for Bioconda packages, then target package.
- Stage order per package is `SPEC -> SRPM -> RPM`.

## Priority SPEC Generation Command

```bash
bioconda2rpm generate-priority-specs \
  --recipe-root <path> \
  --tools-csv <path/to/tools.csv> \
  --container-image <image[:tag]> \
  [--top-n 10] \
  [--workers <n>] \
  [--container-engine docker]
```

## Regression Campaign Command

```bash
bioconda2rpm regression \
  --recipe-root <path> \
  --tools-csv <path/to/tools.csv> \
  [--software-list <path/to/software.txt>] \
  [--mode pr|nightly] \
  [--top-n 25]
```

## Required Inputs

- `<package>`: Bioconda package name.
- `--recipe-root <path>`: external path to Bioconda recipes clone.

## Core Options

- `--stage <spec|srpm|rpm>`
  - Default: `rpm`
- `--dependency-policy <run-only|build-host-run|runtime-transitive-root-build-host>`
  - Default: `build-host-run`
- `--no-deps`
  - Disables dependency closure for the requested package.
- `--container-mode <ephemeral|running|auto>`
  - Default: `ephemeral`
- `--container-image <image[:tag]>`
  - Optional for `build`.
  - Default: `dropworm_dev_almalinux_9_5:0.1.2`.
  - Used to execute SRPM/RPM builds in-container.
- `--container-engine <docker|podman|...>`
  - Optional. Default: `docker`.
- `--parallel-policy <serial|adaptive>`
  - Default: `adaptive`
  - `serial`: enforce single-core package builds.
  - `adaptive`: attempt configured parallel build first and auto-retry serial on failure.
- `--build-jobs <N|auto>`
  - Default: `auto`
  - Sets initial build job count for adaptive mode.
- `--missing-dependency <fail|skip|quarantine>`
  - Default: `quarantine`
- `--arch <host|x86-64|aarch64>`
  - Default: `host`
  - Defines target architecture semantics used by metadata rendering and arch-policy classification.
- `--topdir <path>`
  - Optional. Default: `~/bioconda2rpm` (auto-created if missing).
- `--bad-spec-dir <path>`
  - Optional. Default resolves to `<topdir>/targets/<target-id>/BAD_SPEC` (auto-created if missing).
- `--reports-dir <path>`
  - Optional. Default resolves to `<topdir>/targets/<target-id>/reports` (auto-created if missing).
- `<target-id>`
  - Derived as a deterministic sanitized slug from `<container-image>-<target-arch>`.
- `--naming-profile <phoreus>`
  - Default: `phoreus`
- `--render-strategy <jinja-full>`
  - Default: `jinja-full`
- `--metadata-adapter <auto|conda|native>`
  - Default: `auto`
  - `auto`: use conda-build render adapter when available, otherwise fallback to native parser.
  - `conda`: require conda-build adapter success.
  - `native`: force native parser.
- `--deployment-profile <development|production>`
  - Default: `development`
  - `production` enforces effective metadata adapter `conda`.
- `--kpi-gate`
  - Enables hard arch-adjusted KPI gate for the run.
- `--kpi-min-success-rate <float>`
  - Default: `99.0`
  - Run fails when arch-adjusted success rate is below this threshold while KPI gate is active.
- `--outputs <all>`
  - Default: `all`

Regression-only options:
- `--software-list <path>`
  - Optional newline-delimited software corpus.
  - Overrides `--mode`/`--top-n` selection when provided.
- `--mode <pr|nightly>`
  - `pr`: top-N priority corpus
  - `nightly`: full corpus
- `--top-n <n>`
  - Used by PR mode.

## Baseline Behavior Guarantees

- Dependencies are resolved by default.
- Recipes with `outputs:` are expanded into discrete package outputs.
- Highest versioned recipe subdirectory is selected when present.
- Unresolved dependencies quarantine by default.
- One canonical SPEC/SOURCE set is shared under `<topdir>/SPECS` and `<topdir>/SOURCES`.
- SRPM/RPM/report/quarantine artifacts are isolated under `<topdir>/targets/<target-id>/...`.
- Default quarantine path is `<topdir>/targets/<target-id>/BAD_SPEC`.
- Console + JSON + CSV + Markdown reporting is expected per run.
- Priority SPEC generation uses only Bioconda metadata inputs (`meta.yaml` + `build.sh`) and `tools.csv` priority rows.
- Priority SPEC generation performs overlap resolution and SPEC creation in parallel workers.
- For each generated SPEC, build order is always `SPEC -> SRPM -> RPM` in the selected container image.
- RPM stage is executed as SRPM rebuild (`rpmbuild --rebuild <src.rpm>`).
- Adaptive mode records package-level `parallel_unstable` outcomes in `<topdir>/targets/<target-id>/reports/build_stability.json` and forces serial first pass on subsequent runs for those specs.
- Successful package builds clear stale `<topdir>/targets/<target-id>/BAD_SPEC/<tool>.txt` quarantine notes.
- If local payload artifacts already match the requested Bioconda version, `build` exits with `up-to-date` status.
- If Bioconda has a newer payload version than local artifacts, `build` rebuilds payload and bumps default/meta package version.
- Package-specific heuristics require explicit temporary tagging with a retirement issue (`HEURISTIC-TEMP(issue=...)`) and are test-enforced.
