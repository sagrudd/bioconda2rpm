# bioconda2rpm User Guide

## 1. Purpose

`bioconda2rpm` converts Bioconda recipe metadata into Phoreus-style RPM packaging artifacts and executes builds in a containerized, reproducible sequence:

1. Generate SPEC files
2. Build SRPM from SPEC
3. Rebuild RPM from SRPM

For priority-driven batch generation, the tool selects top tools from `tools.csv` using `RPM Priority Score`.

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

### 4.1 Build Command (single package workflow scaffold)

```bash
cargo run -- build <package> --recipe-root <path/to/recipes>
```

Example:

```bash
cargo run -- build bwa --recipe-root ../bioconda-recipes/recipes
```

### 4.2 Priority SPEC/SRPM/RPM Generation (parallel)

```bash
cargo run -- generate-priority-specs \
  --recipe-root ../bioconda-recipes/recipes \
  --tools-csv ../software_query/tools.csv \
  --container-image dropworm_dev_almalinux_9_5:0.1.2 \
  --workers 6 \
  --top-n 10
```

## 5. Required and Important Flags

For `generate-priority-specs`:

- `--recipe-root <path>`: Bioconda recipe tree root.
- `--tools-csv <path>`: CSV containing `RPM Priority Score`.
- `--container-image <image:tag>`: image used for SRPM/RPM builds.

Common optional flags:

- `--container-engine <engine>`: default `docker`.
- `--top-n <n>`: number of highest priority tools to process (default `10`).
- `--workers <n>`: parallel worker count.
- `--topdir <path>`: artifact/report root override.
- `--bad-spec-dir <path>`: quarantine override.
- `--reports-dir <path>`: report directory override.

## 6. Build Sequence Details

Per generated SPEC:

1. Resolve/prepare sources from Bioconda metadata.
2. Build SRPM inside container:
   - `rpmbuild -bs`
3. Rebuild RPM from SRPM inside container:
   - `rpmbuild --rebuild <generated.src.rpm>`

This enforces an auditable SRPM-to-RPM lineage.

## 7. Output Layout

Under `<topdir>`:

- `SPECS/` generated payload + meta SPEC files
- `SOURCES/` staged `build.sh` and downloaded source archives
- `SRPMS/` generated source RPMs
- `RPMS/` rebuilt binary RPMs
- `reports/priority_spec_generation.json`
- `reports/priority_spec_generation.csv`
- `reports/priority_spec_generation.md`
- `BAD_SPEC/` quarantine notes for failed/unresolved items

## 8. Reports and Status Interpretation

Each report entry includes:

- `software`
- `priority`
- `status` (`generated` or `quarantined`)
- overlap resolution details
- spec paths and staged build script path
- reason/message

Use the Markdown report for quick review and JSON/CSV for automation.

## 9. Troubleshooting

### Container engine not found

Set engine explicitly or install Docker:

```bash
--container-engine docker
```

### Build fails during RPM rebuild

Check:

- `<topdir>/reports/priority_spec_generation.md`
- `<topdir>/BAD_SPEC/<tool>.txt`
- container logs printed during run

Failures typically reflect recipe/toolchain incompatibilities for the target architecture, not workflow failure.

### Wrong or missing sources

Ensure network access is available for `spectool -g -R` to fetch `Source0`.

## 10. Recommended Enterprise Run Pattern

1. Run generation in a clean dedicated topdir.
2. Keep one topdir per build campaign/date.
3. Archive JSON/CSV/MD reports with produced SRPM/RPM artifacts.
4. Use dedicated hosts/runners per architecture while keeping one SPEC per software with `%ifarch` gating.
