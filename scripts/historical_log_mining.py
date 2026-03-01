#!/usr/bin/env python3
"""Deterministic historical failure log mining for bioconda2rpm."""

from __future__ import annotations

import argparse
import csv
import json
import re
from collections import Counter, defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable


@dataclass(frozen=True)
class Rule:
    canonical: str
    stage: str
    stage_failure: str
    pattern: re.Pattern[str]
    structural_label: str
    normalization_layer: str
    automation_feasibility: str
    deterministic_opportunity: bool


RULES: list[Rule] = [
    Rule(
        "MetadataAdapterRuntimeMissing",
        "Ingestion",
        "AdapterConstraintFailure",
        re.compile(
            r"(conda[_-]?build.*(not installed|import failed)|No module named 'conda_build')",
            re.IGNORECASE,
        ),
        "Adapter Infrastructure Issue",
        "ModulePolicy",
        "High",
        True,
    ),
    Rule(
        "MetadataRenderFailure",
        "Ingestion",
        "RecipeParseFailure",
        re.compile(r"failed to parse rendered metadata", re.IGNORECASE),
        "Adapter Infrastructure Issue",
        "ModulePolicy",
        "High",
        True,
    ),
    Rule(
        "DependencyBlockedCascade",
        "DependencyNormalization",
        "DependencyResolutionFailure",
        re.compile(r"blocked by failed dependencies", re.IGNORECASE),
        "Dependency Translation Defect",
        "DependencyClassifier",
        "High",
        True,
    ),
    Rule(
        "UnresolvedBuildRequiresToken",
        "DependencyNormalization",
        "DependencyResolutionFailure",
        re.compile(
            r"(DEPGRAPH\|[^|]+\|unresolved\||Failed build dependencies:|No match for argument:)",
            re.IGNORECASE,
        ),
        "Dependency Translation Defect",
        "DependencyClassifier",
        "High",
        True,
    ),
    Rule(
        "RDependencyRestoreFailure",
        "Build",
        "BuildFailure",
        re.compile(r"(unresolved R deps after restore|dependency '.*' is not available)", re.IGNORECASE),
        "Structural Ecosystem Mismatch",
        "DependencyClassifier",
        "Medium",
        True,
    ),
    Rule(
        "PythonImportOrABIError",
        "Build",
        "BuildFailure",
        re.compile(r"(No module named |ImportError:|DistributionNotFound)", re.IGNORECASE),
        "Structural Ecosystem Mismatch",
        "DependencyClassifier",
        "Medium",
        True,
    ),
    Rule(
        "MissingHeaderOrIncludePath",
        "Build",
        "BuildFailure",
        re.compile(r"fatal error: .*No such file or directory", re.IGNORECASE),
        "Dependency Translation Defect",
        "DependencyClassifier",
        "High",
        True,
    ),
    Rule(
        "MissingLinkTimeDependency",
        "Build",
        "BuildFailure",
        re.compile(r"(undefined reference to|cannot find -l|no usable version found)", re.IGNORECASE),
        "Dependency Translation Defect",
        "DependencyClassifier",
        "High",
        True,
    ),
    Rule(
        "CMakeConfigurationFailure",
        "Build",
        "BuildFailure",
        re.compile(r"CMake Error", re.IGNORECASE),
        "Toolchain Drift",
        "SourceNormalizer",
        "Medium",
        True,
    ),
    Rule(
        "AutotoolsConfigureFailure",
        "Build",
        "BuildFailure",
        re.compile(r"configure: error:", re.IGNORECASE),
        "Toolchain Drift",
        "SourceNormalizer",
        "Medium",
        True,
    ),
    Rule(
        "PatchApplicationFailure",
        "SourceNormalization",
        "SpecSynthesisFailure",
        re.compile(r"(can't find file to patch|No file to patch|Hunk .*FAILED)", re.IGNORECASE),
        "Upstream Build Defect",
        "SourceNormalizer",
        "Low",
        False,
    ),
    Rule(
        "SourceFetchFailure",
        "SourceNormalization",
        "InfrastructureGateFailure",
        re.compile(r"(source download failed after retries|Downloaded: .* failed)", re.IGNORECASE),
        "Adapter Infrastructure Issue",
        "SourceNormalizer",
        "Medium",
        True,
    ),
    Rule(
        "BuildScriptContractFailure",
        "Build",
        "BuildFailure",
        re.compile(
            r"(empty string invalid as file name|No rule to make target|No targets specified and no makefile found|C compiler cannot create executables)",
            re.IGNORECASE,
        ),
        "Upstream Build Defect",
        "SourceNormalizer",
        "Low",
        False,
    ),
    Rule(
        "ToolchainResourceExhaustion",
        "Build",
        "BuildFailure",
        re.compile(r"(exit status: 137|signal: 9|Killed)", re.IGNORECASE),
        "Toolchain Drift",
        "GovernanceException",
        "Low",
        True,
    ),
    Rule(
        "RpmInstallScriptFailure",
        "Build",
        "BuildFailure",
        re.compile(r"Bad exit status from .* \(%install\)", re.IGNORECASE),
        "Upstream Build Defect",
        "SpecGenerator",
        "Low",
        False,
    ),
]


