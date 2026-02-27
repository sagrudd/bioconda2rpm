# bioconda2rpm

Rust CLI to convert Bioconda recipes into Phoreus-style RPM artifacts.

## Baseline CLI

```bash
cargo run -- build <package> --recipe-root <path/to/bioconda-recipes/recipes> --topdir <external/rpmbuild/path>
```

## Documentation

- `docs/RPM_charter.md`
- `docs/Python_RPM_charter.md`
- `docs/R_RPM_charter.md`
- `docs/SRS.md`
- `docs/ARD.md`
- `docs/CLI_contract.md`
- `docs/dependency_policy.md`
- `docs/decision_log.md`
