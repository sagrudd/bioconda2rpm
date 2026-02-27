# Dependency Policy Specification

## Objective

Define dependency closure behavior for `bioconda2rpm build`.

## Default Policy

`build-host-run`

- Include dependencies from:
  - `requirements.build`
  - `requirements.host`
  - `requirements.run`
- Apply closure transitively according to policy implementation.

## Supported Policies

1. `run-only`
- Focus closure on runtime dependencies.
- Suitable for reduced build surfaces where build prerequisites are managed externally.

2. `build-host-run` (default)
- Enterprise default.
- Captures full packaging dependency context from recipe requirements.

3. `runtime-transitive-root-build-host`
- Root package includes build+host+run.
- Transitive dependencies focus on runtime closure.

## Dependency Expansion Toggle

- Default: enabled.
- `--no-deps`: disables dependency closure and processes requested package only.

## Missing Dependency Policies

1. `fail`
- Abort run on first unresolved dependency.

2. `skip`
- Continue while skipping unresolved dependency nodes.

3. `quarantine` (default)
- Move unresolved targets to quarantine set and continue resolvable subset.
- Default quarantine location is `<topdir>/BAD_SPEC` (topdir defaults to `~/bioconda2rpm`).

## Related Rules

- Multi-output recipes (`outputs:`) expand into discrete package outputs.
- Versioned recipe directories use highest version selection.
- Compliance failures (license SPDX/policy) also route to quarantine.

## BuildRequires Sourcing (Container Rebuild Chain)

During `SPEC -> SRPM -> RPM` rebuild, `BuildRequires` are resolved with tolerant ordering:

1. Already installed in the container image.
2. Matching local artifacts from `<topdir>/RPMS`.
3. Enabled distro/core repositories.

Dependency resolution is captured per package in `reports/dependency_graphs/` with source attribution and unresolved reasons.
