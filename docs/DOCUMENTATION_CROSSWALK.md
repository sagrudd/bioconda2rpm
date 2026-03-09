# Documentation Crosswalk (Public + Mnemosyne Mirror)

## Purpose

`bioconda2rpm` is the public repository for this tool. Public-facing requirements, architecture, and usage guidance are maintained here and mirrored into `../mnemosyne-docs` under the Phoreus framework.

## Canonical Public Docs (This Repo)

- SRS: `docs/SRS.md`
- ARD: `docs/ARD.md`
- User guide: `docs/USERGUIDE.md`

## Mnemosyne Mirror Targets

- SRS mirror: `../mnemosyne-docs/requirements/srs/products/bioconda2rpm/BIOCONDA2RPM_SRS.md`
- ARD mirror: `../mnemosyne-docs/architecture/ards/products/bioconda2rpm/ARD-001-bioconda2rpm-architecture-review.md`
- User-facing guide mirror: `../mnemosyne-docs/guides/user/bioconda2rpm/bioconda2rpm-user-guide.md`

## Sync Policy

1. Update public docs in this repository first.
2. Mirror substantive SRS/ARD/user-guide changes into `../mnemosyne-docs` in the same change window.
3. Keep command examples and option names synchronized with `bioconda2rpm --help` output.
