# Stage Isolation and Sampling Blueprint

Version: 1.0  
Date: March 1, 2026  
Status: Refactor Blueprint (implementation-targeted)

## 1. Stage-Isolated Failure Domains

### 1.1 Pipeline Stages

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PipelineStage {
    Environment,              // Stage 0
    Ingestion,                // Stage 1
    DependencyNormalization,  // Stage 2
    SpecGeneration,           // Stage 3
    SourceNormalization,      // Stage 4
    Build,                    // Stage 5
    PostBuildValidation,      // Stage 6
}
```

### 1.2 Failure Classes

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StageFailure {
    InfrastructureGateFailure,
    RecipeParseFailure,
    AdapterConstraintFailure,
    DependencyResolutionFailure,
    SpecSynthesisFailure,
    BuildFailure,
    ValidationFailure,
}
```

### 1.3 Stage Result Contract

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    pub stage: PipelineStage,
    pub success: bool,
    pub failure: Option<StageFailure>,
    pub failure_signal: Option<String>,
}
```

Rules:
- Every stage function must return `StageResult`.
- Stages execute in strict order and terminate on first failure.
- First failure is persisted with package ID + stage + failure type + signal.
- `InfrastructureGateFailure` and other pre-build failures are explicitly segregated from normalization/build failures.

### 1.4 Stage Execution Interface

```rust
pub trait PipelineStageExecutor {
    fn stage(&self) -> PipelineStage;
    fn run(&self, ctx: &mut PackageContext) -> anyhow::Result<StageResult>;
}
```

## 2. Taxonomy Data Integrity Rules

Rules:
1. Only failures at `PipelineStage::Build` or `PipelineStage::PostBuildValidation` are eligible for normalization taxonomy classification.
2. Failures at `Environment`, `Ingestion`, `DependencyNormalization`, or `SpecGeneration` are classified as `InfrastructureDomain`.
3. Taxonomy percentages exclude `InfrastructureDomain` failures from denominator.
4. If more than 80% of processed packages fail at the same non-build stage, abort run as systemic.

### 2.1 Enforcement Pseudocode

```rust
fn classify_taxonomy_eligibility(stage: PipelineStage) -> TaxonomyEligibility {
    match stage {
        PipelineStage::Build | PipelineStage::PostBuildValidation => TaxonomyEligibility::Eligible,
        _ => TaxonomyEligibility::InfrastructureDomain,
    }
}

fn update_taxonomy_stats(stats: &mut TaxonomyStats, first_failure: &FirstFailureRecord) {
    match classify_taxonomy_eligibility(first_failure.stage) {
        TaxonomyEligibility::Eligible => stats.normalization_failures += 1,
        TaxonomyEligibility::InfrastructureDomain => stats.infrastructure_failures += 1,
    }
}
```

## 3. Sampling Mode Execution Profile

### 3.1 Execution Mode and Config

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    Production,
    Sampling,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    pub mode: ExecutionMode,
    pub enforce_adapter_constraints: bool,
    pub enforce_policy_validation: bool,
    pub collect_failure_taxonomy: bool,
}

impl ExecutionConfig {
    pub fn production() -> Self {
        Self {
            mode: ExecutionMode::Production,
            enforce_adapter_constraints: true,
            enforce_policy_validation: true,
            collect_failure_taxonomy: false,
        }
    }

    pub fn sampling() -> Self {
        Self {
            mode: ExecutionMode::Sampling,
            enforce_adapter_constraints: false,
            enforce_policy_validation: false,
            collect_failure_taxonomy: true,
        }
    }
}
```

### 3.2 Sampling Rules

Sampling mode behavior:
- Adapter non-critical constraint violations are warnings, not hard failures.
- Metadata rendering only aborts on structural invalidity (invalid YAML, missing package identity, unrenderable recipe).
- Non-essential policy checks are logged and deferred.
- First failure capture begins after Stage 4 completion (`Build` or `PostBuildValidation` only) for taxonomy denominator.

Production mode behavior:
- Strict gate enforcement in all stages.
- No bypass or soft-fail for adapter/policy constraints.
- Governance blocking remains mandatory.

## 4. Systemic Failure Detection

