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

## Related Rules

- Multi-output recipes (`outputs:`) expand into discrete package outputs.
- Versioned recipe directories use highest version selection.
- Compliance failures (license SPDX/policy) also route to quarantine.
