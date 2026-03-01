# Historical Log Mining Report

## 1. HistoricalFailureSummary
- Target root: `/Users/stephen/bioconda2rpm/targets/phoreus-bioconda2rpm-build-almalinux-9.7-aarch64`
- Total historical failure records: **142**
- BAD_SPEC artifacts: **3**
- Build logs examined (non-attempt): **103**
- Failure-gathering TSV inputs: **1**
- Patch-injection artifacts observed in failing logs: **3**

### HistoricalFailureRecord schema
`{ package, stage, raw_signal, log_excerpt }`

## 2. Failure Signal Extraction Rules (Deterministic)

Regex-style ordered extraction patterns:

| Priority | Pattern (abridged) | CanonicalClass |
|---:|---|---|
| 1 | `(conda[_-]?build.*(not installed\|import failed)\|No module named 'conda_build')` | MetadataAdapterRuntimeMissing |
| 2 | `failed to parse rendered metadata` | MetadataRenderFailure |
| 3 | `blocked by failed dependencies` | DependencyBlockedCascade |
| 4 | `(DEPGRAPH\\|[^\|]+\\|unresolved\\|\|Failed build dependencies:\|No match for argument:)` | UnresolvedBuildRequiresToken |
| 5 | `(unresolved R deps after restore\|dependency '.*' is not available)` | RDependencyRestoreFailure |
| 6 | `(No module named \|ImportError:\|DistributionNotFound)` | PythonImportOrABIError |
| 7 | `fatal error: .*No such file or directory` | MissingHeaderOrIncludePath |
| 8 | `(undefined reference to\|cannot find -l\|no usable version found)` | MissingLinkTimeDependency |
| 9 | `CMake Error` | CMakeConfigurationFailure |
| 10 | `configure: error:` | AutotoolsConfigureFailure |
| 11 | `(can't find file to patch\|No file to patch\|Hunk .*FAILED)` | PatchApplicationFailure |
| 12 | `(source download failed after retries\|Downloaded: .* failed)` | SourceFetchFailure |
| 13 | `(empty string invalid as file name\|No rule to make target\|No targets specified and no makefile ...` | BuildScriptContractFailure |
| 14 | `(exit status: 137\|signal: 9\|Killed)` | ToolchainResourceExhaustion |
| 15 | `Bad exit status from .* \(%install\)` | RpmInstallScriptFailure |

Noise suppression rules:
- Ignore empty lines.
- Ignore rust `error-format=json` command lines.
- If no deterministic pattern matches, fallback to first explicit `error:` line.

## 3. Canonicalization