def file_ts(path: Path) -> str:
    dt = datetime.fromtimestamp(path.stat().st_mtime, tz=timezone.utc)
    return dt.isoformat()


def classify_text(text: str) -> Rule | None:
    for rule in RULES:
        if rule.pattern.search(text):
            return rule
    return None


def first_signal(lines: list[str]) -> tuple[str, int]:
    """Return first meaningful signal + index."""
    for i, line in enumerate(lines):
        s = line.strip()
        if not s:
            continue
        if "error-format=json" in s:
            continue
        rule = classify_text(s)
        if rule is not None:
            return s, i
    # fallback: first explicit error line
    for i, line in enumerate(lines):
        s = line.strip()
        if re.search(r"\berror:\b", s, re.IGNORECASE):
            return s, i
    return "", -1


def excerpt(lines: list[str], idx: int, width: int = 2) -> str:
    if idx < 0:
        return ""
    lo = max(0, idx - width)
    hi = min(len(lines), idx + width + 1)
    part = [ln.strip() for ln in lines[lo:hi] if ln.strip()]
    return " | ".join(part)


def package_from_log(path: Path) -> str:
    n = path.name
    n = n.removesuffix(".log")
    n = re.sub(r"\.attempt\d+$", "", n)
    return n


def read_failure_gathering_tsv(tsv: Path) -> list[dict]:
    rows: list[dict] = []
    with tsv.open("r", encoding="utf-8") as fh:
        reader = csv.DictReader(fh, delimiter="\t")
        for row in reader:
            signal = (row.get("FailureSignal") or "").strip()
            if not signal:
                continue
            rule = classify_text(signal) or classify_text(row.get("FirstFailureCategory", ""))
            rows.append(
                {
                    "source_type": "adapter_failure_tsv",
                    "artifact_path": str(tsv),
                    "timestamp": file_ts(tsv),
                    "package": (row.get("Package") or "").strip(),
                    "stage": rule.stage if rule else "Ingestion",
                    "stage_failure": rule.stage_failure if rule else "InfrastructureGateFailure",
                    "canonical_class": rule.canonical if rule else "Unknown",
                    "raw_signal": signal,
                    "log_excerpt": signal,
                    "execution_mode": (
                        "Production"
                        if "deployment=Production" in (row.get("ModuleContext") or "")
                        else "Unknown"
                    ),
                }
            )
    return rows


def read_bad_spec(path: Path) -> dict | None:
    text = path.read_text(encoding="utf-8", errors="replace")
    reason = ""
    for line in text.splitlines():
        if line.startswith("reason="):
            reason = line[len("reason=") :].strip()
            break
    if not reason:
        return None
    signal = reason
    if " tail=" in reason:
        signal = reason.split(" tail=", 1)[1].strip()
    rule = classify_text(signal) or classify_text(reason)
    return {
        "source_type": "bad_spec",
        "artifact_path": str(path),
        "timestamp": file_ts(path),
        "package": path.stem,
        "stage": rule.stage if rule else "Build",
        "stage_failure": rule.stage_failure if rule else "BuildFailure",
        "canonical_class": rule.canonical if rule else "Unknown",
        "raw_signal": signal,
        "log_excerpt": reason[:1200],
        "execution_mode": "Unknown",
    }


