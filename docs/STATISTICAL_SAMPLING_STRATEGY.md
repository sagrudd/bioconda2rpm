# Statistical Sampling Strategy for Taxonomy Discovery

Version: 1.0  
Date: March 1, 2026  
Status: Engineering Strategy (implementation-targeted)

## 1. Sampling Objective Formalization

### 1.1 Optimization Objective

Per cycle `t`, define:
- `U_t`: cumulative unique `FailureCategory` classes discovered.
- `F_t`: cumulative taxonomy-eligible failures observed (Stage 5+ only).
- `B_t`: cumulative taxonomy-eligible builds executed.
- `R_t`: rule coverage ratio.
- `D_t`: discovery rate per 10 builds.

Derived signals:
- `DeltaU_t = U_t - U_{t-1}`
- `DeltaF_t = F_t - F_{t-1}`
- `D_t = 10 * DeltaU_t / max(1, DeltaB_t)`
- `Redundancy_t = 1 - (DeltaU_t / max(1, DeltaF_t))`

Utility objective (heuristic):

`J_t = alpha * DeltaU_t - beta * Redundancy_t - gamma * ComputeCost_t`

Recommended defaults:
- `alpha = 1.0`
- `beta = 0.6`
- `gamma = 0.15`

Interpretation:
- maximize new class discovery,
- penalize repeated observation of already-known classes,
- penalize compute-heavy cycles with low novelty.

### 1.2 Rule Coverage Ratio

If taxonomy universe is known:
- `R_t = U_t / U_target`

If taxonomy universe is not known, estimate unseen classes using Chao-like estimator:
- `U_hat = U_obs + (f1^2 / max(1, 2*f2))`
Where:
- `f1`: categories observed exactly once.
- `f2`: categories observed exactly twice.

Then:
- `R_t = U_obs / max(1, U_hat)`

### 1.3 Stopping Criteria

Sampling plateau is declared when all are true:
1. `MA_k(D_t) < 0.25` (new categories per 10 builds), with `k=3`.
2. `R_t >= 0.90`.
3. `DeltaU_t = 0` for 2 consecutive cycles or `DeltaU_t <= 1` for 3 cycles.

Hard stop criteria are defined in Section 8.

## 2. Stratification Model

Universe: ~150 packages from `verification_software.txt`.

### 2.1 Strata Definitions

Primary strata:
1. `CppDominant`
2. `PythonHeavy`
3. `RBioconductor`
4. `HybridCppPython`
5. `RustGo`
6. `VendoredLibRisk`
7. `StrictPinHigh`
8. `CMakeBased`
9. `AutotoolsBased`
10. `PureScripting`

### 2.2 Automated Classification Rules

Feature extraction inputs:
- rendered `meta.yaml`
- `build.sh` (or equivalent script fields)
- dependency graph and requirement specs

Rule examples:
- `PythonHeavy` if deps contain `python`, `pip`, `setuptools`, `py*` dominance and no significant compiled stack.
- `RBioconductor` if deps include `r-*` or `bioconductor-*`.
- `HybridCppPython` if compiled toolchain deps (`gcc`, `cmake`, `make`) and Python runtime deps coexist.
- `RustGo` if deps/scripts contain `cargo`/`rust`/`go`.
- `VendoredLibRisk` if `build.sh` or source tree references `third_party`, `vendor`, `bundled`, `submodule`.
- `StrictPinHigh` if strict pin density >= 0.35.
- `CMakeBased` if `build.sh` contains `cmake`/`ninja` or deps include `cmake`.
- `AutotoolsBased` if `./configure`, `autoconf`, or `automake` patterns appear.
- `PureScripting` if no compiled toolchain and only interpreter/runtime install flow.

Strict pin density:
- `strict_pin_density = strict_pin_count / max(1, total_pin_count)`
- strict pins include exact version operators or exact build selectors.

### 2.3 Assignment Policy

Use multi-label tagging for analysis, but single primary stratum for sampling queue.

Primary stratum precedence:
1. `RBioconductor`
2. `HybridCppPython`
3. `RustGo`
4. `CppDominant`
5. `PythonHeavy`
6. `CMakeBased`
7. `AutotoolsBased`
8. `PureScripting`

Cross-cutting tags retained separately:
- `VendoredLibRisk`
- `StrictPinHigh`

### 2.4 Expected Distribution Model (Initial Prior)

For 150 packages, initial planning prior:
- `CppDominant`: 20%
- `PythonHeavy`: 18%
- `RBioconductor`: 18%
- `HybridCppPython`: 12%
- `RustGo`: 5%
- `CMakeBased`: 10%
- `AutotoolsBased`: 8%
- `PureScripting`: 9%

