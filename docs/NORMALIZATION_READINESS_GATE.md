# Normalization Readiness Gate

Version: 1.0  
Date: March 1, 2026  
Status: Governance Standard (deterministic transition control)

## 1. Purpose

This document defines the deterministic gate that controls transition from:
- **Phase A**: Failure Discovery and Taxonomy Enrichment
- **Phase B**: Controlled Normalization Rule Introduction

The gate is fail-closed. If readiness criteria are not fully met, the system remains in Phase A.

## 2. Readiness Dimensions

Notation over a fixed evaluation window `W`:
- `N_total`: total historical + sampling failures in window.
- `N_classified`: failures mapped to canonical taxonomy classes.
- `N_unknown`: failures mapped to `Unknown`.
- `N_structural`: failures labeled as structural (`Structural Ecosystem Mismatch`, `Dependency Translation Defect`, `Toolchain Drift`, `Adapter Infrastructure Issue`, `Module Policy Conflict`, `Vendoring Policy Conflict`).
- `C`: set of canonical failure classes in `W`.
- `Top5(C)`: five most frequent classes in `W`.
- `N_top5`: total failures in `Top5(C)`.
- `N_dom_high_auto`: failures in dominant classes (`Top5`) with `automation_feasibility = High`.
- `U_t`: cumulative unique classes observed by cycle `t`.
- `D_t`: discovery rate per 10 builds in cycle `t`.

### 2.1 Taxonomy Coverage Score (TCS)

`TCS = N_classified / max(1, N_total)`

Interpretation: fraction of failures that map to canonical classes.

### 2.2 Unknown Failure Ratio (UFR)

`UFR = N_unknown / max(1, N_total)`

Interpretation: unresolved taxonomy surface.

### 2.3 Dominant Class Concentration (DCC)

`DCC = N_top5 / max(1, N_total)`

Interpretation: concentration of failures in recurrent classes suitable for systematic normalization.

### 2.4 Structural Class Ratio (SCR)

`SCR = N_structural / max(1, N_total)`

Interpretation: proportion of failures attributable to structural/systemic causes rather than incidental one-offs.

### 2.5 Rule Candidate Confidence (RCC)

Count-weighted formulation:

`RCC = N_dom_high_auto / max(1, N_top5)`

Interpretation: dominant-class failure mass with high automation feasibility.

### 2.6 Discovery Rate Stability (DRS)

Given per-cycle discovery rate `D_t`:

`DRS_t = |D_t - D_{t-1}|`

And moving-average stability over the last `k=3` cycles:

`DRS_MA = mean(DRS_t, DRS_{t-1}, DRS_{t-2})`

Interpretation: taxonomy novelty is no longer volatile; discovery process has converged enough to shift focus.

## 3. Gating Thresholds

Transition eligibility thresholds:
- `TCS >= 0.80`
- `UFR <= 0.20`
- `DCC >= 0.60`
- `SCR >= 0.50`
- `RCC >= 0.70`
- `DRS_MA <= 0.25` for **2 consecutive cycles**

### 3.1 Minimum Evidence Requirement

Gate evaluation is invalid unless all are true:
- `N_total >= 80` failures in evaluation window.
- `N_build_eligible >= 60` Stage 5+ failures (taxonomy-eligible).
- At least **4 completed sampling cycles** after stage-isolation/systemic-failure controls are active.
- At least **2 cycles** since last major taxonomy schema change.

If evidence requirements are not met:
- `PhaseStatus = ContinueDiscovery`
- gate reason: `InsufficientEvidence`

## 4. Phase Transition Rule

Deterministic decision:

```text
IF EvidenceReady
AND TCS >= 0.80
AND UFR <= 0.20
AND DCC >= 0.60
AND SCR >= 0.50
AND RCC >= 0.70
AND DRS_MA <= 0.25 for 2 consecutive cycles
THEN PhaseStatus = ReadyForNormalization
ELSE PhaseStatus = ContinueDiscovery
```

Mandatory constraints:
- No subjective override.
- No manual early transition.
- No partial readiness acceptance.
- No exception path that bypasses failed thresholds.

## 5. Risk Controls

### 5.1 Overfitting Prevention

Rules:
- A normalization rule cannot be promoted if its target class appears in fewer than 3 packages unless tagged `CriticalSecurity`.
- Candidate rules require evidence from at least 2 structural strata unless class is infrastructure-global.

### 5.2 Rule Explosion Prevention

Rules:
- Maximum 3 new structural normalization rules per cycle.
- Long-tail class share (`<3%` per class) above 40% blocks new rule injection and forces additional discovery.