def log_failed(lines: Iterable[str]) -> bool:
    rx = re.compile(
        r"(DEPGRAPH\|[^|]+\|unresolved\||Failed build dependencies:|No match for argument:|fatal error:|configure: error:|CMake Error|No module named |ImportError:|Bad exit status from|undefined reference to|cannot find -l|can't find file to patch|No file to patch|source download failed after retries|blocked by failed dependencies|signal: 9|exit status: 137)",
        re.IGNORECASE,
    )
    return any(rx.search(ln) for ln in lines)


def read_build_log(path: Path) -> dict | None:
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    if not log_failed(lines):
        return None
    signal, idx = first_signal(lines)
    if not signal:
        return None
    rule = classify_text(signal)
    return {
        "source_type": "build_log",
        "artifact_path": str(path),
        "timestamp": file_ts(path),
        "package": package_from_log(path),
        "stage": rule.stage if rule else "Build",
        "stage_failure": rule.stage_failure if rule else "BuildFailure",
        "canonical_class": rule.canonical if rule else "Unknown",
        "raw_signal": signal,
        "log_excerpt": excerpt(lines, idx),
        "execution_mode": "Unknown",
        "has_patch_activity": any("patching file" in ln for ln in lines),
    }


def structural_label(canonical_class: str) -> str:
    for r in RULES:
        if r.canonical == canonical_class:
            return r.structural_label
    return "Adapter Infrastructure Issue"


def candidate_meta(canonical_class: str) -> tuple[str, str, bool]:
    for r in RULES:
        if r.canonical == canonical_class:
            return r.normalization_layer, r.automation_feasibility, r.deterministic_opportunity
    return ("GovernanceException", "Low", False)


def pct(num: int, den: int) -> float:
    return (100.0 * num / den) if den else 0.0


def write_tsv(path: Path, rows: list[dict], fields: list[str]) -> None:
    with path.open("w", encoding="utf-8", newline="") as fh:
        writer = csv.DictWriter(fh, fieldnames=fields, delimiter="\t")
        writer.writeheader()
        for row in rows:
            writer.writerow({k: row.get(k, "") for k in fields})


