# Launch Packaging Constitution

Version: LPU-v1.0  
Date: March 1, 2026  
Status: Active for launch execution window (<3 months)

## 1. Purpose

This constitution defines the launch-bounded packaging governance model for Mnemosyne Biosciences.

This constitution MUST:
- establish deterministic, reproducible, and auditable package production for regulated deployment,
- bound implementation to Launch Packaging Universe v1.0 (LPU-v1),
- prevent heuristic drift and undocumented package-specific behavior,
- enable controlled post-launch expansion,
- minimize founder operational and decision overhead.

This constitution governs launch scope only. It MUST NOT be interpreted as a full-ecosystem parity policy.

## 2. Launch Packaging Universe (LPU)

LPU-v1 is the approved launch package universe.

LPU-v1 rules:
- LPU-v1 package inventory MUST be maintained in an externally referenced registry file (`docs/verification_software.txt`) with normalized Bioconda recipe names.
- LPU-v1 launch size MUST NOT exceed 200 packages.
- LPU-v1 is frozen for launch execution; additions MUST follow formal intake approval.
- Any package not in LPU-v1 MUST be treated as out of regulated production support scope.

## 3. Packaging Invariants

The following are non-negotiable controls:

1. Deterministic SPEC generation  
`bioconda2rpm` MUST generate SPEC content deterministically from recipe metadata (`meta.yaml`), `build.sh`, and declared patches.

2. Explicit dependency declaration  
Generated SPEC files MUST contain explicit `BuildRequires` and `Requires`. Implicit dependency behavior MUST NOT be relied upon.

3. Vendoring prohibition by default  
Vendored libraries MUST NOT be shipped unless approved through the exception process.

4. Module stream isolation  
Runtime and toolchain stacks MUST be isolated by controlled module/stream policy and Phoreus runtime boundaries.

5. Reproducible clean builds  
Builds MUST execute in clean, controlled build roots (mock/containerized equivalent) with reproducibility evidence retained.

6. Stage-isolated failures  
Pipeline failures MUST be stage-labeled; infrastructure-stage failures MUST NOT be conflated with package normalization failures.

7. Provenance metadata  
Each build output MUST include package provenance metadata (source URL/ref, checksums, recipe revision, build target, timestamp).

8. Conda runtime exclusion  
Conda runtime artifacts MUST NOT appear in produced RPM payloads or runtime dependency model.

## 4. Package Tiers

Tier classification:
- Tier 1: Core Pipeline Dependencies
- Tier 2: Supporting Analytical Tools
- Tier 3: Optional/Research Extensions (non-regulated deployment)

Launch compliance rules:
- Tier 1 and Tier 2 packages are in compliance scope for launch.
- Tier 3 packages MUST NOT gate launch readiness.
- Tier 3 packages MAY be built for research environments under separate controls.

## 5. SPEC Hardening Requirements

Each launch-scope SPEC MUST include:
- explicit source URLs and integrity checks,
- transparent version/pin expression,
- stream/runtime targeting consistent with governance,
- explicit dependency declarations,
- RPATH/prefix normalization controls,
- hardening flag compliance with target platform policy.

Generated SPECs MUST NOT rely on opaque post-generation manual edits for successful build.

### Example SPEC Fragment (structure)

```spec
Name:           phoreus-example-tool
Version:        1.2.3
Release:        1%{?dist}
Summary:        Example launch-governed package
License:        BSD-3-Clause
URL:            https://example.org/tool
Source0:        https://example.org/releases/tool-1.2.3.tar.gz

BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  zlib-devel
Requires:       zlib

%description
Launch-governed package built under LPU-v1 controls.

%prep
%autosetup -p1

%build
%set_build_flags
make %{?_smp_mflags}

%install
make DESTDIR=%{buildroot} install

%check
test -x %{buildroot}/usr/local/phoreus/example-tool/1.2.3/bin/example-tool
```

## 6. Reproducibility Standard

A package is reproducible only if all checks below pass:
- clean build root execution (no artifact reuse except governed local RPM dependency ingestion),
- artifact hash recording for SRPM and RPM outputs,
- complete build log archival,
- dependency graph archival,
- module/runtime activation validation,
- environment isolation validation (no host leakage).

Minimum reproducibility checklist MUST be completed and archived per package build attempt.

## 7. Customer Package Intake Protocol (Post-Launch)

Post-launch intake workflow:
1. Customer request is logged.
2. Package is classified by tier and risk.
3. Taxonomy-driven normalization workflow is applied.
4. Deterministic build and validation are completed.
5. Package is added to LPU-v1.x registry with full documentation.
6. Constitution version is incremented (v1.x).

Service target:
- Standard intake SLA SHOULD target 2 to 5 working days per package.
- Operational intake governance MUST follow [Customer Package Intake SLA](./CUSTOMER_PACKAGE_INTAKE_SLA.md).

## 8. Exception Handling

Allowed exceptions are strictly limited to:
- approved vendored library usage with written rationale,
- temporary version pin relaxation with expiry,
- emergency rebuild under audit/security notice.

Exception controls:
- each exception MUST include written justification,
- each exception MUST have approver identity and date,
- each exception MUST be retained in auditable records,
- expired exceptions MUST be removed or re-approved.

## 9. Documentation Requirements

For every LPU package, the documentation set MUST contain:
- SPEC file reference,
- module/stream definition reference,
- provenance record,
- build log location,
- dependency graph artifact references,
- validation evidence reference.
- audit evidence and hardening checklist record per [Audit Template and Tier 1 Checklist](./AUDIT_EVIDENCE_TEMPLATE_AND_TIER1_CHECKLIST.md).

Minimal metadata schema:
- package_name
- bioconda_recipe
- version
- release
- target_os
- target_arch
- build_id
- source_url
- source_checksum
- dependency_graph_ref
- build_log_ref
- validation_ref
- status

## 10. Launch Readiness Criteria

Launch Packaging Constitution is satisfied only when all criteria are met:
- at least 95% of LPU-v1 packages build reproducibly,
- no unresolved systemic Stage 1 to Stage 3 infrastructure failures remain,
- failure taxonomy is stable for launch scope,
- no unclassified dominant failure category remains,
- governance checklist is complete and signed off.

## 11. Boundary Statement

This constitution does not:
- guarantee ecosystem-wide normalization,
- provide full Bioconda translation parity,
- model long-horizon (10-year) ecosystem drift,
- pre-normalize packages outside requested launch scope.

Scope is intentionally bounded to achieve launch velocity with regulatory discipline.

## 12. Versioning

Version model:
- `LPU-v1.0`: launch baseline
- `LPU-v1.x`: controlled post-launch expansion
- `LPU-v2.0`: post-launch structural revision

Version increments MUST be accompanied by documented delta notes and approval records.
