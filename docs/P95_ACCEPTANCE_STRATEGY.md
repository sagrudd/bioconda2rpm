# P95 Acceptance Strategy

Date: March 11, 2026
Baseline campaign: `/Users/stephen/bioconda2rpm-eval-20260309-aarch64-eligible-full-nonfatal/targets/phoreus-bioconda2rpm-build-almalinux-9.7-aarch64/reports/build_batch_183_20260309193117.md`

## Objective

Raise `aarch64` arch-adjusted package generation from the current documented `73.58%` (`298 / 405`) to at least `95%` acceptance (`385 / 405`).

Required net gain:

- Current successes: `298`
- Required successes for `95%`: `385`
- Additional successes required: `87`

## Constraints

1. Work highest-leverage shared failure classes before one-off package fixes.
2. Every normalization change must ship with focused tests and documentation.
3. A class is only considered closed after rerunning the affected roots and then rerunning the full `aarch64` eligible campaign.

## Leverage Model

The current failure queue already groups the March 9 failures by shared class. Using the queue ordering and dependency graph from the baseline report, the quarantine set decomposes into the following maximum recoverable buckets.

These are planning numbers, not guaranteed wins. A blocked package counted under a class may expose a secondary independent failure once its upstream blocker is removed.

| Priority | Failure class | Direct roots | Reachable quarantined packages | Cumulative best-case success rate if class closes |
|---|---:|---:|---:|---:|
| P0 | R / Bioconductor quoted-shell root failures | 10 | 65 | 89.63% |
| P1 | BuildRoot symlink contamination | 9 | 15 | 93.33% |
| P2 | BuildRoot path text contamination | 4 | 6 | 94.81% |
| P3 | Patch / prep normalization failures | 5 | 7 | 96.54% |
| P4 | Minimal interpreter command-shape failures | 4 | 5 | 97.78% |
| P5 | Remaining direct one-off failures | 6 | 9 | 100.00% |

Implication:

- P0 plus P1 plus P2 is still short of the acceptance target in best-case planning terms.
- The minimum viable path to `>=95%` is: close `P0`, `P1`, `P2`, and at least one more shared class, with `P3` the next highest-leverage class.

## Strategy

### Wave 0: Lock in the already-cleared P0 gains

Goal:

- Turn the March 11 P0 rerun result into campaign-level accepted gain instead of treating it as anecdotal progress.

Why first:

- `P0` is the single largest leverage class.
- The queue already marks it closed with `10/10` rerun success.
- Until the full campaign is rerun, the official KPI remains the March 9 `73.58%`.

Actions:

1. Verify all P0 tests and documentation are merged and stable.
2. Rerun the full `aarch64` eligible campaign immediately after the current branch is green.
3. Diff the new campaign report against March 9 to identify which blocked BioC packages promoted automatically and which now expose second-order failures.

Expected result:

- Best-case recovery is `65` packages, taking the campaign from `298` to `363` successes (`89.63%`).
- Even if realized gain is lower, the rerun will collapse uncertainty for the rest of the plan.

### Wave 1: Finish BuildRoot install-surface normalization

Goal:

- Close the two install-surface classes that still dominate non-BioC failures:
  - `P1` BuildRoot symlink contamination
  - `P2` BuildRoot path text contamination

Why second:

- Together they are the next-largest reusable classes after `P0`.
- They share a common implementation zone: post-install normalization, wrapper rewriting, and minimal/full renderer parity.
- They hit widely used tools and unblock multiple downstream packages.

Actions:

1. Unify post-install normalization across minimal canonical and full payload renderers.
2. Make symlink rewriting deterministic:
   - replace absolute BuildRoot symlinks with relative symlinks
   - eliminate wildcard link installation
   - avoid chmod on dangling symlinks
3. Make text scrubbing deterministic:
   - scrub BuildRoot prefixes from wrapper scripts
   - scrub `.pc`, `.la`, launcher scripts, and generated metadata
   - export the Conda-era variables needed by translated minimal-mode install logic
4. Add class-focused regression fixtures for:
   - `barrnap`/`bracken`/`mothur`-style symlink outputs
   - `nextflow`/`viennarna`/`rnabloom` BuildRoot text contamination
   - minimal-mode malformed path cases (`opt/-`, `share/--`)

Execution order inside Wave 1:

