# Normalization Maturity Index (NMI)

Version: 1.0  
Date: March 1, 2026  
Status: Governance Standard (quantitative maturity control)

## 1. Purpose

This standard defines a quantitative maturity model for `bioconda2rpm` to determine transition from:
- **Experimental Bridge System**
to
- **Governed Production-Grade Normalization Engine**

The model is deterministic, multi-dimensional, and fail-closed. It is designed for regulated bioinformatics packaging workflows where heuristic drift is prohibited.

## 2. Maturity Dimensions

Evaluation window `W` (rolling default: last 90 days, minimum 4 sampling cycles + 1 readiness gate evaluation).

Symbols:
- `N_fail`: total failures in `W`
- `N_classified`: failures mapped to canonical taxonomy classes
- `N_unknown`: failures mapped to `Unknown`
- `U_t`: cumulative unique canonical classes at cycle `t`
- `Rules_resolved_builds`: builds resolved by already-approved rules (no new rule)
- `Builds_total`: total builds considered in maturity evaluation
- `Rules_total_t`: total active normalization rules at cycle `t`
- `Rules_redundant_t`: rules flagged as duplicate/overlapping at cycle `t`
- `Attempts_pkg_i`: build attempts for package `i`
- `FPSR`: first-pass success rate
- `S`: set of structural strata
- `S_resolved`: strata with dominant class resolution >= threshold
- `N_rules_with_metadata`: rules with full metadata + approval chain
- `N_rules_untracked_heuristic`: untracked heuristic paths detected
- `N_exceptions`: active governance exceptions
- `N_edges_diverged`: dependency edges violating convergence policy
- `N_edges_total`: total dependency edges
- `V_combo`: observed version combinations across foundational streams
- `V_cap`: approved combinatorics cap

### 2.1 Taxonomy Stability Index (TSI)

Components:
- Classification coverage: `C_cov = N_classified / max(1, N_fail)`
- New-class velocity penalty: `C_new = 1 - min(1, mean(max(0, U_t - U_{t-1})) / 2)`
- Canonical stability: `C_stab = 1 - class_churn_rate`

`TSI = 0.5*C_cov + 0.3*C_stab + 0.2*C_new`

Range: `[0,1]`

### 2.2 Rule Reuse Ratio (RRR)

`RRR = Rules_resolved_builds / max(1, Builds_total)`

Interpretation: ability of existing rule set to handle recurrent failures without new rule creation.

### 2.3 Rule Explosion Control (REC)

Components:
- Registry growth control: `G = 1 - min(1, max(0, growth_rate_rules - g_target) / g_target)` where `g_target=0.10` per cycle.
- Consolidation effectiveness: `K = consolidated_rules / max(1, new_rules + consolidated_rules)`
- Redundancy penalty: `R = 1 - (Rules_redundant_t / max(1, Rules_total_t))`

`REC = 0.4*G + 0.3*K + 0.3*R`

### 2.4 Build Predictability Index (BPI)

Components:
- Attempt efficiency: `A = 1 / (1 + mean_i(Attempts_pkg_i - 1))`
- Strata variance control: `V = 1 - min(1, stddev_strata_attempts / v_max)` with `v_max=1.0`
- First-pass reliability: `P = FPSR`

`BPI = 0.4*P + 0.35*A + 0.25*V`

### 2.5 Cross-Strata Coverage Score (CSCS)

For each stratum `s`, define dominant-class resolution success:
- `resolved_s = 1` if dominant class in `s` has >=50% reduction post-rule validation.
- else `0`.

`CSCS = sum_s(resolved_s) / max(1, |S|)`

### 2.6 Governance Integrity Score (GIS)

Components:
- Rule metadata compliance: `M = N_rules_with_metadata / max(1, Rules_total_t)`
- Heuristic cleanliness: `H = 1 - min(1, N_rules_untracked_heuristic / max(1, Rules_total_t))`
- Exception burden control: `E = 1 - min(1, N_exceptions / e_cap)` with `e_cap=5`

`GIS = 0.45*M + 0.40*H + 0.15*E`

### 2.7 Dependency Convergence Ratio (DCR)

Components:
- Graph convergence: `Gd = 1 - (N_edges_diverged / max(1, N_edges_total))`
- Combinatorics containment: `Vc = 1 - min(1, max(0, V_combo - V_cap) / max(1, V_cap))`
- Stream stabilization: `Ss = stabilized_stream_ratio`

`DCR = 0.5*Gd + 0.25*Vc + 0.25*Ss`

## 3. Composite NMI

Composite index:

`NMI = 100 * ( w_tsi*TSI + w_rrr*RRR + w_rec*REC + w_bpi*BPI + w_cscs*CSCS + w_gis*GIS + w_dcr*DCR )`

Weights (sum=1.0):
- `w_tsi = 0.18`
- `w_rrr = 0.12`
- `w_rec = 0.12`
- `w_bpi = 0.18`
- `w_cscs = 0.10`
- `w_gis = 0.20`
- `w_dcr = 0.10`

Weighting rationale:
- Governance and stability are prioritized over velocity in regulated settings (`GIS`, `TSI`, `BPI` highest combined weight).
- Rule throughput (`RRR`, `REC`) is significant but secondary.
- Cross-strata breadth and dependency convergence are required controls, not primary drivers.

### 3.1 Minimum Production Threshold

Production designation is allowed only if:
- `NMI >= 85`
- `GIS >= 0.90`
- `TSI >= 0.85`
- `BPI >= 0.85`
- readiness gate status = `ReadyForNormalization` (latest snapshot)

