# bioconda2rpm CLI Contract (Baseline)

## Primary Command

```bash
bioconda2rpm build <package> --recipe-root <path> --topdir <path>
```

## Required Inputs

- `<package>`: Bioconda package name.
- `--recipe-root <path>`: external path to Bioconda recipes clone.
- `--topdir <path>`: external RPM build output root.

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
- `--reports-dir <path>`
  - Optional. Default resolves to `<topdir>/reports`.
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
- Console + JSON + CSV + Markdown reporting is expected per run.
