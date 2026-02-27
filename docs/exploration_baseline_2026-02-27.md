# bioconda2rpm Exploration Baseline (2026-02-27)

## Context
- Target project: `bioconda2rpm` (Rust CLI; CLAP planned for argument parsing).
- Goal: convert Bioconda recipes into discrete RPMs, including dependency closure for requested software.
- Primary upstream input source: `../bioconda-recipes/recipes/` (`meta.yaml` and `build.sh`).

## What Exists in This Repo
- Minimal starter repository with only `README.md` and license.
- No Rust crate scaffold, parser, or packaging pipeline code yet.

## External Inputs Verified
### Bioconda recipes clone
- Path exists: `../bioconda-recipes/recipes/`.
- Approximate recipe count (by `meta.yaml` files): `10,271`.
- `build.sh` present in many recipes; recipes may be simple or multi-output.
- `meta.yaml` includes Jinja templating and sections such as:
  - `package`, `source`, `build`, `requirements` (`build/host/run`), `test`, `about`, `outputs`.

### Prior RPM research workspace
- Path exists: `../software_query`.
- Charter files available:
  - `RPM_charter.md`
  - `Python_RPM_charter.md`
  - `R_RPM_charter.md`
- Product docs available:
  - `docs/SRS.md`
  - `docs/ARD.md`
- Existing RPM artifacts and workflow evidence in `rpm/`.
  - SPEC count discovered: `1,282` (`*.spec`).
  - Refresh summary available with processed/updated/moved stats.

## Documentation Action Taken
- Copied authoritative RPM charter into this repository:
  - `docs/RPM_charter.md` (copied from `../software_query/RPM_charter.md`).
- Copied language-specific charters:
  - `docs/Python_RPM_charter.md`
  - `docs/R_RPM_charter.md`

## Reuse Candidates from `software_query`
- Containerized build script with image/container dual mode:
  - `../software_query/rpm/python/scripts/build_in_container.sh`
- Existing container-oriented workflow wrapper scripts:
  - `../software_query/rpm/top25/scripts/build_top25.sh`
  - `../software_query/rpm/next75/scripts/build_next75.sh`
  - `../software_query/rpm/remaining/scripts/build_remaining.sh`
- Prior docs explicitly target AlmaLinux containerized build context:
  - `../software_query/docs/SRS.md`
  - `../software_query/docs/ARD.md`

## Immediate Next Step
- Run a structured 15-question clarification sequence (one question at a time, multiple-choice) to define:
  - CLI contract and UX
  - Dependency closure semantics
  - RPM naming/layout policy
  - parser/templating behavior for Bioconda recipes
  - output/repository strategy
  - build/test/documentation workflow
