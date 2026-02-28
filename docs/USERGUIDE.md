# bioconda2rpm User Guide

## 1. Purpose

`bioconda2rpm` converts Bioconda recipe metadata into Phoreus-style RPM artifacts with dependency-first build ordering.

Primary production workflow:

1. `bioconda2rpm build <tool>`
2. Resolve Bioconda dependency closure
3. Build dependency packages first (SPEC -> SRPM -> RPM)
4. Build requested package last (SPEC -> SRPM -> RPM)

`generate-priority-specs` is a development helper workflow and is not the production entrypoint.

## 2. Prerequisites

- Rust toolchain (`cargo`) available.
- Container engine installed (default: `docker`).
- No external `git` binary requirement for recipe management.
- Priority CSV available (example: `../software_query/tools.csv`).
- Controlled build container profile selected via `--container-profile`:
  - `almalinux-9.7` (default) -> `phoreus/bioconda2rpm-build:almalinux-9.7`
  - `almalinux-10.1` -> `phoreus/bioconda2rpm-build:almalinux-10.1`
  - `fedora-43` -> `phoreus/bioconda2rpm-build:fedora-43`
- If the selected profile image is not present locally, `bioconda2rpm` builds it automatically from `containers/rpm-build-images/`.

## 3. Default Paths

When not overridden:

- `topdir`: `~/bioconda2rpm`
- managed Bioconda repository: `~/bioconda2rpm/bioconda-recipes`
- managed Bioconda recipes root: `~/bioconda2rpm/bioconda-recipes/recipes`
- canonical shared SPEC/SOURCE roots: `~/bioconda2rpm/SPECS`, `~/bioconda2rpm/SOURCES`
- target-scoped quarantine folder: `~/bioconda2rpm/targets/<target-id>/BAD_SPEC`
- target-scoped reports: `~/bioconda2rpm/targets/<target-id>/reports`
- target-scoped binary outputs: `~/bioconda2rpm/targets/<target-id>/SRPMS`, `~/bioconda2rpm/targets/<target-id>/RPMS`

`<target-id>` is derived from `<container-image>-<target-arch>` using a sanitized stable slug.

The tool creates these folders automatically if missing.
On first run, if managed recipes are absent, bioconda2rpm clones `https://github.com/bioconda/bioconda-recipes` automatically.

## 4. Core Commands

### 4.1 Primary Build Command

```bash
cargo run -- build <tool...>
```

Example:

```bash
cargo run -- build bbmap
```

Batch example (multi-root queue):

```bash
cargo run -- build bbmap samtools blast fastqc \
  --sync-recipes \
  --parallel-policy adaptive \
  --build-jobs 4
```

Packages-file example:

```bash
cargo run -- build \
  --packages-file ./docs/verification_software.txt
```

Optional container controls:

```bash
cargo run -- build bbmap \
  --container-profile almalinux-10.1 \
  --container-engine docker
```

Recipe control examples:

```bash
# sync managed recipes repository before build
cargo run -- build bbmap --sync-recipes

# checkout a specific branch/tag/commit
cargo run -- build bbmap --recipe-ref 2025.07.1
```

### 4.2 Development Helper Command (non-production)

```bash
cargo run -- generate-priority-specs \
  --tools-csv ../software_query/tools.csv \
  --container-profile almalinux-9.7
```

### 4.3 Regression Campaign Command

PR corpus (top-N):

```bash
cargo run -- regression \
  --tools-csv ../software_query/tools.csv \
  --mode pr \
  --top-n 25 \
  --deployment-profile production \
  --arch x86-64
```

Curated corpus from a text list (recommended for your essential/key 100):

```bash
cargo run -- regression \
  --tools-csv ../software_query/tools.csv \
  --software-list /path/to/essential_100.txt \
  --deployment-profile production \
  --arch x86-64
```

Nightly full corpus:

```bash
cargo run -- regression \
  --tools-csv ../software_query/tools.csv \
  --mode nightly \
  --deployment-profile production \
  --arch x86-64
```

