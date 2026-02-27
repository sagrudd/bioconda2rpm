# bioconda2rpm

Rust CLI to convert Bioconda recipes into Phoreus-style RPM artifacts.

## Baseline CLI

```bash
cargo run -- build <package> --recipe-root <path/to/bioconda-recipes/recipes>
```

## Priority SPEC Generation (Parallel)

Generate Phoreus payload/meta SPEC pairs for top-priority tools directly from Bioconda metadata:

```bash
cargo run -- generate-priority-specs \
  --recipe-root ../bioconda-recipes/recipes \
  --tools-csv ../software_query/tools.csv \
  --top-n 10 \
  --workers 6
```

Outputs are written under `--topdir` (default `~/bioconda2rpm`):
- `SPECS/` generated payload + meta SPEC files
- `SOURCES/` staged `build.sh` sources from Bioconda recipes
- `reports/priority_spec_generation.{json,csv,md}`
- `BAD_SPEC/` quarantine notes for unresolved/invalid entries

Defaults when omitted:
- `--topdir` -> `~/bioconda2rpm`
- `BAD_SPEC` quarantine -> `~/bioconda2rpm/BAD_SPEC`
- reports -> `~/bioconda2rpm/reports`

`--topdir` and `--bad-spec-dir` can be provided to override these paths.

## Documentation

- `docs/RPM_charter.md`
- `docs/Python_RPM_charter.md`
- `docs/R_RPM_charter.md`
- `docs/SRS.md`
- `docs/ARD.md`
- `docs/CLI_contract.md`
- `docs/dependency_policy.md`
- `docs/decision_log.md`