### 4.1 Types

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemicFailureReport {
    pub trigger_stage: PipelineStage,
    pub processed_packages: usize,
    pub stage_failures: usize,
    pub failure_ratio: f64,
    pub threshold_ratio: f64,
    pub package_samples: Vec<String>,
    pub dominant_failure_signals: Vec<String>,
}
```

### 4.2 Threshold Policy

Defaults:
- `min_sample_size = 20`
- `systemic_stage_threshold = 0.80`
- applies only to non-build stages (`Environment`..`SourceNormalization`)

### 4.3 Detection Pseudocode

```rust
fn detect_systemic_stage_failure(
    processed: usize,
    stage_counts: &HashMap<PipelineStage, usize>,
    min_sample_size: usize,
    threshold_ratio: f64,
) -> Option<PipelineStage> {
    if processed < min_sample_size {
        return None;
    }
    for stage in [
        PipelineStage::Environment,
        PipelineStage::Ingestion,
        PipelineStage::DependencyNormalization,
        PipelineStage::SpecGeneration,
        PipelineStage::SourceNormalization,
    ] {
        let fails = *stage_counts.get(&stage).unwrap_or(&0);
        let ratio = fails as f64 / processed as f64;
        if ratio > threshold_ratio {
            return Some(stage);
        }
    }
    None
}
```

Systemic action:
- classify `SystemicStageFailure`.
- abort current sampling run.
- write `SystemicFailureReport` to report bundle.
- mark run as invalid for taxonomy trend comparisons.

## 5. Reporting Structure

Sampling run outputs must include:

1. `StageFailureDistribution`  
Columns: `stage`, `count`, `percent_of_processed`

2. `BuildFailureDistribution`  
Columns: `taxonomy_category`, `count`, `percent_of_build_failures`

3. `InfrastructureGateSummary`  
Includes stage-level pre-build failures and dominant signals

4. `FirstMeaningfulFailure` per package  
Fields:
- `package`
- `first_failure_stage`
- `stage_failure`
- `failure_signal`
- `taxonomy_eligible` (`true|false`)

Rules:
- Infrastructure-domain failures are always reported separately.
- Taxonomy denominator uses only taxonomy-eligible records.

## 6. Backward Compatibility and Migration

### 6.1 Migration Steps

1. Wrap existing monolithic flow in explicit stage boundaries.
2. Replace implicit `Err(anyhow)` propagation with mapped `StageFailure`.
3. Introduce conversion utility:
   - `fn map_error_to_stage_failure(stage: PipelineStage, err: &anyhow::Error) -> StageFailure`
4. Preserve legacy behavior under `ExecutionMode::Production` defaults.
5. Add sampling mode CLI switch (for example `--execution-mode sampling`).
6. Remove silent fallbacks; every fallback must log explicit stage + reason.

### 6.2 Transitional Adapter Strategy

```rust
pub trait StageErrorMapper {
    fn map(stage: PipelineStage, err_text: &str) -> StageFailure;
}
```

Initial mappings:
- conda adapter invocation errors -> `AdapterConstraintFailure` (Ingestion)
- unresolved render tokens / missing meta package fields -> `RecipeParseFailure` (Ingestion)
- dependency plan construction failures -> `DependencyResolutionFailure` (DependencyNormalization)
- spec template/render failures -> `SpecSynthesisFailure` (SpecGeneration)
- rpmbuild failures -> `BuildFailure` (Build)

## 7. Pipeline Loop Pseudocode

```rust
fn execute_package_pipeline(ctx: &mut PackageContext, cfg: &ExecutionConfig) -> PackageRunResult {
    let stages: Vec<Box<dyn PipelineStageExecutor>> = vec![
        Box::new(EnvironmentStage),
        Box::new(IngestionStage),
        Box::new(DependencyNormalizationStage),
        Box::new(SpecGenerationStage),
        Box::new(SourceNormalizationStage),
        Box::new(BuildStage),
        Box::new(PostBuildValidationStage),
    ];

    for stage_exec in stages {
        let stage = stage_exec.stage();
        let result = match stage_exec.run(ctx) {
            Ok(sr) => sr,
            Err(err) => StageResult {
                stage,
                success: false,
                failure: Some(map_error_to_stage_failure(stage, &err)),
                failure_signal: Some(compact_error_signal(&err)),
            },
        };

        persist_stage_result(ctx.package_name(), &result);

        if !result.success {
            record_first_failure(ctx.package_name(), &result);
            return PackageRunResult::Failed(result);
        }
    }

    PackageRunResult::Succeeded
}
```

```rust
fn execute_sampling_run(pkgs: &[String], cfg: &ExecutionConfig) -> RunSummary {
    let mut summary = RunSummary::new();
    for pkg in pkgs {
        let mut ctx = PackageContext::new(pkg.clone(), cfg.clone());
        let result = execute_package_pipeline(&mut ctx, cfg);
        summary.record(pkg, result);

        if let Some(stage) = detect_systemic_stage_failure(
            summary.processed_packages(),
            summary.nonbuild_stage_fail_counts(),
            20,
            0.80,
        ) {
            summary.mark_systemic_failure(stage);
            break;
        }
    }
    summary
}
```

## 8. Success Criteria

Measurable outcomes:
- Zero ingestion/dependency/spec-generation failures counted in taxonomy denominator.
- Report-level explicit split between `InfrastructureDomain` and taxonomy-eligible build failures.
- Stable sampling completion over at least 100 packages without denominator contamination.
- Deterministic first-failure capture per package with stage and signal.