### 4.4 Recipes Management Command

```bash
# ensure managed clone exists
cargo run -- recipes

# sync latest default branch from origin
cargo run -- recipes --sync

# checkout a specific branch/tag/commit
cargo run -- recipes --recipe-ref 2025.07.1
```

## 5. Required and Important Flags

For `build`:

- `<tool...>` positional: one or more requested Bioconda package names.

Common optional flags:

- `--recipe-root <path>`:
  - optional override for Bioconda recipes root.
  - when omitted, uses managed path `<topdir>/bioconda-recipes/recipes`.
- `--sync-recipes`:
  - fetches latest refs from origin before run.
- `--recipe-ref <branch|tag|commit>`:
  - checks out explicit ref for the recipes repository.
  - implies repository fetch behavior.
- `--container-profile <almalinux-9.7|almalinux-10.1|fedora-43>`:
  - controls the only allowed container images for SRPM/RPM builds.
  - default: `almalinux-9.7`.
  - if local image is missing, it is built automatically from the matching Dockerfile in `containers/rpm-build-images/`.
- `--container-engine <engine>`: default `docker`.
- `--parallel-policy <serial|adaptive>`:
  - `adaptive` (default): run with configured concurrency and retry once in serial on failure.
  - `serial`: force single-core package builds.
- `--build-jobs <N|auto>`:
  - `4` (default): initial jobs per package build in adaptive mode.
  - numeric value: fixed initial job count for adaptive mode.
- `--queue-workers <N>`:
  - optional; controls concurrent package jobs in multi-root queue mode.
  - default auto-calculates from host cores and `--build-jobs`.
- `--packages-file <path>`:
  - optional newline-delimited package roots (supports `#` comments).
  - combined with positional package roots; duplicates are deduplicated.
- `--topdir <path>`: artifact/report root override.
- `--bad-spec-dir <path>`: quarantine override.
- `--reports-dir <path>`: report directory override.
- `--no-deps`: disable Bioconda dependency closure.
- `--dependency-policy <run-only|build-host-run|runtime-transitive-root-build-host>`.
- `--metadata-adapter <auto|conda|native>`:
  - `auto` (default): try conda-build rendering first, then fallback to native parser.
  - `conda`: require conda-build adapter success.
  - `native`: use in-crate selector/Jinja parser only.
- `--deployment-profile <development|production>`:
  - `development` (default): honors selected `--metadata-adapter` (default `auto`).
  - `production`: forces effective metadata adapter to `conda`.
- `--kpi-gate`:
  - enables hard arch-adjusted KPI gate for the current run.
- `--kpi-min-success-rate <float>`:
  - default `99.0`; run fails when KPI falls below threshold while gate is active.
- `--mode <pr|nightly>` (regression command):
  - `pr`: top-N priority tools from `tools.csv`
  - `nightly`: full corpus from `tools.csv`
- `--top-n <N>` (regression command):
  - top-N size for PR mode (default `25`).
- `--software-list <path>` (regression command):
  - newline-delimited software list that overrides `--mode`/`--top-n`.
  - comments using `#` and blank lines are allowed.
  - list order is preserved for campaign execution.
- `--arch <host|x86-64|aarch64>`:
  - sets target architecture semantics for metadata/render and compatibility classification.
  - recommended usage: `aarch64` for current development campaigns, `x86-64` for production validation.

Up-to-date behavior:

- If the requested Bioconda version is already present as a built payload artifact in `<topdir>`, the command exits without rebuilding and reports `up-to-date`.
- If Bioconda has a newer version than the latest local payload artifact, the payload is rebuilt and the default/meta package version is incremented.

## 6. Build Sequence Details

Per `build <tool>` run:

1. Resolve requested recipe from Bioconda metadata.
2. Resolve Bioconda dependency closure (unless `--no-deps`).
3. Build dependencies first.
4. In multi-root mode, dispatch ready nodes through a worker queue:
   - dependency gate enforced per node
   - a node builds only after all upstream Bioconda dependency nodes succeed
   - blocked descendants are quarantined/skipped according to missing-dependency policy
