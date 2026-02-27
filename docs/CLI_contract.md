# bioconda2rpm CLI Contract (Baseline)

## Primary Command

```bash
bioconda2rpm build <package> --recipe-root <path>
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
- `--missing-dependency <fail|skip|quarantine>`
  - Default: `quarantine`
- `--arch <host|x86-64|aarch64>`
  - Default: `host`
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
- `--outputs <all>`
  - Default: `all`

## Baseline Behavior Guarantees

- Dependencies are resolved by default.
- Recipes with `outputs:` are expanded into discrete package outputs.
- Highest versioned recipe subdirectory is selected when present.
- Unresolved dependencies quarantine by default.
- Default quarantine path is `<topdir>/BAD_SPEC`.
- Console + JSON + CSV + Markdown reporting is expected per run.