def run(target_root: Path, out_dir: Path) -> None:
    bad_spec_dir = target_root / "BAD_SPEC"
    build_logs_dir = target_root / "reports" / "build_logs"
    failure_gathering_dir = target_root / "reports" / "failure_gathering"

    records: list[dict] = []
    patch_activity_logs = 0

    bad_spec_files = sorted(bad_spec_dir.glob("*.txt"))
    for p in bad_spec_files:
        rec = read_bad_spec(p)
        if rec:
            records.append(rec)

    # Deterministic build-log selection:
    # use non-attempt final logs only; keep both payload and -default variants as separate contexts.
    build_logs = sorted(
        p
        for p in build_logs_dir.glob("*.log")
        if ".attempt" not in p.name
    )
    for p in build_logs:
        rec = read_build_log(p)
        if rec:
            records.append(rec)
            if rec.get("has_patch_activity"):
                patch_activity_logs += 1

    failure_tsvs = sorted(failure_gathering_dir.glob("*_per_package.tsv"))
    for tsv in failure_tsvs:
        records.extend(read_failure_gathering_tsv(tsv))

    total = len(records)
    category_counts = Counter(r["canonical_class"] for r in records)
    stage_counts = Counter(r["stage"] for r in records)
    source_counts = Counter(r["source_type"] for r in records)

    clusters = []
    by_cat: dict[str, list[dict]] = defaultdict(list)
    for r in records:
        by_cat[r["canonical_class"]].append(r)
    for cat, cat_rows in sorted(by_cat.items(), key=lambda kv: (-len(kv[1]), kv[0])):
        examples = []
        seen = set()
        for row in cat_rows:
            sig = row["raw_signal"]
            if sig not in seen:
                seen.add(sig)
                examples.append(sig[:240])
            if len(examples) >= 3:
                break
        clusters.append(
            {
                "canonical_class": cat,
                "occurrences": len(cat_rows),
                "representative_examples": examples,
                "structural_label": structural_label(cat),
            }
        )

    top10 = sorted(category_counts.items(), key=lambda kv: (-kv[1], kv[0]))[:10]
    long_tail = [(k, v) for k, v in category_counts.items() if pct(v, total) < 3.0]

    normalization_candidates = []
    for cat, cnt in sorted(category_counts.items(), key=lambda kv: (-kv[1], kv[0])):
        layer, feasibility, deterministic = candidate_meta(cat)
        normalization_candidates.append(
            {
                "failure_category": cat,
                "proposed_layer": layer,
                "automation_feasibility": feasibility,
                "deterministic_normalization_opportunity": "yes" if deterministic else "no",
                "estimated_impact_percent": round(pct(cnt, total), 2),
            }
        )

    unknown = category_counts.get("Unknown", 0)
    classified = total - unknown
    coverage = pct(classified, total)
    unknown_pct = pct(unknown, total)
    taxonomy_insufficient = unknown_pct > 20.0

    # Heuristic drift signals
    repeated = Counter((r["package"], r["canonical_class"]) for r in records)
    repeated_signatures = [
        {"package": pkg, "canonical_class": cat, "occurrences": n}
        for (pkg, cat), n in repeated.items()
        if n >= 2
    ]
    repeated_signatures.sort(key=lambda x: (-x["occurrences"], x["package"], x["canonical_class"]))

    patch_drift = []
    for r in records:
        if r.get("source_type") == "build_log" and r.get("has_patch_activity"):
            if r["canonical_class"] not in {"PatchApplicationFailure"}:
                patch_drift.append(
                    {
                        "package": r["package"],
                        "canonical_class": r["canonical_class"],
                        "artifact_path": r["artifact_path"],
                    }
                )

    long_tail_share = pct(sum(v for _, v in long_tail), total)
    rule_proliferation_indicator = long_tail_share > 40.0

    out_dir.mkdir(parents=True, exist_ok=True)
    records_tsv = out_dir / "historical_failure_records.tsv"
    clusters_tsv = out_dir / "historical_failure_clusters.tsv"
    candidates_tsv = out_dir / "historical_normalization_candidates.tsv"
    summary_json = out_dir / "historical_log_mining_summary.json"
    report_md = out_dir / "historical_log_mining_report.md"

    write_tsv(
        records_tsv,
        records,
        [
            "package",
            "stage",
            "stage_failure",
            "canonical_class",
            "source_type",
            "timestamp",
            "execution_mode",
            "raw_signal",
            "log_excerpt",
            "artifact_path",
        ],
    )
    write_tsv(
        clusters_tsv,
        clusters,
        [
            "canonical_class",
            "occurrences",
            "structural_label",
            "representative_examples",
        ],
    )
    write_tsv(
        candidates_tsv,
        normalization_candidates,
        [
            "failure_category",
            "proposed_layer",
            "automation_feasibility",
            "deterministic_normalization_opportunity",
            "estimated_impact_percent",
        ],
    )

    payload = {
        "target_root": str(target_root),
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "input_discovery": {
            "bad_spec_files": len(bad_spec_files),
            "build_logs_examined": len(build_logs),
            "failure_gathering_tsv_files": len(failure_tsvs),
            "patch_activity_logs": patch_activity_logs,
            "source_counts": dict(source_counts),
        },
        "classifier_validation": {
            "total_historical_failures": total,
            "classified_count": classified,
            "classified_percent": round(coverage, 2),
            "unknown_count": unknown,
            "unknown_percent": round(unknown_pct, 2),
            "taxonomy_insufficiency_flag": taxonomy_insufficient,
        },
        "top10_classes": top10,
        "stage_distribution": dict(stage_counts),
        "long_tail_share_percent": round(long_tail_share, 2),
        "rule_proliferation_indicator": rule_proliferation_indicator,
        "repeated_signatures_top": repeated_signatures[:20],
        "patch_drift_top": patch_drift[:20],
        "artifacts": {
            "records_tsv": str(records_tsv),
            "clusters_tsv": str(clusters_tsv),
            "candidates_tsv": str(candidates_tsv),
            "report_md": str(report_md),
        },
    }
    summary_json.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    # markdown report
    lines = []
    lines.append("# Historical Log Mining Report")
    lines.append("")
    lines.append("## 1. HistoricalFailureSummary")
    lines.append(f"- Target root: `{target_root}`")
    lines.append(f"- Total historical failure records: **{total}**")
    lines.append(f"- BAD_SPEC artifacts: **{len(bad_spec_files)}**")
    lines.append(f"- Build logs examined (non-attempt): **{len(build_logs)}**")
    lines.append(f"- Failure-gathering TSV inputs: **{len(failure_tsvs)}**")
    lines.append(f"- Patch-injection artifacts observed in failing logs: **{patch_activity_logs}**")
    lines.append("")
    lines.append("### HistoricalFailureRecord schema")
    lines.append("`{ package, stage, raw_signal, log_excerpt }`")
    lines.append("")
    lines.append("## 2. Failure Signal Extraction Rules (Deterministic)")
    lines.append("")
    lines.append("Regex-style ordered extraction patterns:")
    lines.append("")
    lines.append("| Priority | Pattern (abridged) | CanonicalClass |")
    lines.append("|---:|---|---|")
    for i, rule in enumerate(RULES, start=1):
        patt = rule.pattern.pattern.replace("|", "\\|")
        if len(patt) > 100:
            patt = patt[:97] + "..."
        lines.append(f"| {i} | `{patt}` | {rule.canonical} |")
    lines.append("")
    lines.append("Noise suppression rules:")
    lines.append("- Ignore empty lines.")
    lines.append("- Ignore rust `error-format=json` command lines.")
    lines.append("- If no deterministic pattern matches, fallback to first explicit `error:` line.")
    lines.append("")
    lines.append("## 3. Canonicalization")
    lines.append("")
    lines.append("| CanonicalClass | Occurrences | RepresentativeExamples |")
    lines.append("|---|---:|---|")
    for cluster in clusters[:10]:
        ex = " <br> ".join(cluster["representative_examples"]).replace("|", "\\|")
        lines.append(f"| {cluster['canonical_class']} | {cluster['occurrences']} | {ex} |")
    lines.append("")
    lines.append("## 4. CanonicalFailureDistribution")
    lines.append("")
    lines.append("| FailureCategory | Count | % of Historical Failures |")
    lines.append("|---|---:|---:|")
    for cat, cnt in sorted(category_counts.items(), key=lambda kv: (-kv[1], kv[0])):
        lines.append(f"| {cat} | {cnt} | {pct(cnt, total):.2f}% |")
    lines.append("")
    lines.append("## 5. Frequency & Stage Distribution")
    lines.append("")
    lines.append("| PipelineStage | Count | % |")
    lines.append("|---|---:|---:|")
    for stg, cnt in sorted(stage_counts.items(), key=lambda kv: (-kv[1], kv[0])):
        lines.append(f"| {stg} | {cnt} | {pct(cnt, total):.2f}% |")
    lines.append("")
    lines.append("### Top 10 recurring canonical classes")
    lines.append("")
    lines.append("| Rank | FailureCategory | Count | % |")
    lines.append("|---:|---|---:|---:|")
    for i, (cat, cnt) in enumerate(top10, start=1):
        lines.append(f"| {i} | {cat} | {cnt} | {pct(cnt, total):.2f}% |")
    lines.append("")
    lines.append("### Long-tail classes (<3% frequency)")
    lines.append("")
    lines.append("| FailureCategory | Count | % |")
    lines.append("|---|---:|---:|")
    for cat, cnt in sorted(long_tail, key=lambda kv: (-kv[1], kv[0])):
        lines.append(f"| {cat} | {cnt} | {pct(cnt, total):.2f}% |")
    lines.append("")
    lines.append("## 6. StructuralVsIncidentalBreakdown")
    lines.append("")
    lines.append("| FailureCategory | Structural Label | Count |")
    lines.append("|---|---|---:|")
    for cat, cnt in sorted(category_counts.items(), key=lambda kv: (-kv[1], kv[0])):
        lines.append(f"| {cat} | {structural_label(cat)} | {cnt} |")
    lines.append("")
    lines.append("## 7. NormalizationCandidateMatrix")
    lines.append("")
    lines.append("| FailureCategory | Deterministic Normalization Opportunity | Proposed Layer | Automation Feasibility | Estimated Impact % |")
    lines.append("|---|---|---|---|---:|")
    for row in normalization_candidates:
        lines.append(
            "| {failure_category} | {deterministic_normalization_opportunity} | {proposed_layer} | {automation_feasibility} | {estimated_impact_percent:.2f}% |".format(
                **row
            )
        )
    lines.append("")
    lines.append("## 8. ClassifierCoverageScore")
    lines.append("")
    lines.append(f"- Classified: **{classified}/{total} ({coverage:.2f}%)**")
    lines.append(f"- Unknown: **{unknown}/{total} ({unknown_pct:.2f}%)**")
    lines.append(f"- Taxonomy insufficiency flag (`Unknown > 20%`): **{str(taxonomy_insufficient).lower()}**")
    lines.append("")
    lines.append("## 9. HeuristicDriftFindings")
    lines.append("")
    lines.append("### Repeated manual-signature candidates (package + canonical class, occurrences >=2)")
    lines.append("")
    lines.append("| Package | FailureCategory | Occurrences |")
    lines.append("|---|---|---:|")
    for item in repeated_signatures[:25]:
        lines.append(f"| {item['package']} | {item['canonical_class']} | {item['occurrences']} |")
    lines.append("")
    lines.append("### Patch activity not ending in PatchApplicationFailure")
    lines.append("")
    lines.append("| Package | FailureCategory | Artifact |")
    lines.append("|---|---|---|")
    for item in patch_drift[:25]:
        lines.append(f"| {item['package']} | {item['canonical_class']} | `{item['artifact_path']}` |")
    lines.append("")
    lines.append(f"- Long-tail share: **{long_tail_share:.2f}%**")
    lines.append(f"- Rule proliferation indicator (long-tail share > 40%): **{str(rule_proliferation_indicator).lower()}**")
    lines.append("")
    lines.append("## 10. StrategicRecommendations for Live Sampling Readiness")
    lines.append("")
    lines.append("1. Keep metadata-adapter runtime preflight as a hard campaign gate to prevent ingestion-stage dataset contamination.")
    lines.append("2. Prioritize deterministic normalization for `UnresolvedBuildRequiresToken`, `MissingHeaderOrIncludePath`, and `MissingLinkTimeDependency` before expanding live sampling breadth.")
    lines.append("3. Treat `PatchApplicationFailure` and `BuildScriptContractFailure` as lower-automation classes requiring targeted SourceNormalizer policy, not package-local heuristics.")
    lines.append("4. Use this mined distribution as a seed prior for stratified adaptive sampling weights; refresh after each normalization rule cycle.")
    lines.append("")
    lines.append("## 11. Artifact Paths")
    lines.append(f"- Records: `{records_tsv}`")
    lines.append(f"- Clusters: `{clusters_tsv}`")
    lines.append(f"- Candidates: `{candidates_tsv}`")
    lines.append(f"- Summary JSON: `{summary_json}`")
    report_md.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target-root",
        type=Path,
        default=Path("/Users/stephen/bioconda2rpm/targets/phoreus-bioconda2rpm-build-almalinux-9.7-aarch64"),
        help="Path to target build root containing BAD_SPEC and reports/",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=Path("docs/historical_log_mining"),
        help="Output directory for mined artifacts",
    )
    args = parser.parse_args()
    run(args.target_root, args.out_dir)


if __name__ == "__main__":
    main()