5. For each package:
   - Resolve/prepare sources from recipe metadata.
   - Stage and apply any `source.patches` entries during `%prep`.
   - Build SRPM inside container (`rpmbuild -bs`).
   - Preflight `BuildRequires` inside container:
     - already installed packages
     - local RPM reuse from `<topdir>/targets/<target-id>/RPMS` (plus legacy `<topdir>/RPMS` compatibility read)
     - distro/core repos with unavailable-repo tolerance
   - If `--parallel-policy adaptive` is active:
     - first attempt uses `--build-jobs`
     - failed attempts automatically retry once with single-core settings
     - successful serial retries are recorded in per-target stability cache for future runs
   - Rebuild RPM from SRPM (`rpmbuild --rebuild <generated.src.rpm>`).

For each package build step:

1. Resolve/prepare sources from Bioconda metadata.
2. Build SRPM inside container:
   - `rpmbuild -bs`
3. Preflight `BuildRequires` inside container:
   - Use installed packages when already present.
   - Reuse matching local RPMs from `<topdir>/targets/<target-id>/RPMS`.
   - Install remaining requirements from configured repos (with unavailable-repo tolerance).
4. Rebuild RPM from SRPM inside container:
   - `rpmbuild --rebuild <generated.src.rpm>`

This enforces an auditable SRPM-to-RPM lineage.

Python charter behavior:

- For all Python software installs (detected from recipe metadata and staged `build.sh` patterns such as `pip install` / `python -m pip` / `setup.py install`), `bioconda2rpm` builds a hermetic virtual environment under `/usr/local/phoreus/<tool>/<version>/venv`.
- Python dependency trees are solved inside that venv using `pip-compile --generate-hashes` and `pip install --require-hashes`.
- Python library dependencies are not emitted as shared RPM `Requires`; payload runtime requires are limited to `phoreus` and `phoreus-python-3.11`.
- This avoids unresolved distro RPM names such as `jinja2`/`rich` and follows the Python RPM charter isolation model.

R charter behavior:

- `bioconda2rpm` provisions `phoreus-r-4.5.2` on demand when a recipe or dependency graph references R ecosystem dependencies (`r`, `r-base`, `r-*`, `bioconductor-*`).
- R ecosystem dependencies are mapped to `phoreus-r-4.5.2` instead of distro `R-*` RPMs, and are not pushed into `pip` lock generation.
- For R project recipes, the generated SPEC exports `R_HOME`/`R_LIBS_USER` into an isolated tool prefix and performs `renv::restore()` when `renv.lock` is present.

Rust charter behavior:

- `bioconda2rpm` provisions `phoreus-rust-1.92` on demand when a recipe or dependency graph references Rust ecosystem dependencies (`rust`, `rustc`, `cargo`, `rustup`, `rust-*`, `cargo-*`) or staged `build.sh` rust/cargo usage.
- Rust ecosystem dependencies are mapped to `phoreus-rust-1.92` instead of distro Rust toolchain RPMs.
- Generated SPECs route all Rust/Cargo execution through `/usr/local/phoreus/rust/1.92` and export deterministic cargo settings rooted in orchestrator policy (`CARGO_INCREMENTAL=0`; job count from adaptive/serial settings).

Nim runtime behavior:

- `bioconda2rpm` provisions `phoreus-nim-2.2` on demand when a recipe dependency set references Nim ecosystem packages (`nim`, `nimble`, `nim-*`).
- Nim ecosystem dependencies are mapped to `phoreus-nim-2.2` instead of distro Nim package names.
- Generated SPECs route Nim/Nimble execution through `/usr/local/phoreus/nim/2.2` and set `NIMBLE_DIR` under the payload prefix for isolated builds.

Precompiled binary policy:

