# bioconda2rpm CLI Contract (Baseline)

## Primary Command

```bash
bioconda2rpm build <package...>
```

Production expectation:
- `build` is the canonical end-user command.
- Build order is dependency-first for Bioconda packages, then target package.
- Stage order per package is `SPEC -> SRPM -> RPM`.

## Priority SPEC Generation Command

```bash
bioconda2rpm generate-priority-specs \
  --tools-csv <path/to/tools.csv> \
  [--recipe-root <path>] \
  [--sync-recipes] \
  [--recipe-ref <branch|tag|commit>] \
  [--container-profile <almalinux-9.7|almalinux-10.1|fedora-43>] \
  [--top-n 10] \
  [--workers <n>] \
  [--container-engine docker]
```

## Regression Campaign Command

```bash
bioconda2rpm regression \
  --tools-csv <path/to/tools.csv> \
  [--recipe-root <path>] \
  [--sync-recipes] \
  [--recipe-ref <branch|tag|commit>] \
  [--software-list <path/to/software.txt>] \
  [--mode pr|nightly] \
  [--top-n 25]
```

## Recipes Management Command

```bash
bioconda2rpm recipes [--topdir <path>] [--recipe-root <path>] [--sync] [--recipe-ref <branch|tag|commit>]
```

## Required Inputs

- `<package...>`: one or more Bioconda package names.
- Bioconda recipes input is optional:
  - default managed clone path: `<topdir>/bioconda-recipes/recipes`
  - first run auto-clones `https://github.com/bioconda/bioconda-recipes`

## Core Options

- `--stage <spec|srpm|rpm>`
  - Default: `rpm`
- `--dependency-policy <run-only|build-host-run|runtime-transitive-root-build-host>`
  - Default: `build-host-run`
- `--no-deps`
  - Disables dependency closure for the requested package.
- `--recipe-root <path>`
  - Optional override for recipes root.
- `--sync-recipes`
  - Fetches latest refs from origin before command execution.
- `--recipe-ref <branch|tag|commit>`
  - Checks out explicit repository ref; implies repository fetch.
- `--container-mode <ephemeral|running|auto>`
  - Default: `ephemeral`
- `--container-profile <almalinux-9.7|almalinux-10.1|fedora-43>`
  - Optional for `build`.
  - Default: `almalinux-9.7`.
  - Resolves to controlled build images only:
    - `almalinux-9.7` -> `phoreus/bioconda2rpm-build:almalinux-9.7`
    - `almalinux-10.1` -> `phoreus/bioconda2rpm-build:almalinux-10.1`
    - `fedora-43` -> `phoreus/bioconda2rpm-build:fedora-43`
  - If selected image is missing locally, bioconda2rpm builds it automatically from `containers/rpm-build-images/`.
- `--container-engine <docker|podman|...>`
  - Optional. Default: `docker`.
- `--parallel-policy <serial|adaptive>`
  - Default: `adaptive`
  - `serial`: enforce single-core package builds.
  - `adaptive`: attempt configured parallel build first and auto-retry serial on failure.
- `--build-jobs <N|auto>`
  - Default: `4`
  - Sets initial build job count for adaptive mode.
- `--queue-workers <N>`
  - Optional. Default: `floor(host_cores / effective_build_jobs)`, minimum `1`.
  - Controls how many package build jobs run concurrently in multi-package queue mode.
- `--packages-file <path>`
  - Optional newline-delimited package list (supports `#` comments).
  - Combined with positional package args; duplicates are deduplicated.
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
  - Derived as a deterministic sanitized slug from the resolved `<container-image>-<target-arch>`.
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
- Multiple requested roots are supported in one build invocation.
- Multi-package queue mode enforces dependency gates: a package is dispatched only after its Bioconda dependency nodes succeed.
- Recipes with `outputs:` are expanded into discrete package outputs.
- Highest versioned recipe subdirectory is selected when present.
- Unresolved dependencies quarantine by default.
- One canonical SPEC/SOURCE set is shared under `<topdir>/SPECS` and `<topdir>/SOURCES`.
- SRPM/RPM/report/quarantine artifacts are isolated under `<topdir>/targets/<target-id>/...`.
- Default quarantine path is `<topdir>/targets/<target-id>/BAD_SPEC`.
- Console + JSON + CSV + Markdown reporting is expected per run.
- Priority SPEC generation uses only Bioconda metadata inputs (`meta.yaml` + `build.sh`) and `tools.csv` priority rows.
- Priority SPEC generation performs overlap resolution and SPEC creation in parallel workers.
- For each generated SPEC, build order is always `SPEC -> SRPM -> RPM` in the selected controlled container profile image.
- RPM stage is executed as SRPM rebuild (`rpmbuild --rebuild <src.rpm>`).
- Adaptive mode records package-level `parallel_unstable` outcomes in `<topdir>/targets/<target-id>/reports/build_stability.json` and forces serial first pass on subsequent runs for those specs.
- Successful package builds clear stale `<topdir>/targets/<target-id>/BAD_SPEC/<tool>.txt` quarantine notes.
- If local payload artifacts already match the requested Bioconda version, `build` exits with `up-to-date` status.
- If Bioconda has a newer payload version than local artifacts, `build` rebuilds payload and bumps default/meta package version.
- Package-specific heuristics require explicit temporary tagging with a retirement issue (`HEURISTIC-TEMP(issue=...)`) and are test-enforced.
- Managed recipe repository operations do not require a system `git` binary.
