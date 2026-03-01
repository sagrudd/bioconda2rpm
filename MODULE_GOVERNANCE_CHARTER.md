# Phoreus Module Governance Charter

Version: 1.0  
Date: March 1, 2026  
Status: Active Baseline  
Scope: Regulated bioinformatics RPM ecosystem on AlmaLinux/RHEL using DNF modulemd v2

## 1. Module Philosophy

### 1.1 Purpose
Module streams are the mandatory mechanism for controlled multi-version coexistence of bioinformatics software and language runtimes in Phoreus. Streams preserve reproducibility by separating version lines and binding dependency policy to a governed release unit.

### 1.2 System Packages vs Module Streams
Rules:
- Base OS and security-critical core packages remain system-managed (non-modular) unless explicitly elevated to a foundational module.
- Bioinformatics payloads, language runtimes, and ecosystem stacks are stream-managed.
- Application delivery shall not depend on mutable system package state after stream publication.

### 1.3 Stream Immutability
Rules:
- An `Active` stream is immutable for semantic version identity (`name:stream`).
- Stream metadata and RPM content for a published build context are append-only.
- Replacing already-published artifacts is forbidden.
- Any ABI-breaking change requires a new stream.

## 2. Stream Design Principles

### 2.1 Naming Convention
Rules:
- Stream naming format: `<software>:<major.minor>`.
- Stream values are numeric for versioned software lines (for example `htslib:1.19`, `python:3.11`).
- Non-version labels (`latest`, `stable`) are forbidden as stream IDs.

### 2.2 New Stream vs Existing Stream
Rules:
- Create a new stream when:
  - Upstream major changes.
  - Upstream minor introduces ABI/API incompatibility.
  - Runtime floor changes (for example Python 3.11 to 3.13).
  - Compiler baseline changes in a way that invalidates reproducibility claims.
- Reuse existing stream only for patch-level, ABI-compatible updates.

### 2.3 Patch-Level Update Rules
Rules:
- Patch updates within a stream are permitted only for:
  - Security fixes.
  - Critical bug fixes.
  - Deterministic rebuild corrections without behavior drift.
- Patch updates shall preserve dependency major/minor contracts.

### 2.4 ABI-Breaking Releases
Rules:
- ABI break triggers mandatory stream split.
- Upstream soname changes require new stream.
- Reverse dependencies remain pinned to previous compatible stream until explicitly migrated.

### 2.5 Version Pinning Policy
Rules:
- Stream-level runtime dependencies are pinned to major.minor.
- Build artifact metadata records exact NEVRA for all resolved build-time dependencies.
- Exact pin preservation is mandatory for regulated replay workloads.

## 3. Dependency Strategy

### 3.1 Stream Dependency Rules
Rules:
- Module dependencies must be explicit in modulemd.
- Dependencies are allowed only on approved foundational streams and governed peer streams.
- Implicit dependency on global system paths is forbidden.

### 3.2 Cross-Stream Dependencies
Rules:
- Cross-stream dependencies are permitted only when they target foundational streams.
- Cross-stream dependencies between two application streams are forbidden by default.
- Exception requires governance approval and expiration plan.

### 3.3 Language Stack Policy
Rules:
- Python packages must depend on an approved Phoreus Python stream (`python:3.11` or `python:3.13` as approved).
- R/Bioconductor packages must depend on approved Phoreus R stream (`r:4.5` baseline).
- Perl ecosystem packages must depend on approved Phoreus Perl stream.
- Mixed-language builds must declare each runtime explicitly; transitive runtime discovery is not acceptable.

### 3.4 Shared Low-Level Libraries
Rules:
- Common low-level libraries (for example `htslib`, `boost`, `zlib`, `libdeflate`) shall be sourced from:
  - Core distro repos when policy allows, or
  - Foundational Phoreus streams when version governance is required.
- Vendored copies of shared low-level libraries are forbidden by default.

### 3.5 Strict Bioconda Pins
Rules:
- Upstream strict pins are preserved unless they conflict with platform governance.
- If conflict exists, dependency normalization must emit an explicit policy decision record.
- Silent pin relaxation is forbidden.

## 4. Build Governance

### 4.1 BuildRequires in Modular Context
Rules:
- Generated SPEC `BuildRequires` must be derived deterministically from recipe metadata plus normalized policy.
- BuildRequires must resolve through approved repos/streams only.
- Missing BuildRequires shall fail build with classified dependency error.

### 4.2 Deterministic Build Requirements
Rules:
- Builds execute only in controlled container images or mock-equivalent controlled roots.
- Build root must be isolated from host package state.
- Build environments must be versioned and auditable.

### 4.3 Compiler Stack Consistency
Rules:
- Compiler stack is pinned by target build profile.
- Profile change requires architecture board approval and compatibility replay.
- Mixing compiler stacks inside one stream is forbidden.

### 4.4 Hardening and Security
Rules:
- Mandatory hardening flags are enforced by profile policy.
- Exceptions must be explicitly documented in package-level compliance record.

### 4.5 Prefix and RPATH
Rules:
- Payloads install under `/usr/local/phoreus/<tool>/<version>`.
- Conda-prefix leakage is forbidden.
- Runtime paths must not reference temporary build roots.
- Non-approved RPATH values fail validation.

## 5. Version Coexistence Policy

### 5.1 Concurrent Installability
Rules:
- Multiple versions coexist by isolated prefixes and modulefiles.
- Package payloads must not claim ownership of shared non-versioned runtime paths.

