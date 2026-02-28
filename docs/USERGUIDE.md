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
- Bioconda recipes clone available (example: `../bioconda-recipes/recipes`).
- Priority CSV available (example: `../software_query/tools.csv`).
- Build container image available locally or pullable, e.g.:
  - `dropworm_dev_almalinux_9_5:0.1.2`

## 3. Default Paths

When not overridden:

- `topdir`: `~/bioconda2rpm`
- quarantine folder: `~/bioconda2rpm/BAD_SPEC`
- reports: `~/bioconda2rpm/reports`

The tool creates these folders automatically if missing.

## 4. Core Commands

### 4.1 Primary Build Command

```bash
cargo run -- build <tool> --recipe-root <path/to/recipes>
```

Example:

```bash
cargo run -- build bbmap --recipe-root ../bioconda-recipes/recipes
```

Optional container controls:

```bash
cargo run -- build bbmap \
  --recipe-root ../bioconda-recipes/recipes \
  --container-image dropworm_dev_almalinux_9_5:0.1.2 \
  --container-engine docker
```

### 4.2 Development Helper Command (non-production)

```bash
cargo run -- generate-priority-specs \
  --recipe-root ../bioconda-recipes/recipes \
  --tools-csv ../software_query/tools.csv \
  --container-image dropworm_dev_almalinux_9_5:0.1.2
```

## 5. Required and Important Flags

For `build`:

- `--recipe-root <path>`: Bioconda recipe tree root.
- `<tool>` positional: requested Bioconda package name.

Common optional flags:

- `--container-image <image:tag>`: image used for SRPM/RPM builds.
- `--container-engine <engine>`: default `docker`.
- `--topdir <path>`: artifact/report root override.
- `--bad-spec-dir <path>`: quarantine override.
- `--reports-dir <path>`: report directory override.
- `--no-deps`: disable Bioconda dependency closure.
- `--dependency-policy <run-only|build-host-run|runtime-transitive-root-build-host>`.
- `--metadata-adapter <auto|conda|native>`:
  - `auto` (default): try conda-build rendering first, then fallback to native parser.
  - `conda`: require conda-build adapter success.
  - `native`: use in-crate selector/Jinja parser only.
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
3. Build dependencies first in deterministic order.
4. For each package:
   - Resolve/prepare sources from recipe metadata.
   - Stage and apply any `source.patches` entries during `%prep`.
   - Build SRPM inside container (`rpmbuild -bs`).
   - Preflight `BuildRequires` inside container:
     - already installed packages
     - local RPM reuse from `<topdir>/RPMS`
     - distro/core repos with unavailable-repo tolerance
   - Rebuild RPM from SRPM (`rpmbuild --rebuild <generated.src.rpm>`).

For each package build step:

1. Resolve/prepare sources from Bioconda metadata.
2. Build SRPM inside container:
   - `rpmbuild -bs`
3. Preflight `BuildRequires` inside container:
   - Use installed packages when already present.
   - Reuse matching local RPMs from `<topdir>/RPMS`.
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
- Generated SPECs route all Rust/Cargo execution through `/usr/local/phoreus/rust/1.92` and export deterministic cargo settings (`CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`) for reproducible builds.

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
- `SRPMS/` generated source RPMs
- `RPMS/` rebuilt binary RPMs
- `reports/build_<tool>.json`
- `reports/build_<tool>.csv`
- `reports/build_<tool>.md`
- `reports/dependency_graphs/*.json` per-package dependency resolution graph
- `reports/dependency_graphs/*.md` per-package dependency resolution graph
- `BAD_SPEC/` quarantine notes for failed/unresolved items

When a package builds successfully (or is confirmed up-to-date), stale `<topdir>/BAD_SPEC/<tool>.txt` notes are removed for that package.

## 8. Reports and Status Interpretation

Each report entry includes:

- `software`
- `priority`
- `status` (`generated` or `quarantined`)
- overlap resolution details
- spec paths and staged build script path
- reason/message

Use the Markdown report for quick review and JSON/CSV for automation.
For dependency analysis, inspect `reports/dependency_graphs/`:
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

- `<topdir>/reports/build_<tool>.md`
- `<topdir>/reports/dependency_graphs/<tool>.md`
- `<topdir>/BAD_SPEC/<tool>.txt`
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
