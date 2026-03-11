# Failure Priority Queue

## Scope

This queue is derived from the full `aarch64` verification report dated March 9, 2026:

- [build_batch_183_20260309193117.md](/Users/stephen/bioconda2rpm-eval-20260309-aarch64-eligible-full-nonfatal/targets/phoreus-bioconda2rpm-build-almalinux-9.7-aarch64/reports/build_batch_183_20260309193117.md)

The queue is strict:

1. Fix classes are ordered by unblock leverage.
2. Within a class, root failures come before blocked dependents.
3. New heuristics are not accepted without matching tests and documentation.

## Current KPI Context

- Quarantined packages: `107`
- Direct root failures: `38`
- Blocked by failed dependencies: `69`

## Queue

### P0. R / Bioconductor quoted-shell root failures

Status:
- Closed on March 11, 2026
- Rerun result: `10/10` generated, `0` quarantined
- Regression tests and documentation in place

Root packages:
- `bioconductor-ctc`
- `bioconductor-biocfilecache`
- `bioconductor-biocgenerics`
- `bioconductor-dnacopy`
- `bioconductor-assorthead`
- `bioconductor-biocparallel`
- `bioconductor-limma`
- `bioconductor-qvalue`
- `bioconductor-zlibbioc`
- `bioconductor-matrixgenerics`

Primary blocked dependents:
- `bioconductor-biobase`
- `bioconductor-s4vectors`
- `bioconductor-iranges`
- `bioconductor-genomeinfodb`
- `bioconductor-rsamtools`
- `bioconductor-genomicranges`
- `bioconductor-deseq2`
- `bioconductor-dexseq`
- `bioconductor-rtracklayer`
- `bioconductor-scater`
- `bioconductor-bambu`
- `trinity`

Exit criteria:
- Rerun the ten root packages
- Confirm no `%build`/`%install` shell quoting failures remain in this class
- Promote newly passing downstream BioC packages before addressing lower-priority classes

### P1. BuildRoot symlink contamination

Status:
- In progress
- March 11 rerun result: `1/9` generated, `8/9` quarantined
- `barrnap` cleared; remaining failures narrowed to minimal-mode path and wrapper/symlink normalization

Root packages:
- `bpipe`
- `fastqc`
- `gatk`
- `phylip`
- `pilon`
- `picard`
- `barrnap`
- `bracken`
- `mothur`

Observed pattern:
- absolute symlink into `BUILDROOT`
- wildcard symlink payloads such as `bin/*`
- dangling symlink chmod operations
- unset minimal-mode `PKG_*` / `RECIPE_DIR` variables causing malformed install roots such as `opt/-` and `share/--`

Blocked dependents:
- `trim-galore`
- `poa`
- `t-coffee`
- `perl-bio-tools-run-alignment-tcoffee`
- `perl-bioperl`
- `prokka`
- `jaffa`
- `dragonflye`

Exit criteria:
- Normalize or replace generated symlinks so installed payloads do not point into `BUILDROOT`
- Export the Conda-era variable surface required by translated minimal-mode install commands
- Add regression coverage for wildcard and dangling-symlink cases

### P2. BuildRoot path text contamination

Status:
- In progress
- First focused rerun was interrupted by Docker Desktop VM filesystem corruption, so package-level closure is still pending
- Minimal canonical renderer parity patch is now required because the text scrub pass was present in full payload specs but missing from minimal canonical specs

Root packages:
- `kmer-jellyfish`
- `nextflow`
- `viennarna`
- `rnabloom`

Observed pattern:
- installed files embed `BUILDROOT` text
- wrappers and metadata still reference temporary installation prefixes

Blocked dependents:
- `krakenuniq`
- `seqscreen`
- `t-coffee`
- `trinity`

Exit criteria:
- rewrite generated text payloads and metadata deterministically
- add regression tests for embedded buildroot strings in wrapper scripts and `.pc`/`.la` style files

### P3. Patch / prep normalization failures

Root packages:
- `necat`
- `tbl2asn-forever`
- `ucsc-fatotwobit`
- `ucsc-twobitinfo`
- `vcfdist`

Blocked dependents:
- `augustus`
- `busco`

Exit criteria:
- patch application and source extraction behave deterministically in `%prep`
- each new patch rule is covered by a focused test

### P4. Minimal interpreter command-shape failures

Root packages:
- `blast-legacy`
- `minced`
- `rtg-tools`
- `vcf-validator`

Observed pattern:
- command extraction preserves syntactically invalid or semantically partial shell
- directory assumptions or fallback `|| true` placement still need tightening

Exit criteria:
- regression tests model the exact failing command shapes
- minimal canonical `%build`/`%install` blocks are shell-valid and package-valid

### P5. Remaining direct one-off failures

Root packages:
- `staden_io_lib`
- `snpsift`
- `strelka`
- `snpeff`
- `trimmomatic`
- `flair`

Exit criteria:
- resolve individually after shared classes above are closed

## Regression Policy

Every queue item requires:

1. At least one focused unit or regression test per failure class
2. Decision-log documentation when the normalization strategy changes
3. A rerun against the affected root packages before the class is considered closed

## Commit Policy

Execution should proceed in small commits:

1. tests and docs for the class
2. implementation for the class
3. rerun or verification adjustments for the class

Each commit should be pushed after local verification completes.