## 4. Maturity Tiers

### Level 0 — Experimental (NMI < 35)

Traits:
- Unstable taxonomy.
- High unknown ratio.
- No reliable rule reuse.

Hard indicators:
- `TSI < 0.50` or `GIS < 0.60`

### Level 1 — Structured Discovery (35 <= NMI < 55)

Traits:
- Stage isolation functioning.
- Taxonomy becoming coherent.
- Sampling and mining data usable.

Expected floor:
- `TSI >= 0.60`
- `UFR <= 0.25`

### Level 2 — Controlled Normalization (55 <= NMI < 70)

Traits:
- Rule introduction is controlled.
- Reuse begins to dominate over new-rule creation.

Expected floor:
- `RRR >= 0.55`
- `REC >= 0.60`
- `GIS >= 0.75`

### Level 3 — Convergent Engine (70 <= NMI < 85)

Traits:
- Failure classes concentrated and managed.
- Predictability stable across strata.
- Dependency convergence materially improved.

Expected floor:
- `TSI >= 0.80`
- `BPI >= 0.80`
- `CSCS >= 0.75`
- `DCR >= 0.75`

### Level 4 — Governed Production System (NMI >= 85)

Traits:
- Rule set stable and auditable.
- Minimal heuristic drift.
- Cross-strata convergence achieved.

Mandatory floor:
- `GIS >= 0.90`
- `TSI >= 0.85`
- `BPI >= 0.85`
- `REC >= 0.80`
- readiness gate pass required

## 5. Evaluation Cadence and Data Contract

Cadence:
- Compute provisional NMI after each sampling cycle.
- Compute official NMI weekly and at release candidate cut.

Data sources:
- stage-isolated run reports
- sampling cycle outputs
- historical mining outputs
- readiness gate snapshots
- rule registry metadata
- dependency graph convergence metrics

Minimum window validity:
- at least 80 failures and 60 taxonomy-eligible build failures
- at least 4 sampling cycles in window

If invalid, NMI status = `InsufficientEvidence`.

## 6. Deterministic Promotion and Demotion Rules

Promotion:
- A tier upgrade requires two consecutive official NMI evaluations meeting threshold and floor metrics.

Demotion:
- If two consecutive official evaluations fall below current-tier threshold, demote by one level.
- Immediate demotion to at most Level 2 if:
  - untracked heuristic detected in production path, or
  - `GIS < 0.80`, or
  - readiness gate fails for two consecutive evaluations.

No manual overrides are permitted.

## 7. Drift and Anti-Gaming Controls

Rules:
1. Metric manipulation by excluding difficult strata is forbidden; stratum coverage completeness must be >=90% of active universe.
2. Unknown-class suppression via forced mapping without evidence is forbidden; classifier confidence logs required.
3. Rule count inflation without net failure reduction triggers REC penalty multiplier (`x0.8` on REC for current window).
4. Exceptions older than expiry automatically penalize GIS.

## 8. Rust-Oriented Model Contract

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaturityDimensions {
    pub tsi: f64,
    pub rrr: f64,
    pub rec: f64,
    pub bpi: f64,
    pub cscs: f64,
    pub gis: f64,
    pub dcr: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MaturityLevel {
    Level0Experimental,
    Level1StructuredDiscovery,
    Level2ControlledNormalization,
    Level3ConvergentEngine,
    Level4GovernedProduction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NmiSnapshot {
    pub timestamp_utc: String,
    pub dimensions: MaturityDimensions,
    pub nmi_score: f64,      // 0..100
    pub level: MaturityLevel,
    pub evidence_ready: bool,
    pub readiness_gate_passed: bool,
    pub failed_floor_checks: Vec<String>,
}
```

Pseudocode:

```rust
fn compute_nmi(d: &MaturityDimensions) -> f64 {
    100.0 * (
        0.18*d.tsi + 0.12*d.rrr + 0.12*d.rec +
        0.18*d.bpi + 0.10*d.cscs + 0.20*d.gis + 0.10*d.dcr
    )
}

fn classify_level(score: f64) -> MaturityLevel {
    if score < 35.0 { MaturityLevel::Level0Experimental }
    else if score < 55.0 { MaturityLevel::Level1StructuredDiscovery }
    else if score < 70.0 { MaturityLevel::Level2ControlledNormalization }
    else if score < 85.0 { MaturityLevel::Level3ConvergentEngine }
    else { MaturityLevel::Level4GovernedProduction }
}
```

## 9. Required Outputs

Artifacts per official evaluation:
- `reports/maturity/nmi_<timestamp>.json`
- `reports/maturity/nmi_<timestamp>.md`

Mandatory report sections:
1. Dimension values and formulas
2. Composite NMI and current maturity level
3. Threshold/floor compliance matrix
4. Promotion/demotion decision
5. Drift control findings
6. Actionable focus for next cycle

## 10. Governance Linkage

This NMI model is normative and depends on:
- [Normalization Readiness Gate](./NORMALIZATION_READINESS_GATE.md)
- [Stage Isolation and Sampling Blueprint](./STAGE_ISOLATION_AND_SAMPLING_BLUEPRINT.md)
- [Statistical Sampling Strategy](./STATISTICAL_SAMPLING_STRATEGY.md)
- [Historical Log Mining Report](./historical_log_mining/historical_log_mining_report.md)
- [Module Governance Charter](../MODULE_GOVERNANCE_CHARTER.md)

Production-grade designation is invalid unless both:
- `NMI Level = 4`, and
- readiness gate status = `ReadyForNormalization`.
