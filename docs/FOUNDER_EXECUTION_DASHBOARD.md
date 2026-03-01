# Founder Execution Dashboard Model (Launch Phase)

Version: LPU-v1.0-DASH  
Date: March 1, 2026  
Status: Active (launch phase)

## 1. Dashboard Structure

The founder dashboard is a weekly control panel with six sections:
1. Launch Packaging Universe Status
2. Tier 1 Hardening Progress
3. Build Stability Indicators
4. Intake SLA Tracker
5. Risk and Escalation Monitor
6. Founder Capacity Guardrail

Each section MUST use 3 to 5 metrics maximum.

## 2. Core Metrics (Minimal Set)

### 2.1 Launch Universe Metrics
- `lpu_total_packages`
- `reproducible_build_percent`
- `unstable_package_count`
- `tier1_fully_hardened_percent`

### 2.2 Build Stability Metrics
- `stage_1_3_failure_count` (target: 0)
- `unclassified_failure_count`
- `avg_build_attempts_per_package`
- `rule_reuse_ratio` (lightweight: reused_rules / total_rule_applications)

### 2.3 Intake SLA Metrics
- `active_intake_requests`
- `sla_compliance_percent`
- `avg_turnaround_business_days`
- `class_c_escalation_count`

### 2.4 Risk Metrics
- `vendoring_exception_count`
- `module_stream_count_delta_weekly`
- `unknown_failure_ratio`
- `nmi_lite`

### 2.5 Capacity Metrics
- `packaging_hours_this_week`
- `packaging_hours_budget_weekly`
- `new_packages_added_this_week`

## 3. NMI-Lite Model (Launch Version)

NMI-Lite is a launch-scope maturity indicator for weekly control.

Inputs:
- `R` = reproducible build percent (0..100)
- `U` = unclassified failure ratio percent (0..100, lower is better)
- `S` = stage isolation integrity percent (0..100); use `100` if Stage 1 to 3 failures are zero, otherwise `max(0, 100 - 20 * stage_1_3_failure_count)`
- `C` = SLA compliance percent (0..100)

Formula:

```text
NMI-Lite = 0.45*R + 0.25*(100-U) + 0.20*S + 0.10*C
```

Interpretation bands:
- `>= 85`: Stable
- `70..84.99`: Caution
- `< 70`: Escalation

This model is intentionally minimal and MUST NOT replace the full normalization maturity model.

## 4. Weekly Review Protocol (20 to 30 Minutes)

Weekly cadence:
1. Update all dashboard metrics from latest build/intake records.
2. Mark red flags.
3. Choose one operating mode for the next week:
   - stabilize existing packages,
   - accept intake within SLA,
   - pause expansion and close debt.
4. Record one-sentence weekly status summary.

Hard stop rules:
- If `stage_1_3_failure_count > 0`, intake MUST be frozen until cleared.
- If `unclassified_failure_count` spikes week-over-week, new rule creation MUST pause until classification stabilizes.
- If `packaging_hours_this_week > packaging_hours_budget_weekly`, new package intake MUST be deferred.

## 5. Visual Layout

### 5.1 Markdown Dashboard Template

```markdown
# Founder Execution Dashboard — Week YYYY-WW

## 1) Launch Packaging Universe Status
| Metric | Value | Target | Status |
|---|---:|---:|---|
| Total LPU-v1 packages |  | 150-200 |  |
| Reproducibly building (%) |  | >=95 |  |
| Remaining unstable (#) |  | as low as possible |  |
| Tier 1 fully hardened (%) |  | >=95 |  |

## 2) Tier 1 Hardening Progress
| Metric | Value | Target | Status |
|---|---:|---:|---|
| Tier 1 total packages |  | fixed |  |
| Tier 1 hardened packages |  | increasing |  |
| Tier 1 hardening completion (%) |  | >=95 |  |

## 3) Build Stability Indicators
| Metric | Value | Target | Status |
|---|---:|---:|---|
| Stage 1-3 failures (#) |  | 0 |  |
| Unclassified failures (#) |  | 0 dominant |  |
| Avg build attempts/pkg |  | <=1.5 |  |
| Rule reuse ratio |  | increasing |  |

## 4) Intake SLA Tracker
| Metric | Value | Target | Status |
|---|---:|---:|---|
| Active intake requests |  | <=capacity |  |
| SLA compliance (%) |  | >=90 |  |
| Avg turnaround (business days) |  | <=5 |  |
| Class C escalations (#) |  | visible |  |

## 5) Risk & Escalation Monitor
| Metric | Value | Target | Status |
|---|---:|---:|---|
| Vendoring exceptions (#) |  | minimal |  |
| Module stream growth (delta) |  | bounded |  |
| Unknown failure ratio (%) |  | trending down |  |
| NMI-Lite |  | >=85 |  |

## 6) Founder Capacity Guardrail
| Metric | Value | Target | Status |
|---|---:|---:|---|
| Packaging hours this week |  | <=budget |  |
| Packaging hours budget |  | fixed weekly |  |
| New packages added this week |  | bounded |  |

Weekly mode decision: [Stabilize / Intake / Pause Expansion]
Weekly summary: <one sentence>
```

