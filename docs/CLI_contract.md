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
- `--missing-dependency <fail|skip|quarantine>`
  - Default: `quarantine`
- `--arch <host|x86-64|aarch64>`
  - Default: `host`
  - Defines target architecture semantics used by metadata rendering and arch-policy classification.
- `--topdir <path>`
  - Optional. Default: `~/bioconda2rpm` (auto-created if missing).
- `--bad-spec-dir <path>`
  - Optional. Default resolves to `<topdir>/BAD_SPEC` (auto-created if missing).
- `--reports-dir <path>`
  - Optional. Default resolves to `<topdir>/reports` (auto-created if missing).
- `--naming-profile <phoreus>`
  - Default: `phoreus`
- `--render-strategy <jinja-full>`
  - Default: `jinja-full`
- `--metadata-adapter <auto|conda|native>`
  - Default: `auto`
  - `auto`: use conda-build render adapter when available, otherwise fallback to native parser.
  - `conda`: require conda-build adapter success.
  - `native`: force native parser.
- `--outputs <all>`
  - Default: `all`

## Baseline Behavior Guarantees

- Dependencies are resolved by default.
- Recipes with `outputs:` are expanded into discrete package outputs.
- Highest versioned recipe subdirectory is selected when present.
- Unresolved dependencies quarantine by default.
- Default quarantine path is `<topdir>/BAD_SPEC`.
- Console + JSON + CSV + Markdown reporting is expected per run.
- Priority SPEC generation uses only Bioconda metadata inputs (`meta.yaml` + `build.sh`) and `tools.csv` priority rows.
- Priority SPEC generation performs overlap resolution and SPEC creation in parallel workers.
- For each generated SPEC, build order is always `SPEC -> SRPM -> RPM` in the selected container image.
- RPM stage is executed as SRPM rebuild (`rpmbuild --rebuild <src.rpm>`).
- Successful package builds clear stale `<topdir>/BAD_SPEC/<tool>.txt` quarantine notes.
- If local payload artifacts already match the requested Bioconda version, `build` exits with `up-to-date` status.
- If Bioconda has a newer payload version than local artifacts, `build` rebuilds payload and bumps default/meta package version.
