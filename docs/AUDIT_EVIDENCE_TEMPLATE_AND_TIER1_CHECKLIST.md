# Minimalist Packaging Audit Evidence Template and Tier 1 Hardening Checklist

Version: LPU-v1.0-AUDIT  
Date: March 1, 2026  
Status: Active

This document defines two founder-executable operational artifacts:
1. Minimalist Packaging Audit Evidence Template (per package record).
2. Tier 1 Package Hardening Checklist (pre-launch execution checklist).

## Part I: Minimalist Packaging Audit Evidence Template

Use one record per package version/release.

### Section A: Package Identity

- [Required] Package Name:
- [Required] Version:
- [Required] Upstream Source URL:
- [Required] Source Checksum (SHA256):
- [Required] SPEC File Version/Release:
- [Required] Module Stream:
- [Required] Packaging Constitution Version (for example, `LPU-v1.0`):

### Section B: Build Reproducibility Record

- [Required] Build Date (UTC):
- [Required] Builder Environment (mock/container profile):
- [Required] OS Version (AlmaLinux release):
- [Required] Toolchain Version:
- [Required] Successful Build Log Reference (path/link):
- [Required] RPM Artifact Checksum (SHA256):
- [Required] Module Activation Test Result (`pass`/`fail`):

### Section C: Dependency Declaration

- [Required] Explicit `BuildRequires` Snapshot:
- [Required] Explicit `Requires` Snapshot:
- [Required] Dependency Graph Snapshot (summary):
- [Required] Confirmation: no implicit dependency resolution (`yes`/`no`):
- [Required] Confirmation: no Conda runtime artifacts (`yes`/`no`):

### Section D: Normalization Record

- [Required] Normalization Rules Applied (or `none`):
- [Required] Failure Category Addressed (if applicable):
- [Required] Vendoring Status (`YES`/`NO`):
- [Required] RPATH Normalization Confirmation (`yes`/`no`):
- [Required] Hardening Flag Confirmation (`yes`/`no`):

### Section E: Validation Reference

- [Required] Validation Boundary Reference (Urania layer reference):
- [Required] Pipeline Usage Context (Tier 1 role):
- [Required] Confirmation packaging changes do not alter methodological logic (`yes`/`no`):

### Section F: Sign-off

- [Required] Packaging Validation Date:
- [Required] Responsible Engineer:
- [Required] Constitution Compliance Confirmation (`YES`/`NO`):
- [Optional] Notes:

### Suggested Record Footer

- Record ID:
- Linked Artifacts:
- Last Updated (UTC):

---

## Part II: Tier 1 Package Hardening Checklist

Expected completion time per package: 60 to 90 minutes (typical case).

### 1. Source Verification

- [ ] Upstream source URL verified
- [ ] SHA256 checksum recorded
- [ ] Version matches Tier 1 pipeline requirement
- [ ] No hidden patch drift detected

### 2. SPEC Structure

- [ ] Deterministic `Name/Version/Release`
- [ ] Explicit `BuildRequires`
- [ ] Explicit `Requires`
- [ ] No implicit dependency assumptions
- [ ] No embedded `$PREFIX` artifacts
- [ ] Correct `%{_prefix}` usage
- [ ] No bundled libraries (unless approved exception)

### 3. Build Validation

- [ ] Clean mock/container build successful
- [ ] No unresolved Stage 1 to Stage 3 infrastructure failures
- [ ] No unclassified build failures
- [ ] Build logs archived
- [ ] RPM artifact checksum recorded

### 4. Runtime Validation

- [ ] Module stream loads cleanly
- [ ] Binary executes with `--version` (or equivalent deterministic probe)
- [ ] Shared library dependencies verified (`ldd` or equivalent)
- [ ] No unexpected runtime library resolution

### 5. Hardening and Security

- [ ] RPATH normalized or stripped
- [ ] Hardened flags respected
- [ ] No debug artifacts in production package set
- [ ] No leftover build-time paths in payload

### 6. Documentation Finalization

- [ ] Audit Evidence Template completed
- [ ] Package added to `LPU-v1.x` registry
- [ ] Version recorded in packaging inventory
- [ ] Intake classification updated (post-launch intake only)

### 7. Stop Conditions (Escalate Before Deployment)

Stop immediately and escalate if any condition is true:
- [ ] Vendoring required
- [ ] Strict pin conflict unresolved
- [ ] Toolchain incompatibility unresolved
- [ ] Reproducibility failure observed
- [ ] Packaging governance invariant violated

Escalation is required before deployment approval.