### 5.2 Activation Model
Rules:
- Runtime activation is module-driven.
- Modulefiles must set PATH/LD_LIBRARY_PATH/PKG_CONFIG_PATH to stream-local prefix only.
- Default module selection is explicit and revision-controlled.

### 5.3 Conflict Avoidance
Rules:
- File conflicts across streams are forbidden.
- Any shared path content must be provided by dedicated infrastructure packages only.

### 5.4 Isolation Guarantee
Rules:
- One stream’s activation must not mutate another stream’s state.
- Cross-stream state write operations are forbidden.

## 6. Lifecycle Management

### 6.1 Stream States
Defined states:
- `Draft`: design and dry-run only.
- `Active`: production supported.
- `Maintenance`: only security/critical fixes.
- `Frozen`: no new builds; retained for replay.
- `Deprecated`: replacement announced; removal countdown active.
- `Retired`: unpublished from active repos, retained in archive.

### 6.2 Security Expectations
Rules:
- `Active` and `Maintenance` streams receive security updates within SLA window defined by operations policy.
- Known critical CVEs without mitigation require temporary stream hold or quarantine.

### 6.3 End-of-Life Process
Rules:
- EOL requires deprecation notice, migration path, and final archived snapshot.
- EOL date must be published in stream metadata.

### 6.4 Regulatory Archival
Rules:
- Retired streams remain retrievable with immutable metadata, source provenance, and build logs.
- Archive retention follows regulated records policy.

## 7. Compliance and Traceability

### 7.1 Required Stream Metadata
Each stream release record must include:
- Stream identifier and context.
- Build container profile and digest.
- Source URLs and checksums.
- Applied patches with rationale.
- Dependency graph snapshot.
- SPDX license data.
- Build timestamp and builder identity.

### 7.2 Source Provenance
Rules:
- All sources must be cryptographically identified (hash/checksum).
- Unverifiable source inputs are non-compliant.

### 7.3 Patch Documentation
Rules:
- Every patch must have origin, intent, and impact classification.
- Untracked patch injection is forbidden.

### 7.4 SBOM
Rules:
- Each release must produce SBOM artifacts (SPDX or CycloneDX).
- SBOM must include direct and transitive runtime components.

### 7.5 Audit Logging
Rules:
- Build, dependency normalization, and publication events are audit logged.
- Failure classifications and overrides are retained with actor and timestamp.

## 8. CI/CD and Validation

### 8.1 Pre-Publish Gates
Required gates:
- SPEC lint/policy validation.
- Dependency graph validation.
- Build success in controlled environment.
- Installability check.
- Module metadata validation.

### 8.2 ABI Validation
Rules:
- ABI-sensitive libraries require ABI diff checks on stream updates.
- ABI break in same stream is forbidden.

### 8.3 Dependency Validation
Rules:
- Dependency graphs must be acyclic at stream-level governance boundaries.
- New dependency edges require policy check against allowed matrix.

### 8.4 Reproducibility Verification
Rules:
- Periodic rebuild replay is required for active high-priority streams.
- Reproducibility drift must trigger governance review.

## 9. Exception Handling

### 9.1 Vendoring Approval
Rules:
- Vendoring is prohibited unless approved via formal exception.
- Exception record must include:
  - justification,
  - scope,
  - expiry date,
  - de-vendoring plan.

### 9.2 Temporary Pin Overrides
Rules:
- Temporary pin overrides require change record and expiration.
- Permanent silent overrides are forbidden.

### 9.3 Emergency Rebuild Protocol
Rules:
- Emergency rebuilds allowed for security incidents or supply-chain compromise.
- Emergency builds must still emit full traceability records.
- Post-incident review is mandatory.

## 10. Risk Model

### 10.1 Primary Risks
- Stream proliferation causing dependency combinatorics.
- Cross-stream coupling growth.
- Override accumulation replacing policy discipline.
- Drift between build policy and published module metadata.

### 10.2 Controls
Rules:
- Stream count caps per software family are enforced by governance board.
- New stream creation requires impact analysis.
- Cross-stream dependencies are blocked unless explicitly approved.
- Override registry size is monitored with hard thresholds.

### 10.3 Governance Limits
Rules:
- Parallel supported versions per tool default to 2; third concurrent stream requires explicit approval.
- Foundational runtime streams (Python, R, Perl) follow separate controlled cadence.

## Appendix A: Example modulemd v2 Fragments

```yaml
document: modulemd
version: 2
data:
  name: phoreus-python
  stream: "3.11"
  version: 2026030101
  context: "a1b2c3d4"
  arch: x86_64
  summary: "Phoreus Python runtime 3.11"
  description: >
    Governed Python runtime stream for bioinformatics payload modules.
  license:
    module:
      - MIT
  dependencies:
    - buildrequires:
        platform: [el9]
      requires:
        platform: [el9]
  profiles:
    default:
      rpms:
        - phoreus-python-3.11
```

```yaml
document: modulemd
version: 2
data:
  name: phoreus-htslib
  stream: "1.19"
  version: 2026030101
  context: "9f8e7d6c"
  arch: aarch64
  summary: "Phoreus htslib stream"
  description: >
    Foundational htslib stream consumed by governed bioinformatics payload streams.
  dependencies:
    - buildrequires:
        platform: [el9]
        phoreus-python: ["3.11"]
      requires:
        platform: [el9]
  profiles:
    default:
      rpms:
        - phoreus-htslib-1.19
```

## Appendix B: Governance Defaults

- Default target policy base: AlmaLinux 9.x build profiles.
- Default runtime stream preference: Python 3.11 unless package governance requires 3.13.
- Default stream publication state: `Draft` until all validation gates pass.