### 5.2 Spreadsheet Schema (single tab)

Columns:
- `week`
- `lpu_total_packages`
- `reproducible_build_percent`
- `unstable_package_count`
- `tier1_fully_hardened_percent`
- `stage_1_3_failure_count`
- `unclassified_failure_count`
- `avg_build_attempts_per_package`
- `rule_reuse_ratio`
- `active_intake_requests`
- `sla_compliance_percent`
- `avg_turnaround_business_days`
- `class_c_escalation_count`
- `vendoring_exception_count`
- `module_stream_count_delta_weekly`
- `unknown_failure_ratio`
- `packaging_hours_this_week`
- `packaging_hours_budget_weekly`
- `new_packages_added_this_week`
- `nmi_lite`
- `risk_level`
- `weekly_mode_decision`
- `weekly_summary`

### 5.3 CLI-Friendly Summary Format

```text
week=YYYY-WW nmi_lite=82.4 risk=Caution reproducible=61.3% tier1_hardened=44.0%
stage13_failures=0 unclassified=6 avg_attempts=1.7 intake_active=4 sla=88.0%
vendoring_exceptions=1 module_delta=+2 unknown_ratio=12.0%
hours=9.5/10.0 new_packages=3 mode=Stabilize
summary=Stability improving; intake held to Class A only.
```

## 6. Rust Struct Model

```rust
struct FounderDashboardSnapshot {
    week: String,
    lpu_total: usize,
    reproducible_percent: f64,
    tier1_hardened_percent: f64,
    stage_failure_count: usize,
    unclassified_failures: usize,
    active_intake: usize,
    sla_compliance_percent: f64,
    vendoring_exceptions: usize,
    packaging_hours_this_week: f64,
    nmi_lite: f64,
}

enum RiskLevel {
    Stable,
    Caution,
    Escalation,
}
```

Pseudocode:

```rust
fn compute_nmi_lite(snapshot: &FounderDashboardSnapshot) -> f64 {
    let r = snapshot.reproducible_percent.clamp(0.0, 100.0);
    let unclassified_ratio = if snapshot.lpu_total == 0 {
        100.0
    } else {
        (snapshot.unclassified_failures as f64 / snapshot.lpu_total as f64) * 100.0
    };
    let u = unclassified_ratio.clamp(0.0, 100.0);
    let s = if snapshot.stage_failure_count == 0 {
        100.0
    } else {
        (100.0 - 20.0 * snapshot.stage_failure_count as f64).max(0.0)
    };
    let c = snapshot.sla_compliance_percent.clamp(0.0, 100.0);
    0.45 * r + 0.25 * (100.0 - u) + 0.20 * s + 0.10 * c
}

fn evaluate_risk(snapshot: &FounderDashboardSnapshot) -> RiskLevel {
    if snapshot.stage_failure_count > 0 {
        return RiskLevel::Escalation;
    }
    let nmi = compute_nmi_lite(snapshot);
    if nmi >= 85.0 {
        RiskLevel::Stable
    } else if nmi >= 70.0 {
        RiskLevel::Caution
    } else {
        RiskLevel::Escalation
    }
}
```

## 7. Boundary Statement

This dashboard is launch-phase only.

It:
- does not replace full NMI or stress-test frameworks,
- does not authorize unbounded package expansion,
- is a founder bandwidth protection instrument,
- exists to preserve launch velocity and compliance defensibility.