- `bioconda2rpm` supports package-specific precompiled-binary overrides when upstream guidance recommends binary consumption over source builds.
- For `k8`, the build path is forced to use upstream precompiled release archives instead of compiling Node/V8 from source.
- If a requested architecture has no upstream precompiled binary, the package is quarantined with explicit architecture policy classification.

## 7. Output Layout

Under `<topdir>`:

- `SPECS/` generated payload + meta SPEC files
- `SOURCES/` staged `build.sh` and downloaded source archives
- `targets/<target-id>/SRPMS/` generated source RPMs for that build target
- `targets/<target-id>/RPMS/` rebuilt binary RPMs for that build target
- `targets/<target-id>/reports/build_<tool>.json`
- `targets/<target-id>/reports/build_<tool>.csv`
- `targets/<target-id>/reports/build_<tool>.md`
- `targets/<target-id>/reports/dependency_graphs/*.json` per-package dependency resolution graph
- `targets/<target-id>/reports/dependency_graphs/*.md` per-package dependency resolution graph
- `targets/<target-id>/reports/build_stability.json` learned package-level concurrency compatibility cache (`parallel_unstable`)
- `targets/<target-id>/BAD_SPEC/` quarantine notes for failed/unresolved items

This layout keeps one canonical SPEC set while isolating binary artifacts by build OS target. Use one SPEC with `%ifarch` / distro conditionals when needed.

When a package builds successfully (or is confirmed up-to-date), stale `<topdir>/targets/<target-id>/BAD_SPEC/<tool>.txt` notes are removed for that package.

## 8. Reports and Status Interpretation

Each report entry includes:

- `software`
- `priority`
- `status` (`generated` or `quarantined`)
- overlap resolution details
- spec paths and staged build script path
- reason/message

Use the Markdown report for quick review and JSON/CSV for automation.
For dependency analysis, inspect `targets/<target-id>/reports/dependency_graphs/`:
- `status=resolved` entries include `source` (`installed`, `local_rpm`, `repo`).
- `status=unresolved` entries include captured package-manager detail.
- Build Markdown reports include an arch-adjusted reliability KPI block where architecture-incompatible packages are excluded from denominator.

Generated payload RPMs include `Provides: <tool>` (for example `Provides: samtools`) so downstream builds can consume previously generated local RPMs when available.

Architecture restriction policy:

- If build logs show known intrinsic/header incompatibilities (for example `emmintrin.h` on `aarch64`), the run classifies the package as architecture-restricted.
- Example classification: `arch_policy=amd64_only`.
- This is treated as a package-level compatibility outcome, not a global run blocker.

## 9. Troubleshooting

### Container engine not found

Set engine explicitly or install Docker:

```bash
--container-engine docker
```

### Build fails during RPM rebuild

Check:

- `<topdir>/targets/<target-id>/reports/build_<tool>.md`
- `<topdir>/targets/<target-id>/reports/dependency_graphs/<tool>.md`
- `<topdir>/targets/<target-id>/BAD_SPEC/<tool>.txt`
- container logs printed during run

Failures typically reflect recipe/toolchain incompatibilities for the target architecture, not workflow failure.
When detected, the error reason includes `arch_policy=...` to capture compatibility constraints.
If failure is dependency-related, the reason includes dependency graph paths and unresolved dependency names.

### Wrong or missing sources

Ensure network access is available for `spectool -g -R` to fetch `Source0`.

## 10. Recommended Enterprise Run Pattern

1. Run `build <tool>` in a clean dedicated topdir.
2. Keep one topdir per build campaign/date.
3. Archive JSON/CSV/MD reports with produced SRPM/RPM artifacts.
4. Use dedicated hosts/runners per architecture while keeping one SPEC per software with `%ifarch` gating.

Merge-gate invocation example:

```bash
cargo run -- build <tool> \
  --deployment-profile production \
  --arch x86-64 \
  --kpi-gate \
  --kpi-min-success-rate 99.0
```

Contributor policy note:
- New package-specific heuristics are not allowed unless temporary and tagged with `HEURISTIC-TEMP(issue=...)`.
- Untagged package-specific heuristic blocks fail test checks.