1. Finish the remaining `P1` roots first.
2. Land minimal/full renderer parity patch.
3. Close `P2` roots once parity is in place.
4. Rerun the union of `P1` and `P2` roots before any full campaign rerun.

Expected result:

- Best-case `P1` adds `15` successes and lifts the campaign to `93.33%` after `P0`.
- Best-case `P2` adds `6` more and lifts the campaign to `94.81%`.
- This wave is necessary but not sufficient for the `95%` gate.

### Wave 2: Close the next shared prep-time class

Goal:

- Close `P3` Patch / prep normalization failures.

Why third:

- `P3` is the smallest remaining shared class that is still sufficient to push the campaign above `95%` once `P0` through `P2` are closed.
- It is structurally cleaner than dropping into one-off fixes.
- It has downstream leverage through `augustus` and `busco`.

Actions:

1. Normalize patch strip detection and source extraction in `%prep`.
2. Add focused coverage for:
   - missing patch path roots
   - tar root / strip-component inference
   - truncated archive detection
3. Use the five roots as the only acceptance set for the class:
   - `necat`
   - `tbl2asn-forever`
   - `ucsc-fatotwobit`
   - `ucsc-twobitinfo`
   - `vcfdist`

Expected result:

- Best-case `P3` adds `7` more successes and lifts the campaign to `96.54%`.
- This is the first wave sequence that clears the `95%` gate without relying on fragile one-off fixes.

### Wave 3: Only then spend effort on lower-yield cleanup

Goal:

- Use `P4` and `P5` as reserve classes once the acceptance gate is already passed or when Wave 2 exposes unexpected residual failures.

Why deferred:

- `P4` and `P5` have lower leverage.
- `P5` is explicitly one-off work and should not displace a reusable normalization class.
- If `P0` or `P1` underperform best-case estimates after rerun, these classes become contingency capacity rather than the default path.

Contingency rules:

1. If `P0` full-rerun realized gain is materially below `50` packages, start `P3` in parallel with `P1`.
2. If `P1` closes but `P2` stalls on renderer parity, take the easiest `P4` root in parallel only if it does not slow the parity work.
3. Use `P5` only to patch the final few packages needed for `95%` after all reusable classes above have been exploited.

## Resourcing Plan

Because effort is increasing, run the work in three lanes with explicit ownership boundaries.

### Lane A: Renderer and install-surface normalization

Scope:

- `P1`
- `P2`
- shared minimal/full renderer parity

Deliverables:

- deterministic post-install scrub pass
- deterministic relative-symlink normalization
- regression fixtures for BuildRoot contamination

### Lane B: Prep-time normalization

Scope:

- `P3`

Deliverables:

- patch strip/path inference hardening
- source extraction hardening
- focused `%prep` regression tests

### Lane C: Campaign verification and unblock accounting

Scope:

- full reruns after each wave
- dependency-unblock accounting
- queue updates and residual-class triage

Deliverables:

- fresh `build_batch_*.md` campaign reports
- updated failure queue with promoted packages removed
- observed-versus-planned leverage table after each wave

## Acceptance Gates

Do not count progress informally. Use these gates.

### Class gate

For each class:

1. focused regression test coverage is added
2. root packages rerun cleanly
3. queue/documentation is updated

### Wave gate

After each wave:

1. rerun full `aarch64` eligible campaign
2. record new KPI percentage
3. re-baseline remaining quarantine classes from the new report

### Program gate

The strategy is complete only when:

1. campaign KPI is `>=95%`
2. the remaining failures are documented as residual classes or one-offs
3. the path to `>=99%` is restated from the new baseline

## Recommended Immediate Sequence

1. Run a full `aarch64` campaign now to cash out the already-closed `P0` work.
2. Assign the main engineering lane to `P1` and `P2` together because they share the same install-surface machinery.
3. Start `P3` in parallel with a second lane; do not wait for all of Wave 1 to be fully merged if staffing allows.
4. Rerun the full campaign after `P1`/`P2`, then again after `P3`.
5. Use `P4` or `P5` only as contingency to close any remaining shortfall to `95%`.

## Expected Path To 95%

The most defensible route is:

1. `P0` campaign promotion
2. `P1` BuildRoot symlink contamination
3. `P2` BuildRoot path text contamination
4. `P3` Patch / prep normalization

This path targets the largest reusable classes first, minimizes dependence on one-off fixes, and provides enough best-case lift to move from `73.58%` to `96.54%`.
