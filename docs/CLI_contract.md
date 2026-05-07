# bioconda2rpm CLI Contract (Baseline)

## Primary Command

```bash
bioconda2rpm build <package...>
```

Production expectation:
- `build` is the canonical end-user command.
- Build order is dependency-first for Bioconda packages, then target package.
- Stage order per package is `SPEC -> SRPM -> RPM`.

## Persistent Server Command

```bash
bioconda2rpm server [build-options] [<initial-package...>]
```

Production expectation:
- `server` owns the workspace build lock for the selected target id.
- It launches the progress TUI and remains alive after each build batch completes.
- It drains forwarded requests from secondary `build` invocations until Ctrl-C is provided by the user.
- Initial packages are optional; without them the server starts idle and waits for forwarded work.

## Remove Command

```bash
bioconda2rpm remove [--topdir <path>] [--target-id <target-id>] [--queue-only] <package...>
```

Production expectation:
- `remove` prunes matching packages from the pending forwarded request queue.
- It submits an active remove request for the owning build/server process so in-memory queued nodes are skipped.
- Unless `--queue-only` is set, it stops matching running `bioconda2rpm-<package>-...` package build containers to free worker capacity.
- Removal targets the same derived `<target-id>` semantics as `build`, unless `--target-id` is provided explicitly.

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

## Catalog Management Command

```bash
bioconda2rpm catalog migrate [--topdir <path>] [--json]
```

Production expectation:
- `catalog migrate` upgrades old flat `.catalog.json` files to the current package/version/build schema.
- Migration preserves the compatibility `entries` list used by `list` and `failures`, while adding nested package/version/build provenance.
- Migration injects discovered SRPMs from `<topdir>/targets/*/SRPMS/*.src.rpm` and records an authoritative SRPM for each matching package version even when no binary RPM has been produced.
- Missing host/user/date values are inferred from the current host and existing file metadata where available.

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
- Workspace-lock ownership is authoritative: secondary `build` invocations submit package names into the active session queue instead of failing lock-acquisition.
- Forwarded packages carry `--force` into the authoritative queue for that request and otherwise inherit the owning session scheduler/container settings.
- The persistent `server` process does not own or broadcast `--force`; submit `bioconda2rpm build --force <package>` to force a package through a running server.
- `--force` bypasses blacklist quarantine only for the requested package and its dependency closure; recipe-declared `build.skip=true` still records a skipped result because the active render context has no buildable recipe output.
- Persistent `server` ownership keeps the queue interface available between batches and exits only on user Ctrl-C.
- Operator removals are explicit queue events; removed package nodes are reported as skipped and their dependents are blocked by normal dependency-gate handling.
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
- SRPMs are copied to `<topdir>/targets/<target-id>/SRPMS` as soon as `rpmbuild -bs` succeeds so the catalog can record an SRPM-prepared state independently of later binary RPM success.
- Adaptive mode records package-level `parallel_unstable` outcomes in `<topdir>/targets/<target-id>/reports/build_stability.json` and forces serial first pass on subsequent runs for those specs.
- Successful package builds clear stale `<topdir>/targets/<target-id>/BAD_SPEC/<tool>.txt` quarantine notes.
- If local payload artifacts already match the requested Bioconda version, `build` exits with `up-to-date` status.
- If Bioconda has a newer payload version than local artifacts, `build` rebuilds payload and bumps default/meta package version.
- Package-specific heuristics require explicit temporary tagging with a retirement issue (`HEURISTIC-TEMP(issue=...)`) and are test-enforced.
- Managed recipe repository operations do not require a system `git` binary.