Cross-cutting prevalence priors:
- `VendoredLibRisk`: 20-30%
- `StrictPinHigh`: 25-35%

## 3. Sampling Strategies

### 3.1 Strategy Comparison

`A) Random Sampling`
- Pros: simple unbiased baseline.
- Cons: poor rare-class discovery efficiency; high redundancy.

`B) Stratified Sampling`
- Pros: guarantees structural coverage.
- Cons: static weights do not react to emerging failure classes.

`C) Adaptive Sampling`
- Pros: reallocates compute to high-novelty strata.
- Cons: can starve low-frequency strata without exploration floor.

`D) Failure-Class Weighted Sampling`
- Pros: focuses on unresolved/high-impact classes.
- Cons: feedback loops can overfit to a dominant class.

`E) Exploration vs Exploitation`
- Pros: balances novelty discovery and rule validation.
- Cons: requires disciplined parameter scheduling.

### 3.2 Recommended Default Strategy

Recommended default: `Stratified Adaptive Novelty-Weighted` (SANW)

Policy:
1. Start with stratified seed to ensure broad structural coverage.
2. Reweight strata by novelty yield and unresolved failure burden.
3. Reserve fixed exploration budget each cycle.
4. Increase exploitation toward unresolved high-frequency classes after cycle 2.

### 3.3 Batch Size Guidance

For `N=150`:
- Initial seed batch: `30` packages (20%).
- Iterative batch: `12` packages per cycle.
- Final confirmation cycle: `10` packages focused on residual unresolved classes.

Exploration schedule:
- Cycle 1-2: 40% exploration, 60% exploitation.
- Cycle 3-5: 30% exploration, 70% exploitation.
- Cycle 6+: 20% exploration, 80% exploitation.

### 3.4 Weight Rebalancing

Per stratum `s` after each cycle:
- `novelty_s = new_categories_s / max(1, failures_s)`
- `burden_s = unresolved_failures_s / max(1, tested_s)`
- `redundancy_s = repeated_failures_s / max(1, failures_s)`

Weight update:

`w'_s = w_base_s * (1 + lambda*novelty_s + mu*burden_s - nu*redundancy_s)`

Normalize:

`w_s = w'_s / sum_j w'_j`

Recommended:
- `lambda = 0.8`
- `mu = 0.5`
- `nu = 0.6`

Exploration floor:
- `w_s >= 0.05` for every primary stratum while untested packages remain.

## 4. Diminishing Returns Detection

### 4.1 Convergence Signals

Compute:
- `S1_t = MA_3(D_t)` discovery moving average.
- `S2_t = DeltaU_t / max(1, DeltaF_t)` novelty efficiency.
- `S3_t = R_t` rule coverage ratio.

Convergence plateau is declared if:
1. `S1_t < 0.25`
2. `S2_t < 0.08`
3. `S3_t >= 0.90`
for `N=3` consecutive cycles.

### 4.2 Delta Trend Heuristic

Track linear slope over last 4 cycles:
- `slope_D = d(D)/dt`

If `slope_D <= 0` and `MA_3(D_t)` stays below threshold, classify as plateau.

## 5. Normalization Impact Feedback Loop

After introducing a normalization rule `r`:

1. Select representative retest subset:
   - all packages previously failing target category `c` (up to 15),
   - plus 5 control packages from adjacent strata.
2. Re-run in sampling mode with stage isolation.
3. Measure before/after category frequency.

Metrics:
- `category_frequency_before(c)`
- `category_frequency_after(c)`
- `residual_failure_density(c) = after_failures_c / retested_packages_c`

Rule effectiveness:

`rule_effectiveness_score(r,c) = max(0, (before - after) / max(1, before))`

Regression penalty:

`penalty = new_unrelated_failures / max(1, total_retested)`

Adjusted effectiveness:

`effective_score = rule_effectiveness_score * (1 - penalty)`

Promotion rule:
- Promote normalization rule to stable only if `effective_score >= 0.50` and no systemic regressions are introduced.

## 6. Sample Size Calculation

### 6.1 Dominant Class Discovery

To detect a class with prevalence `p` at confidence `1-alpha`, probability of at least one observation:

`n >= ln(alpha) / ln(1 - p)`

Examples at 95% confidence (`alpha=0.05`):
- if `p=0.10`, `n >= 29`
- if `p=0.05`, `n >= 59`
- if `p=0.03`, `n >= 99`

Guidance:
- Minimum seed sample: `n=60` equivalent observations across first cycles to expose common classes (>=5% prevalence).
- For the 150-package universe, this is reachable in seed + 3 iterative cycles.

### 6.2 Coverage Confidence for Top N Classes