| CanonicalClass | Occurrences | RepresentativeExamples |
|---|---:|---|
| MetadataRenderFailure | 133 | failed to parse rendered metadata for /Users/stephen/bioconda2rpm/bioconda-recipes/recipes/abyss/meta.yaml <br> failed to parse rendered metadata for /Users/stephen/bioconda2rpm/bioconda-recipes/recipes/alevin-fry/meta.yaml <br> failed to parse rendered metadata for /Users/stephen/bioconda2rpm/bioconda-recipes/recipes/any2fasta/meta.yaml |
| UnresolvedBuildRequiresToken | 3 | DEPGRAPH\|jsoncpp-devel\|unresolved\|unresolved\|-\|Last metadata expiration check: 0:00:06 ago on Sun Mar  1 00:44:03 2026.; No match for argument: jsoncpp-devel; Error: Unable to find a match: jsoncpp-devel; <br> DEPGRAPH\|isa-l\|unresolved\|unresolved\|-\|AlmaLinux 9 - Extras                             32 kB/s /  20 kB     00:00    ; No match for argument: isa-l; Error: Unable to find a match: isa-l; <br> DEPGRAPH\|highway-devel\|unresolved\|unresolved\|-\|AlmaLinux 9 - Extras                             40 kB/s /  20 kB     00:00    ; No match for argument: highway-devel; Error: Unable to find a match: highway-devel; |
| BuildScriptContractFailure | 2 | + CPU_COUNT=1 \| + export MAKEFLAGS=-j1 \| + MAKEFLAGS=-j1 \| + export CMAKE_BUILD_PARALLEL_LEVEL=1 \| + CMAKE_BUILD_PARALLEL_LEVEL=1 \| + export NINJAFLAGS=-j1 \| + NINJAFLAGS=-j1 \| ++ basename /work/.build-work/minimap2/BUILD/buildsrc/.bioconda <br> make: *** empty string invalid as file name.  Stop. |
| AutotoolsConfigureFailure | 1 | configure: error: in `/work/.build-work/flye/BUILD/buildsrc/lib/samtools-1.9': |
| MissingLinkTimeDependency | 1 | checking for FCGX_Accept_r... no \| checking for libfastcgipp... no \| checking for NCBI SSS directories in /sss/BUILD... no \| checking for SP directories... no \| checking for ORBacus... no \| checking for ICU... no \| checking for libexpat...  |
| PatchApplicationFailure | 1 | can't find file to patch at input line 6 |
| ToolchainResourceExhaustion | 1 | Downloading: https://github.com/samtools/htslib/releases/download/1.23/htslib-1.23.tar.bz2 \| |

## 4. CanonicalFailureDistribution

| FailureCategory | Count | % of Historical Failures |
|---|---:|---:|
| MetadataRenderFailure | 133 | 93.66% |
| UnresolvedBuildRequiresToken | 3 | 2.11% |
| BuildScriptContractFailure | 2 | 1.41% |
| AutotoolsConfigureFailure | 1 | 0.70% |
| MissingLinkTimeDependency | 1 | 0.70% |
| PatchApplicationFailure | 1 | 0.70% |
| ToolchainResourceExhaustion | 1 | 0.70% |

## 5. Frequency & Stage Distribution

| PipelineStage | Count | % |
|---|---:|---:|
| Ingestion | 133 | 93.66% |
| Build | 5 | 3.52% |
| DependencyNormalization | 3 | 2.11% |
| SourceNormalization | 1 | 0.70% |

### Top 10 recurring canonical classes

| Rank | FailureCategory | Count | % |
|---:|---|---:|---:|
| 1 | MetadataRenderFailure | 133 | 93.66% |
| 2 | UnresolvedBuildRequiresToken | 3 | 2.11% |
| 3 | BuildScriptContractFailure | 2 | 1.41% |
| 4 | AutotoolsConfigureFailure | 1 | 0.70% |
| 5 | MissingLinkTimeDependency | 1 | 0.70% |
| 6 | PatchApplicationFailure | 1 | 0.70% |
| 7 | ToolchainResourceExhaustion | 1 | 0.70% |

### Long-tail classes (<3% frequency)

| FailureCategory | Count | % |
|---|---:|---:|
| UnresolvedBuildRequiresToken | 3 | 2.11% |
| BuildScriptContractFailure | 2 | 1.41% |
| AutotoolsConfigureFailure | 1 | 0.70% |
| MissingLinkTimeDependency | 1 | 0.70% |
| PatchApplicationFailure | 1 | 0.70% |
| ToolchainResourceExhaustion | 1 | 0.70% |

## 6. StructuralVsIncidentalBreakdown

| FailureCategory | Structural Label | Count |
|---|---|---:|
| MetadataRenderFailure | Adapter Infrastructure Issue | 133 |
| UnresolvedBuildRequiresToken | Dependency Translation Defect | 3 |
| BuildScriptContractFailure | Upstream Build Defect | 2 |
| AutotoolsConfigureFailure | Toolchain Drift | 1 |
| MissingLinkTimeDependency | Dependency Translation Defect | 1 |
| PatchApplicationFailure | Upstream Build Defect | 1 |
| ToolchainResourceExhaustion | Toolchain Drift | 1 |

## 7. NormalizationCandidateMatrix

| FailureCategory | Deterministic Normalization Opportunity | Proposed Layer | Automation Feasibility | Estimated Impact % |
|---|---|---|---|---:|
| MetadataRenderFailure | yes | ModulePolicy | High | 93.66% |
| UnresolvedBuildRequiresToken | yes | DependencyClassifier | High | 2.11% |
| BuildScriptContractFailure | no | SourceNormalizer | Low | 1.41% |
| AutotoolsConfigureFailure | yes | SourceNormalizer | Medium | 0.70% |
| MissingLinkTimeDependency | yes | DependencyClassifier | High | 0.70% |
| PatchApplicationFailure | no | SourceNormalizer | Low | 0.70% |
| ToolchainResourceExhaustion | yes | GovernanceException | Low | 0.70% |

## 8. ClassifierCoverageScore

- Classified: **142/142 (100.00%)**
- Unknown: **0/142 (0.00%)**
- Taxonomy insufficiency flag (`Unknown > 20%`): **false**

## 9. HeuristicDriftFindings

### Repeated manual-signature candidates (package + canonical class, occurrences >=2)

| Package | FailureCategory | Occurrences |
|---|---|---:|
| minimap2 | BuildScriptContractFailure | 2 |

### Patch activity not ending in PatchApplicationFailure

| Package | FailureCategory | Artifact |
|---|---|---|
| flye | AutotoolsConfigureFailure | `/Users/stephen/bioconda2rpm/targets/phoreus-bioconda2rpm-build-almalinux-9.7-aarch64/reports/build_logs/flye.log` |
| minimap2 | BuildScriptContractFailure | `/Users/stephen/bioconda2rpm/targets/phoreus-bioconda2rpm-build-almalinux-9.7-aarch64/reports/build_logs/minimap2.log` |

- Long-tail share: **6.34%**
- Rule proliferation indicator (long-tail share > 40%): **false**

## 10. StrategicRecommendations for Live Sampling Readiness

1. Keep metadata-adapter runtime preflight as a hard campaign gate to prevent ingestion-stage dataset contamination.
2. Prioritize deterministic normalization for `UnresolvedBuildRequiresToken`, `MissingHeaderOrIncludePath`, and `MissingLinkTimeDependency` before expanding live sampling breadth.
3. Treat `PatchApplicationFailure` and `BuildScriptContractFailure` as lower-automation classes requiring targeted SourceNormalizer policy, not package-local heuristics.
4. Use this mined distribution as a seed prior for stratified adaptive sampling weights; refresh after each normalization rule cycle.

## 11. Artifact Paths
- Records: `/Users/stephen/Projects/bioconda2rpm/docs/historical_log_mining/historical_failure_records.tsv`
- Clusters: `/Users/stephen/Projects/bioconda2rpm/docs/historical_log_mining/historical_failure_clusters.tsv`
- Candidates: `/Users/stephen/Projects/bioconda2rpm/docs/historical_log_mining/historical_normalization_candidates.tsv`
- Summary JSON: `/Users/stephen/Projects/bioconda2rpm/docs/historical_log_mining/historical_log_mining_summary.json`
