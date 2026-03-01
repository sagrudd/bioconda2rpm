# Customer Package Intake SLA Model

Version: LPU-v1.0-SLA  
Date: March 1, 2026  
Status: Active (post-launch controlled expansion)

## 1. Purpose

The Customer Package Intake SLA defines a deterministic and auditable pathway for adding customer-requested packages after LPU-v1.0 launch.

This SLA MUST:
- provide predictable onboarding timelines,
- preserve reproducibility and compliance controls,
- prevent heuristic drift,
- bound founder operational load,
- preserve Launch Packaging Constitution integrity.

## 2. Intake Classification

Every intake request MUST be classified before build work starts.

### Class A — Low Complexity
- Primarily C/C++ tool with standard build system (Autotools/CMake/Make).
- Low dependency depth.
- No strict micro-version pin concentration.

### Class B — Moderate Complexity
- Python extension and/or R/Bioconductor package.
- Moderate dependency tree.
- Minor version pin conflicts expected.

### Class C — High Complexity
- Deep dependency chains.
- Strict ABI coupling and/or module cross-conflicts.
- Vendored library pressure and high governance risk.

### Deterministic Classification Heuristics

Initial class assignment SHOULD be computed from recipe metadata:
- language stack markers:
  - Python: `python` in host/run/build requirements or Python build backend markers
  - R/Bioconductor: `r-*` or `bioconductor-*` dependencies
- dependency count:
  - low: total unique build/host/run deps <= 15
  - moderate: 16 to 40
  - high: > 40
- pin density:
  - strict pins ratio = exact-or-micro-pinned deps / total deps
  - high pin density if strict pins ratio >= 0.35

Classification rule:
- assign Class C if dependency count > 40, or strict pins ratio >= 0.35, or vendoring signal present.
- else assign Class B if Python/R/Bioconductor stack present or dependency count in 16 to 40.
- else assign Class A.

## 3. SLA Targets

SLA clock MUST start only when complete intake metadata is received:
- package identifier (normalized recipe name),
- requested version or policy (`latest`),
- target platform (`almalinux-9.x`),
- deployment tier (regulated/non-regulated),
- requester and business priority.

Turnaround targets:
- Class A: target 2 business days.
- Class B: target 3 to 5 business days.
- Class C: escalation required; case-by-case estimate. Customer MUST receive initial decision/update within 2 business days.

## 4. Standard Intake Workflow

All intake executions MUST follow this sequence:
1. Request logged with intake ID.
2. Package metadata retrieved from managed recipe source.
3. Complexity classification computed and recorded.
4. Taxonomy analysis run (existing failure classes + stage labeling).
5. Normalization rules applied (deterministic policy only).
6. Deterministic mock/container build executed.
7. Reproducibility validation executed.
8. Module stream assignment finalized.
9. Documentation bundle recorded.
10. Package registry updated to LPU-v1.x.

Stage isolation MUST be enforced; infrastructure-stage failures MUST be recorded separately from build normalization failures.

## 5. Acceptance Criteria

A package MUST be marked accepted only if all criteria pass:
- deterministic SPEC generated from governed inputs,
- explicit `BuildRequires` and `Requires` present,
- reproducible build succeeds on target profile,
- no dominant unclassified failure category remains,
- provenance metadata recorded,
- module stream assignment declared and recorded.

## 6. Exception and Escalation

Escalation MUST trigger on any of:
- vendored dependency required for successful build,
- unresolved strict pin conflict,
- toolchain incompatibility with governed platform,
- module proliferation or cross-stream conflict risk,
- SLA breach risk.

Escalation pathway:
1. Risk assessment documented.
2. Governance note recorded with recommendation.
3. Customer notified with status and constraints.
4. Timeline revised and approved.

## 7. Capacity Protection Rules

To protect delivery stability:
- maximum concurrent active intakes MUST be 5.
- weekly intake acceptance cap SHOULD be 12 packages.
- founder override MAY pause new intake acceptance for one cycle when governance risk or delivery risk is elevated.
- prioritization MUST rank regulated production requests ahead of academic/research-only requests.

## 8. Audit Record Requirements

Each intake MUST produce an auditable intake record.

```rust
struct IntakeRecord {
    package_name: String,
    intake_date: DateTime<Utc>,
    complexity_class: String,
    sla_target_days: u8,
    build_success_date: Option<DateTime<Utc>>,
    normalization_rules_applied: Vec<String>,
    provenance_recorded: bool,
    module_stream: String,
    escalation_flag: bool,
}
```

The following artifacts MUST be archived and cross-referenced:
- generated SPEC,
- build logs,
- dependency graph,
- version/pin justification,
- acceptance or escalation decision record.

## 9. Versioning Impact

LPU versioning model:
- `LPU-v1.0`: launch baseline.
- `LPU-v1.1`, `LPU-v1.2`, ...: incremental intake additions.
- major version increments only when structural packaging model changes.

Each accepted intake MUST map to an LPU-v1.x change record.

## 10. Performance Metrics

The intake program MUST track:
- average intake turnaround time (business days),
- SLA compliance percentage,
- percentage of Class C requests,
- rule reuse ratio,
- intake-induced NMI delta.

Operational review SHOULD occur weekly and MUST occur monthly.

## 11. Boundary Conditions

This SLA does not:
- guarantee universal Bioconda coverage,
- guarantee acceptance of every request,
- bypass compliance feasibility constraints.

Requests MAY be deferred or rejected when governance risk is unacceptable or deterministic packaging criteria cannot be met in scope.