Declare top-`N` class coverage sufficient when:
1. cumulative share of observed top `N` classes >= 0.80 of taxonomy-eligible failures, and
2. no new top-`N` entrant appears for 3 cycles, and
3. `R_t >= 0.90`.

## 7. Sampling Mode Implementation Contract

### 7.1 Rust Data Structures

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingCycle {
    pub cycle_id: usize,
    pub packages_tested: Vec<String>,
    pub new_failure_categories: Vec<FailureCategory>,
    pub discovery_rate: f64,
    pub convergence_score: f64,
}

pub trait SamplingStrategy {
    fn select_batch(&self, universe: &[PackageMeta]) -> Vec<PackageMeta>;
    fn update_weights(&mut self, results: &[FailureInstance]);
}
```

Supporting contract:

```rust
#[derive(Debug, Clone)]
pub struct SamplingModel {
    pub cycle_id: usize,
    pub stratum_weights: HashMap<Stratum, f64>,
    pub observed_categories: HashSet<FailureCategory>,
    pub discovery_history: Vec<f64>,
    pub convergence_window: Vec<f64>,
}
```

### 7.2 Pseudocode: `run_sampling_cycle()`

```rust
fn run_sampling_cycle(
    strategy: &mut dyn SamplingStrategy,
    universe: &[PackageMeta],
    model: &mut SamplingModel,
    cfg: &ExecutionConfig,
) -> SamplingCycle {
    let batch = strategy.select_batch(universe);
    let results = execute_batch_in_sampling_mode(&batch, cfg);

    let new_categories = collect_new_categories(&results, &model.observed_categories);
    let discovery_rate = 10.0 * (new_categories.len() as f64) / (batch.len().max(1) as f64);

    strategy.update_weights(&results);
    model.observed_categories.extend(new_categories.iter().cloned());
    model.discovery_history.push(discovery_rate);

    let convergence_score = compute_convergence_score(model);

    SamplingCycle {
        cycle_id: model.cycle_id,
        packages_tested: batch.iter().map(|p| p.name.clone()).collect(),
        new_failure_categories: new_categories,
        discovery_rate,
        convergence_score,
    }
}
```

### 7.3 Pseudocode: `update_sampling_model()`

```rust
fn update_sampling_model(model: &mut SamplingModel, cycle: &SamplingCycle) {
    model.cycle_id += 1;
    model.convergence_window.push(cycle.discovery_rate);
    if model.convergence_window.len() > 3 {
        model.convergence_window.remove(0);
    }
}
```

### 7.4 Pseudocode: `detect_convergence()`

```rust
fn detect_convergence(model: &SamplingModel, coverage_ratio: f64) -> bool {
    if model.convergence_window.len() < 3 {
        return false;
    }
    let ma = model.convergence_window.iter().sum::<f64>() / 3.0;
    let low_discovery = ma < 0.25;
    let high_coverage = coverage_ratio >= 0.90;
    low_discovery && high_coverage
}
```

## 8. Governance Safeguards

### 8.1 Anti-Overfitting Controls

Rules:
- Do not create a new normalization rule from a failure observed in fewer than 3 packages unless severity is critical/security.
- Require cross-stratum evidence (at least 2 strata) before promoting structural rule status.
- Rare outliers are recorded as `package_anomaly` until replicated.

### 8.2 Rule Proliferation Controls

Rules:
- Enforce per-cycle maximum of 3 candidate new structural rules.
- Require rule retirement/merge review when candidate registry grows > 25% without net failure reduction.

### 8.3 Loop Termination Controls

Hard termination criteria:
1. `max_cycles = 12`
2. `max_taxonomy_eligible_builds = 150`
3. convergence plateau reached (Section 4)
4. systemic non-build stage failure triggered
5. compute budget exceeded (team-defined wall-clock or container-hour cap)

### 8.4 Compute Waste Controls

Rules:
- Packages with 2 identical first-failure signatures in consecutive cycles are deprioritized unless targeted by a new rule validation cycle.
- Mandatory cache of first-failure signature to avoid blind reruns.

## 9. Required Outputs per Cycle

Each cycle report must emit:
1. `cycle_id`, `batch_size`, `taxonomy_eligible_builds`
2. `new_failure_categories_count`, list of new categories
3. `discovery_rate_per_10_builds`
4. `rule_coverage_ratio`
5. `redundancy_ratio`
6. `stratum_selection_distribution`
7. `top_failure_categories` (frequency table)
8. `candidate_rules_promoted`, `candidate_rules_rejected`
9. `convergence_score` and convergence decision
10. systemic-failure indicator (if any)

Recommended default operating profile:
- Strategy: `SANW` (Stratified Adaptive Novelty-Weighted)
- Seed batch: `30`
- Iterative batch: `12`
- Plateau window: `3` cycles
- Coverage target: `R >= 0.90`