### 5.3 Low-Frequency Noise Control

Rules:
- Classes with frequency `<2%` default to `ObservationOnly` unless repeated across 2 consecutive cycles.
- Package-specific anomalies remain outside structural normalization registry.

### 5.4 Infinite Loop / Compute Waste Control

Rules:
- Hard cap: 12 cycles per discovery campaign.
- Hard cap: 150 taxonomy-eligible builds per campaign window.
- If no new class is discovered in 3 consecutive cycles and thresholds still fail, escalate to taxonomy review rather than continue blind sampling.

## 6. Gate Evaluation Contract (Rust-Oriented)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseStatus {
    ContinueDiscovery,
    ReadyForNormalization,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessMetrics {
    pub tcs: f64,
    pub ufr: f64,
    pub dcc: f64,
    pub scr: f64,
    pub rcc: f64,
    pub drs_ma: f64,
    pub total_failures: usize,
    pub taxonomy_eligible_failures: usize,
    pub cycles_observed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessThresholds {
    pub min_tcs: f64,
    pub max_ufr: f64,
    pub min_dcc: f64,
    pub min_scr: f64,
    pub min_rcc: f64,
    pub max_drs_ma: f64,
    pub min_total_failures: usize,
    pub min_taxonomy_eligible_failures: usize,
    pub min_cycles: usize,
    pub drs_consecutive_cycles: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessDecision {
    pub phase_status: PhaseStatus,
    pub evidence_ready: bool,
    pub all_thresholds_met: bool,
    pub failed_checks: Vec<String>,
    pub metrics: ReadinessMetrics,
}
```

### 6.1 Deterministic Evaluator Pseudocode

```rust
fn evaluate_readiness_gate(
    metrics: &ReadinessMetrics,
    thresholds: &ReadinessThresholds,
    drs_history: &[f64],
) -> ReadinessDecision {
    let mut failed = Vec::new();

    let evidence_ready =
        metrics.total_failures >= thresholds.min_total_failures &&
        metrics.taxonomy_eligible_failures >= thresholds.min_taxonomy_eligible_failures &&
        metrics.cycles_observed >= thresholds.min_cycles;

    if !evidence_ready {
        failed.push("InsufficientEvidence".to_string());
    }
    if metrics.tcs < thresholds.min_tcs {
        failed.push("TCS".to_string());
    }
    if metrics.ufr > thresholds.max_ufr {
        failed.push("UFR".to_string());
    }
    if metrics.dcc < thresholds.min_dcc {
        failed.push("DCC".to_string());
    }
    if metrics.scr < thresholds.min_scr {
        failed.push("SCR".to_string());
    }
    if metrics.rcc < thresholds.min_rcc {
        failed.push("RCC".to_string());
    }

    let stable_drs = drs_history
        .iter()
        .rev()
        .take(thresholds.drs_consecutive_cycles)
        .all(|v| *v <= thresholds.max_drs_ma);
    if !stable_drs {
        failed.push("DRS".to_string());
    }

    let all_thresholds_met = failed.is_empty();
    let phase_status = if all_thresholds_met {
        PhaseStatus::ReadyForNormalization
    } else {
        PhaseStatus::ContinueDiscovery
    };

    ReadinessDecision {
        phase_status,
        evidence_ready,
        all_thresholds_met,
        failed_checks: failed,
        metrics: metrics.clone(),
    }
}
```

## 7. Reporting Requirements

Every gate evaluation must emit:
1. `ReadinessMetrics` snapshot
2. threshold table (`metric`, `observed`, `required`, `pass/fail`)
3. `PhaseStatus`
4. failed-check list
5. recommendation:
   - `ContinueDiscovery`: next-cycle sampling focus
   - `ReadyForNormalization`: limited-scope rule introduction plan

Output artifacts (required):
- `reports/readiness_gate/readiness_gate_<timestamp>.json`
- `reports/readiness_gate/readiness_gate_<timestamp>.md`

## 8. Governance Integration

The readiness gate is normative and binds:
- [Stage Isolation and Sampling Blueprint](./STAGE_ISOLATION_AND_SAMPLING_BLUEPRINT.md)
- [Statistical Sampling Strategy](./STATISTICAL_SAMPLING_STRATEGY.md)
- [Historical Log Mining Report](./historical_log_mining/historical_log_mining_report.md)
- [Module Governance Charter](../MODULE_GOVERNANCE_CHARTER.md)

Phase B work (rule injection) is non-compliant unless latest gate result is `ReadyForNormalization`.
