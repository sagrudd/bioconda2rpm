use crate::cli::{
    BuildArgs, DependencyPolicy, GeneratePrioritySpecsArgs, MetadataAdapter,
    MissingDependencyPolicy,
};
use anyhow::{Context, Result};
use chrono::Utc;
use csv::{ReaderBuilder, Writer};
use minijinja::{Environment, context, value::Kwargs};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct PriorityTool {
    line_no: usize,
    software: String,
    priority: i64,
}

#[derive(Debug, Clone)]
struct RecipeDir {
    name: String,
    path: PathBuf,
    normalized: String,
}

#[derive(Debug, Clone)]
struct ResolvedRecipe {
    recipe_name: String,
    recipe_dir: PathBuf,
    variant_dir: PathBuf,
    meta_path: PathBuf,
    build_sh_path: Option<PathBuf>,
    overlap_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ParsedMeta {
    package_name: String,
    version: String,
    build_number: String,
    source_url: String,
    source_folder: String,
    homepage: String,
    license: String,
    summary: String,
    source_patches: Vec<String>,
    build_script: Option<String>,
    noarch_python: bool,
    build_dep_specs_raw: Vec<String>,
    host_dep_specs_raw: Vec<String>,
    run_dep_specs_raw: Vec<String>,
    build_deps: BTreeSet<String>,
    host_deps: BTreeSet<String>,
    run_deps: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct ParsedRecipeResult {
    parsed: ParsedMeta,
    build_skip: bool,
}

#[derive(Debug, Deserialize)]
struct CondaRenderMetadata {
    build_skip: bool,
    package_name: String,
    version: String,
    build_number: String,
    source_url: String,
    source_folder: String,
    homepage: String,
    license: String,
    summary: String,
    source_patches: Vec<String>,
    build_script: Option<String>,
    noarch_python: bool,
    build_dep_specs_raw: Vec<String>,
    host_dep_specs_raw: Vec<String>,
    run_dep_specs_raw: Vec<String>,
}

#[derive(Debug, Clone)]
struct ResolvedParsedRecipe {
    resolved: ResolvedRecipe,
    parsed: ParsedMeta,
    build_skip: bool,
}

#[derive(Debug, Clone)]
struct BuildConfig {
    topdir: PathBuf,
    reports_dir: PathBuf,
    container_engine: String,
    container_image: String,
    host_arch: String,
}

#[derive(Debug, Clone)]
struct PrecompiledBinaryOverride {
    source_url: String,
    build_script: String,
}

const PHOREUS_PYTHON_VERSION: &str = "3.11";
const PHOREUS_PYTHON_PACKAGE: &str = "phoreus-python-3.11";
const REFERENCE_PYTHON_SPECS_DIR: &str = "../software_query/rpm/python/specs";
const PHOREUS_R_VERSION: &str = "4.5.2";
const PHOREUS_R_MINOR: &str = "4.5";
const PHOREUS_R_PACKAGE: &str = "phoreus-r-4.5.2";
static PHOREUS_R_BOOTSTRAP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const PHOREUS_RUST_VERSION: &str = "1.92.0";
const PHOREUS_RUST_MINOR: &str = "1.92";
const PHOREUS_RUST_PACKAGE: &str = "phoreus-rust-1.92";
static PHOREUS_RUST_BOOTSTRAP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const PHOREUS_NIM_SERIES: &str = "2.2";
const PHOREUS_NIM_PACKAGE: &str = "phoreus-nim-2.2";
static PHOREUS_NIM_BOOTSTRAP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const CONDA_RENDER_ADAPTER_SCRIPT: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/conda_render_ir.py");

fn log_progress(message: impl AsRef<str>) {
    println!("progress {}", message.as_ref());
}

fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let mins = secs / 60;
    let rem_secs = secs % 60;
    if mins > 0 {
        format!("{mins}m{rem_secs:02}s")
    } else {
        format!("{rem_secs}s")
    }
}

fn compact_reason(reason: &str, limit: usize) -> String {
    let collapsed = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        collapsed
    } else {
        format!("{}...", collapsed.chars().take(limit).collect::<String>())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct DependencyResolutionEvent {
    dependency: String,
    status: String,
    source: String,
    provider: String,
    detail: String,
}

#[derive(Debug, Clone)]
struct DependencyGraphSummary {
    json_path: PathBuf,
    md_path: PathBuf,
    unresolved: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ReportEntry {
    pub software: String,
    pub priority: i64,
    pub status: String,
    pub reason: String,
    pub overlap_recipe: String,
    pub overlap_reason: String,
    pub variant_dir: String,
    pub package_name: String,
    pub version: String,
    pub payload_spec_path: String,
    pub meta_spec_path: String,
    pub staged_build_sh: String,
}

#[derive(Debug)]
pub struct GenerationSummary {
    pub requested: usize,
    pub generated: usize,
    pub quarantined: usize,
    pub report_json: PathBuf,
    pub report_csv: PathBuf,
    pub report_md: PathBuf,
}

#[derive(Debug)]
pub struct BuildSummary {
    pub requested: usize,
    pub generated: usize,
    pub up_to_date: usize,
    pub skipped: usize,
    pub quarantined: usize,
    pub build_order: Vec<String>,
    pub report_json: PathBuf,
    pub report_csv: PathBuf,
    pub report_md: PathBuf,
}

#[derive(Debug, Clone)]
struct BuildPlanNode {
    name: String,
    direct_bioconda_deps: BTreeSet<String>,
}

#[derive(Debug, Clone)]
enum PayloadVersionState {
    NotBuilt,
    UpToDate { existing_version: String },
    Outdated { existing_version: String },
}

#[derive(Debug, Deserialize)]
struct ToolsCsvRow {
    #[serde(rename = "Software")]
    software: String,
    #[serde(rename = "RPM Priority Score")]
    priority: String,
}

pub fn run_generate_priority_specs(args: &GeneratePrioritySpecsArgs) -> Result<GenerationSummary> {
    let topdir = args.effective_topdir();
    let specs_dir = topdir.join("SPECS");
    let sources_dir = topdir.join("SOURCES");
    let reports_dir = args.effective_reports_dir();
    let bad_spec_dir = args.effective_bad_spec_dir();

    fs::create_dir_all(&specs_dir)
        .with_context(|| format!("creating specs dir {}", specs_dir.display()))?;
    fs::create_dir_all(&sources_dir)
        .with_context(|| format!("creating sources dir {}", sources_dir.display()))?;
    fs::create_dir_all(&reports_dir)
        .with_context(|| format!("creating reports dir {}", reports_dir.display()))?;
    fs::create_dir_all(&bad_spec_dir)
        .with_context(|| format!("creating bad spec dir {}", bad_spec_dir.display()))?;
    ensure_container_engine_available(&args.container_engine)?;
    sync_reference_python_specs(&specs_dir).context("syncing reference Phoreus Python specs")?;

    let mut tools = load_top_tools(&args.tools_csv, args.top_n)?;
    tools.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.line_no.cmp(&b.line_no)));

    let recipe_dirs = discover_recipe_dirs(&args.recipe_root)?;
    let build_config = BuildConfig {
        topdir: topdir.clone(),
        reports_dir: reports_dir.clone(),
        container_engine: args.container_engine.clone(),
        container_image: args.container_image.clone(),
        host_arch: std::env::consts::ARCH.to_string(),
    };
    ensure_phoreus_python_bootstrap(&build_config, &specs_dir)
        .context("bootstrapping Phoreus Python runtime")?;

    let indexed_tools: Vec<(usize, PriorityTool)> = tools.into_iter().enumerate().collect();
    let worker_count = args.workers.filter(|w| *w > 0);

    let runner = || {
        indexed_tools
            .par_iter()
            .map(|(idx, tool)| {
                let entry = process_tool(
                    tool,
                    &args.recipe_root,
                    &recipe_dirs,
                    &specs_dir,
                    &sources_dir,
                    &bad_spec_dir,
                    &build_config,
                    &args.metadata_adapter,
                );
                (*idx, entry)
            })
            .collect::<Vec<_>>()
    };

    let mut indexed_results = if let Some(workers) = worker_count {
        rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .context("creating rayon worker pool")?
            .install(runner)
    } else {
        runner()
    };

    indexed_results.sort_by_key(|(idx, _)| *idx);
    let results: Vec<ReportEntry> = indexed_results.into_iter().map(|(_, r)| r).collect();

    let report_json = reports_dir.join("priority_spec_generation.json");
    let report_csv = reports_dir.join("priority_spec_generation.csv");
    let report_md = reports_dir.join("priority_spec_generation.md");

    write_reports(&results, &report_json, &report_csv, &report_md)?;

    let generated = results.iter().filter(|r| r.status == "generated").count();
    let quarantined = results.len().saturating_sub(generated);

    Ok(GenerationSummary {
        requested: results.len(),
        generated,
        quarantined,
        report_json,
        report_csv,
        report_md,
    })
}

pub fn run_build(args: &BuildArgs) -> Result<BuildSummary> {
    let build_started = Instant::now();
    let topdir = args.effective_topdir();
    let specs_dir = topdir.join("SPECS");
    let sources_dir = topdir.join("SOURCES");
    let reports_dir = args.effective_reports_dir();
    let bad_spec_dir = args.effective_bad_spec_dir();
    log_progress(format!(
        "phase=build-start package={} deps_enabled={} dependency_policy={:?} recipe_root={} topdir={}",
        args.package,
        args.with_deps(),
        args.dependency_policy,
        args.recipe_root.display(),
        topdir.display()
    ));

    fs::create_dir_all(&specs_dir)
        .with_context(|| format!("creating specs dir {}", specs_dir.display()))?;
    fs::create_dir_all(&sources_dir)
        .with_context(|| format!("creating sources dir {}", sources_dir.display()))?;
    fs::create_dir_all(&reports_dir)
        .with_context(|| format!("creating reports dir {}", reports_dir.display()))?;
    fs::create_dir_all(&bad_spec_dir)
        .with_context(|| format!("creating bad spec dir {}", bad_spec_dir.display()))?;

    ensure_container_engine_available(&args.container_engine)?;
    sync_reference_python_specs(&specs_dir).context("syncing reference Phoreus Python specs")?;
    let recipe_dirs = discover_recipe_dirs(&args.recipe_root)?;
    log_progress(format!(
        "phase=recipe-discovery status=completed recipe_count={} elapsed={}",
        recipe_dirs.len(),
        format_elapsed(build_started.elapsed())
    ));

    let build_config = BuildConfig {
        topdir: topdir.clone(),
        reports_dir: reports_dir.clone(),
        container_engine: args.container_engine.clone(),
        container_image: args.container_image.clone(),
        host_arch: std::env::consts::ARCH.to_string(),
    };
    ensure_phoreus_python_bootstrap(&build_config, &specs_dir)
        .context("bootstrapping Phoreus Python runtime")?;

    let Some(root_recipe) = resolve_and_parse_recipe(
        &args.package,
        &args.recipe_root,
        &recipe_dirs,
        true,
        &args.metadata_adapter,
    )?
    else {
        anyhow::bail!(
            "no overlapping recipe found in bioconda metadata for '{}'",
            args.package
        );
    };
    if root_recipe.build_skip {
        let root_slug = normalize_name(&root_recipe.resolved.recipe_name);
        clear_quarantine_note(&bad_spec_dir, &root_slug);
        let reason = "recipe declares build.skip=true for this render context".to_string();
        let entry = ReportEntry {
            software: root_recipe.resolved.recipe_name.clone(),
            priority: 0,
            status: "skipped".to_string(),
            reason: reason.clone(),
            overlap_recipe: root_recipe.resolved.recipe_name.clone(),
            overlap_reason: "requested-root".to_string(),
            variant_dir: root_recipe.resolved.variant_dir.display().to_string(),
            package_name: root_recipe.parsed.package_name.clone(),
            version: root_recipe.parsed.version.clone(),
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: String::new(),
        };
        let report_stem = normalize_name(&args.package);
        let report_json = reports_dir.join(format!("build_{report_stem}.json"));
        let report_csv = reports_dir.join(format!("build_{report_stem}.csv"));
        let report_md = reports_dir.join(format!("build_{report_stem}.md"));
        write_reports(&[entry], &report_json, &report_csv, &report_md)?;
        return Ok(BuildSummary {
            requested: 1,
            generated: 0,
            up_to_date: 0,
            skipped: 1,
            quarantined: 0,
            build_order: vec![root_recipe.resolved.recipe_name.clone()],
            report_json,
            report_csv,
            report_md,
        });
    }

    let root_slug = normalize_name(&root_recipe.resolved.recipe_name);
    if let PayloadVersionState::UpToDate { existing_version } =
        payload_version_state(&topdir, &root_slug, &root_recipe.parsed.version)?
    {
        log_progress(format!(
            "phase=build status=up-to-date package={} version={} local_version={} elapsed={}",
            root_recipe.resolved.recipe_name,
            root_recipe.parsed.version,
            existing_version,
            format_elapsed(build_started.elapsed())
        ));
        clear_quarantine_note(&bad_spec_dir, &root_slug);
        let reason = format!(
            "already up-to-date: bioconda version {} already built (latest local payload version {})",
            root_recipe.parsed.version, existing_version
        );
        let entry = ReportEntry {
            software: root_recipe.resolved.recipe_name.clone(),
            priority: 0,
            status: "up-to-date".to_string(),
            reason,
            overlap_recipe: root_recipe.resolved.recipe_name.clone(),
            overlap_reason: "requested-root".to_string(),
            variant_dir: root_recipe.resolved.variant_dir.display().to_string(),
            package_name: root_recipe.parsed.package_name.clone(),
            version: root_recipe.parsed.version.clone(),
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: String::new(),
        };

        let report_stem = normalize_name(&args.package);
        let report_json = reports_dir.join(format!("build_{report_stem}.json"));
        let report_csv = reports_dir.join(format!("build_{report_stem}.csv"));
        let report_md = reports_dir.join(format!("build_{report_stem}.md"));
        write_reports(&[entry], &report_json, &report_csv, &report_md)?;

        return Ok(BuildSummary {
            requested: 1,
            generated: 0,
            up_to_date: 1,
            skipped: 0,
            quarantined: 0,
            build_order: vec![root_recipe.resolved.recipe_name],
            report_json,
            report_csv,
            report_md,
        });
    }

    let (plan_order, plan_nodes) = collect_build_plan(
        &args.package,
        args.with_deps(),
        &args.dependency_policy,
        &args.recipe_root,
        &recipe_dirs,
        &args.metadata_adapter,
    )?;
    let build_order = plan_order
        .iter()
        .filter_map(|key| plan_nodes.get(key).map(|node| node.name.clone()))
        .collect::<Vec<_>>();
    log_progress(format!(
        "phase=dependency-plan status=completed package={} planned_nodes={} order={}",
        args.package,
        build_order.len(),
        build_order.join("->")
    ));

    let mut built = HashSet::new();
    let mut results = Vec::new();
    let mut fail_reason: Option<String> = None;

    for (idx, key) in plan_order.iter().enumerate() {
        let Some(node) = plan_nodes.get(key) else {
            continue;
        };
        let package_started = Instant::now();
        log_progress(format!(
            "phase=package status=started index={}/{} package={}",
            idx + 1,
            plan_order.len(),
            node.name
        ));

        let blocked_by = node
            .direct_bioconda_deps
            .iter()
            .filter(|dep| !built.contains(*dep))
            .cloned()
            .collect::<Vec<_>>();

        if !blocked_by.is_empty() {
            let reason = format!(
                "blocked by unresolved bioconda dependencies: {}",
                blocked_by.join(", ")
            );
            let status = match args.missing_dependency {
                MissingDependencyPolicy::Skip => "skipped",
                _ => "quarantined",
            }
            .to_string();

            if status == "quarantined" {
                quarantine_note(&bad_spec_dir, key, &reason);
            }
            log_progress(format!(
                "phase=package status={} package={} blocked_by={} reason={}",
                status,
                node.name,
                blocked_by.join(","),
                compact_reason(&reason, 240)
            ));
            results.push(ReportEntry {
                software: node.name.clone(),
                priority: 0,
                status,
                reason: reason.clone(),
                overlap_recipe: node.name.clone(),
                overlap_reason: "dependency-closure".to_string(),
                variant_dir: String::new(),
                package_name: String::new(),
                version: String::new(),
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            });
            if args.missing_dependency == MissingDependencyPolicy::Fail {
                fail_reason = Some(reason);
                break;
            }
            continue;
        }

        let tool = PriorityTool {
            line_no: 0,
            software: node.name.clone(),
            priority: 0,
        };
        let entry = process_tool(
            &tool,
            &args.recipe_root,
            &recipe_dirs,
            &specs_dir,
            &sources_dir,
            &bad_spec_dir,
            &build_config,
            &args.metadata_adapter,
        );
        log_progress(format!(
            "phase=package status={} package={} elapsed={} reason={}",
            entry.status,
            node.name,
            format_elapsed(package_started.elapsed()),
            compact_reason(&entry.reason, 240)
        ));
        if entry.status == "generated" || entry.status == "up-to-date" {
            built.insert(key.clone());
        } else if args.missing_dependency == MissingDependencyPolicy::Fail {
            fail_reason = Some(entry.reason.clone());
            results.push(entry);
            break;
        }
        results.push(entry);
    }

    let report_stem = normalize_name(&args.package);
    let report_json = reports_dir.join(format!("build_{report_stem}.json"));
    let report_csv = reports_dir.join(format!("build_{report_stem}.csv"));
    let report_md = reports_dir.join(format!("build_{report_stem}.md"));
    write_reports(&results, &report_json, &report_csv, &report_md)?;
    log_progress(format!(
        "phase=report status=written report_json={} report_csv={} report_md={}",
        report_json.display(),
        report_csv.display(),
        report_md.display()
    ));

    if let Some(reason) = fail_reason {
        log_progress(format!(
            "phase=build status=failed policy={:?} reason={} elapsed={}",
            args.missing_dependency,
            compact_reason(&reason, 320),
            format_elapsed(build_started.elapsed())
        ));
        anyhow::bail!(
            "build failed under missing-dependency policy fail: {} (report_md={})",
            reason,
            report_md.display()
        );
    }

    let generated = results.iter().filter(|r| r.status == "generated").count();
    let up_to_date = results.iter().filter(|r| r.status == "up-to-date").count();
    let skipped = results.iter().filter(|r| r.status == "skipped").count();
    let quarantined = results.iter().filter(|r| r.status == "quarantined").count();
    log_progress(format!(
        "phase=build status=completed requested={} generated={} up_to_date={} skipped={} quarantined={} elapsed={}",
        results.len(),
        generated,
        up_to_date,
        skipped,
        quarantined,
        format_elapsed(build_started.elapsed())
    ));
    Ok(BuildSummary {
        requested: results.len(),
        generated,
        up_to_date,
        skipped,
        quarantined,
        build_order,
        report_json,
        report_csv,
        report_md,
    })
}

fn collect_build_plan(
    root: &str,
    with_deps: bool,
    policy: &DependencyPolicy,
    recipe_root: &Path,
    recipe_dirs: &[RecipeDir],
    metadata_adapter: &MetadataAdapter,
) -> Result<(Vec<String>, BTreeMap<String, BuildPlanNode>)> {
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut order = Vec::new();
    let mut nodes = BTreeMap::new();

    let root_key = visit_build_plan_node(
        root,
        true,
        with_deps,
        policy,
        recipe_root,
        recipe_dirs,
        metadata_adapter,
        &mut visiting,
        &mut visited,
        &mut nodes,
        &mut order,
    )?;
    if root_key.is_none() {
        anyhow::bail!(
            "no overlapping recipe found in bioconda metadata for '{}'",
            root
        );
    }

    Ok((order, nodes))
}

#[allow(clippy::too_many_arguments)]
fn visit_build_plan_node(
    query: &str,
    is_root: bool,
    with_deps: bool,
    policy: &DependencyPolicy,
    recipe_root: &Path,
    recipe_dirs: &[RecipeDir],
    metadata_adapter: &MetadataAdapter,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
    nodes: &mut BTreeMap<String, BuildPlanNode>,
    order: &mut Vec<String>,
) -> Result<Option<String>> {
    let resolved_and_parsed = match resolve_and_parse_recipe(
        query,
        recipe_root,
        recipe_dirs,
        is_root,
        metadata_adapter,
    ) {
        Ok(v) => v,
        Err(err) => {
            if is_root {
                return Err(err);
            }
            return Ok(None);
        }
    };

    let Some(resolved_parsed) = resolved_and_parsed else {
        if is_root {
            anyhow::bail!(
                "no overlapping recipe found in bioconda metadata for '{}'",
                query
            );
        }
        return Ok(None);
    };
    let resolved = &resolved_parsed.resolved;
    let parsed = &resolved_parsed.parsed;
    if resolved_parsed.build_skip && !is_root {
        log_progress(format!(
            "phase=dependency action=skip package={} reason=build.skip=true",
            resolved.recipe_name
        ));
        return Ok(None);
    }

    let canonical = normalize_name(&resolved.recipe_name);
    if !is_root && !is_buildable_recipe(&resolved, &parsed) {
        log_progress(format!(
            "phase=dependency action=skip package={} reason=not-buildable(build.sh/meta-script/source-url missing)",
            resolved.recipe_name
        ));
        return Ok(None);
    }
    if visited.contains(&canonical) {
        return Ok(Some(canonical));
    }
    if visiting.contains(&canonical) {
        return Ok(Some(canonical));
    }

    visiting.insert(canonical.clone());
    let mut bioconda_deps = BTreeSet::new();

    if with_deps {
        let selected = selected_dependency_set(&parsed, policy, is_root);
        if !selected.is_empty() {
            log_progress(format!(
                "phase=dependency action=scan package={} selected_count={} policy={:?} is_root={}",
                resolved.recipe_name,
                selected.len(),
                policy,
                is_root
            ));
        }
        for dep in selected {
            if dep == canonical {
                log_progress(format!(
                    "phase=dependency action=skip from={} to={} reason=self-reference",
                    canonical, dep
                ));
                continue;
            }
            if map_perl_core_dependency(&dep).is_some() {
                log_progress(format!(
                    "phase=dependency action=skip from={} to={} reason=perl-core-system-provided",
                    canonical, dep
                ));
                continue;
            }
            if is_r_ecosystem_dependency_name(&dep) && is_r_base_dependency_name(&dep) {
                log_progress(format!(
                    "phase=dependency action=skip from={} to={} reason=r-runtime-provided",
                    canonical, dep
                ));
                continue;
            }
            if is_phoreus_python_toolchain_dependency(&dep) {
                log_progress(format!(
                    "phase=dependency action=skip from={} to={} reason=python-runtime-provided",
                    canonical, dep
                ));
                continue;
            }
            if is_conda_only_dependency(&dep) {
                log_progress(format!(
                    "phase=dependency action=skip from={} to={} reason=conda-helper-not-rpm",
                    canonical, dep
                ));
                continue;
            }
            if normalize_dependency_token(&dep) == "k8" {
                log_progress(format!(
                    "phase=dependency action=skip from={} to={} reason=core-runtime-alias-nodejs",
                    canonical, dep
                ));
                continue;
            }
            if is_rust_ecosystem_dependency_name(&dep) {
                log_progress(format!(
                    "phase=dependency action=skip from={} to={} reason=rust-runtime-provided",
                    canonical, dep
                ));
                continue;
            }
            if is_nim_ecosystem_dependency_name(&dep) {
                log_progress(format!(
                    "phase=dependency action=skip from={} to={} reason=nim-runtime-provided",
                    canonical, dep
                ));
                continue;
            }
            log_progress(format!(
                "phase=dependency action=follow from={} to={}",
                canonical, dep
            ));
            if let Some(dep_key) = visit_build_plan_node(
                &dep,
                false,
                with_deps,
                policy,
                recipe_root,
                recipe_dirs,
                metadata_adapter,
                visiting,
                visited,
                nodes,
                order,
            )? {
                if dep_key == canonical {
                    log_progress(format!(
                        "phase=dependency action=skip from={} to={} reason=alias-self-resolution",
                        canonical, dep
                    ));
                    continue;
                }
                bioconda_deps.insert(dep_key);
            } else {
                log_progress(format!(
                    "phase=dependency action=unresolved from={} to={}",
                    canonical, dep
                ));
            }
        }
    }

    visiting.remove(&canonical);
    visited.insert(canonical.clone());
    nodes.insert(
        canonical.clone(),
        BuildPlanNode {
            name: resolved.recipe_name.clone(),
            direct_bioconda_deps: bioconda_deps,
        },
    );
    order.push(canonical.clone());
    Ok(Some(canonical))
}

fn is_buildable_recipe(resolved: &ResolvedRecipe, parsed: &ParsedMeta) -> bool {
    (resolved.build_sh_path.is_some()
        || parsed.build_script.is_some()
        || synthesize_fallback_build_sh(parsed).is_some())
        && !parsed.source_url.trim().is_empty()
}

fn selected_dependency_set(
    parsed: &ParsedMeta,
    policy: &DependencyPolicy,
    is_root: bool,
) -> BTreeSet<String> {
    // Precompiled-binary policy packages should not pull source-build closure.
    // Their runtime requirements are sufficient for dependency planning.
    let package_slug = normalize_name(&parsed.package_name);
    if precompiled_binary_override(&package_slug, parsed).is_some() {
        return parsed
            .run_deps
            .iter()
            .filter(|dep| !is_conda_only_dependency(dep))
            .cloned()
            .collect();
    }

    if is_python_recipe(parsed) {
        let mut out = BTreeSet::new();
        out.extend(
            parsed
                .build_deps
                .iter()
                .filter(|dep| !is_conda_only_dependency(dep))
                .filter(|dep| should_keep_rpm_dependency_for_python(dep))
                .cloned(),
        );
        out.extend(
            parsed
                .host_deps
                .iter()
                .filter(|dep| !is_conda_only_dependency(dep))
                .filter(|dep| should_keep_rpm_dependency_for_python(dep))
                .cloned(),
        );
        out.extend(
            parsed
                .run_deps
                .iter()
                .filter(|dep| !is_conda_only_dependency(dep))
                .filter(|dep| should_keep_rpm_dependency_for_python(dep))
                .cloned(),
        );
        return out;
    }

    match policy {
        DependencyPolicy::RunOnly => parsed
            .run_deps
            .iter()
            .filter(|dep| !is_conda_only_dependency(dep))
            .cloned()
            .collect(),
        DependencyPolicy::BuildHostRun => {
            let mut out = BTreeSet::new();
            out.extend(
                parsed
                    .build_deps
                    .iter()
                    .filter(|dep| !is_conda_only_dependency(dep))
                    .cloned(),
            );
            out.extend(
                parsed
                    .host_deps
                    .iter()
                    .filter(|dep| !is_conda_only_dependency(dep))
                    .cloned(),
            );
            out.extend(
                parsed
                    .run_deps
                    .iter()
                    .filter(|dep| !is_conda_only_dependency(dep))
                    .cloned(),
            );
            out
        }
        DependencyPolicy::RuntimeTransitiveRootBuildHost => {
            if is_root {
                let mut out = BTreeSet::new();
                out.extend(
                    parsed
                        .build_deps
                        .iter()
                        .filter(|dep| !is_conda_only_dependency(dep))
                        .cloned(),
                );
                out.extend(
                    parsed
                        .host_deps
                        .iter()
                        .filter(|dep| !is_conda_only_dependency(dep))
                        .cloned(),
                );
                out.extend(
                    parsed
                        .run_deps
                        .iter()
                        .filter(|dep| !is_conda_only_dependency(dep))
                        .cloned(),
                );
                out
            } else {
                parsed
                    .run_deps
                    .iter()
                    .filter(|dep| !is_conda_only_dependency(dep))
                    .cloned()
                    .collect()
            }
        }
    }
}

fn resolve_and_parse_recipe(
    tool_name: &str,
    recipe_root: &Path,
    recipe_dirs: &[RecipeDir],
    allow_identifier_lookup: bool,
    metadata_adapter: &MetadataAdapter,
) -> Result<Option<ResolvedParsedRecipe>> {
    let Some(resolved) =
        resolve_recipe_for_tool_mode(tool_name, recipe_root, recipe_dirs, allow_identifier_lookup)?
    else {
        return Ok(None);
    };
    let parsed_result =
        parse_meta_for_resolved(&resolved, metadata_adapter).with_context(|| {
            format!(
                "failed to parse rendered metadata for {}",
                resolved.meta_path.display()
            )
        })?;
    Ok(Some(ResolvedParsedRecipe {
        resolved,
        parsed: parsed_result.parsed,
        build_skip: parsed_result.build_skip,
    }))
}

fn parse_meta_for_resolved(
    resolved: &ResolvedRecipe,
    metadata_adapter: &MetadataAdapter,
) -> Result<ParsedRecipeResult> {
    match metadata_adapter {
        MetadataAdapter::Native => parse_meta_for_resolved_native(resolved),
        MetadataAdapter::Conda => parse_meta_for_resolved_conda(resolved),
        MetadataAdapter::Auto => match parse_meta_for_resolved_conda(resolved) {
            Ok(parsed) => Ok(parsed),
            Err(err) => {
                log_progress(format!(
                    "phase=metadata-adapter status=fallback recipe={} from=conda to=native reason={}",
                    resolved.recipe_name,
                    compact_reason(&err.to_string(), 240)
                ));
                parse_meta_for_resolved_native(resolved)
            }
        },
    }
}

fn parse_meta_for_resolved_native(resolved: &ResolvedRecipe) -> Result<ParsedRecipeResult> {
    let meta_text = fs::read_to_string(&resolved.meta_path)
        .with_context(|| format!("failed to read metadata {}", resolved.meta_path.display()))?;
    let selector_ctx = SelectorContext::for_rpm_build();
    let selected_meta = apply_selectors(&meta_text, &selector_ctx);
    let rendered = render_meta_yaml(&selected_meta).with_context(|| {
        format!(
            "failed to render Jinja for {}",
            resolved.meta_path.display()
        )
    })?;
    let build_skip = rendered_meta_declares_build_skip(&rendered);
    let parsed = parse_rendered_meta(&rendered).with_context(|| {
        format!(
            "failed to parse rendered metadata for {}",
            resolved.meta_path.display()
        )
    })?;
    Ok(ParsedRecipeResult { parsed, build_skip })
}

fn parse_meta_for_resolved_conda(resolved: &ResolvedRecipe) -> Result<ParsedRecipeResult> {
    let output = Command::new("python3")
        .arg(CONDA_RENDER_ADAPTER_SCRIPT)
        .arg(&resolved.variant_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "running conda render adapter for {}",
                resolved.variant_dir.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!(
            "conda render adapter failed (status: {}) stdout={} stderr={}",
            output.status,
            compact_reason(&stdout, 200),
            compact_reason(&stderr, 400)
        );
    }

    let adapter: CondaRenderMetadata =
        serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "parsing conda render adapter JSON for {}",
                resolved.variant_dir.display()
            )
        })?;

    let build_dep_specs_raw = adapter.build_dep_specs_raw;
    let host_dep_specs_raw = adapter.host_dep_specs_raw;
    let run_dep_specs_raw = adapter.run_dep_specs_raw;

    let parsed = ParsedMeta {
        package_name: adapter.package_name,
        version: adapter.version,
        build_number: adapter.build_number,
        source_url: adapter.source_url,
        source_folder: adapter.source_folder,
        homepage: adapter.homepage,
        license: adapter.license,
        summary: adapter.summary,
        source_patches: adapter.source_patches,
        build_script: adapter.build_script,
        noarch_python: adapter.noarch_python,
        build_dep_specs_raw: build_dep_specs_raw.clone(),
        host_dep_specs_raw: host_dep_specs_raw.clone(),
        run_dep_specs_raw: run_dep_specs_raw.clone(),
        build_deps: normalize_dep_specs_to_set(&build_dep_specs_raw),
        host_deps: normalize_dep_specs_to_set(&host_dep_specs_raw),
        run_deps: normalize_dep_specs_to_set(&run_dep_specs_raw),
    };

    Ok(ParsedRecipeResult {
        parsed,
        build_skip: adapter.build_skip,
    })
}

fn normalize_dep_specs_to_set(raw_specs: &[String]) -> BTreeSet<String> {
    raw_specs
        .iter()
        .filter_map(|raw| normalize_dependency_name(raw))
        .collect()
}

fn load_top_tools(tools_csv: &Path, top_n: usize) -> Result<Vec<PriorityTool>> {
    let mut reader = ReaderBuilder::new()
        .has_headers(true)
        .from_path(tools_csv)
        .with_context(|| format!("opening tools csv {}", tools_csv.display()))?;

    let mut rows: Vec<PriorityTool> = Vec::new();
    for (line_no, row) in reader.deserialize::<ToolsCsvRow>().enumerate() {
        let line = line_no + 2;
        let row = row.with_context(|| format!("parsing tools csv line {line}"))?;
        let software = row.software.trim();
        if software.is_empty() {
            continue;
        }
        let priority = match row.priority.trim().parse::<i64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        rows.push(PriorityTool {
            line_no: line,
            software: software.to_string(),
            priority,
        });
    }

    rows.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.line_no.cmp(&b.line_no)));
    rows.truncate(top_n);
    Ok(rows)
}

fn discover_recipe_dirs(recipe_root: &Path) -> Result<Vec<RecipeDir>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(recipe_root)
        .with_context(|| format!("reading recipe root {}", recipe_root.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", recipe_root.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        dirs.push(RecipeDir {
            normalized: normalize_name(&name),
            name,
            path,
        });
    }
    Ok(dirs)
}

fn process_tool(
    tool: &PriorityTool,
    recipe_root: &Path,
    recipe_dirs: &[RecipeDir],
    specs_dir: &Path,
    sources_dir: &Path,
    bad_spec_dir: &Path,
    build_config: &BuildConfig,
    metadata_adapter: &MetadataAdapter,
) -> ReportEntry {
    let software_slug = normalize_name(&tool.software);

    let resolved = match resolve_recipe_for_tool(&tool.software, recipe_root, recipe_dirs) {
        Ok(Some(v)) => v,
        Ok(None) => {
            let reason = "no overlapping recipe found in bioconda metadata".to_string();
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: String::new(),
                overlap_reason: String::new(),
                variant_dir: String::new(),
                package_name: String::new(),
                version: String::new(),
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            };
        }
        Err(err) => {
            let reason = format!("recipe resolution failed: {err}");
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: String::new(),
                overlap_reason: String::new(),
                variant_dir: String::new(),
                package_name: String::new(),
                version: String::new(),
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            };
        }
    };

    let parsed_result = match parse_meta_for_resolved(&resolved, metadata_adapter) {
        Ok(v) => v,
        Err(err) => {
            let reason = format!("failed to parse rendered metadata: {err}");
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: String::new(),
                version: String::new(),
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            };
        }
    };
    if parsed_result.build_skip {
        clear_quarantine_note(bad_spec_dir, &software_slug);
        return ReportEntry {
            software: tool.software.clone(),
            priority: tool.priority,
            status: "skipped".to_string(),
            reason: "recipe declares build.skip=true for this render context".to_string(),
            overlap_recipe: resolved.recipe_name,
            overlap_reason: resolved.overlap_reason,
            variant_dir: resolved.variant_dir.display().to_string(),
            package_name: parsed_result.parsed.package_name,
            version: parsed_result.parsed.version,
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: String::new(),
        };
    }
    let mut parsed = parsed_result.parsed;

    let version_state =
        match payload_version_state(&build_config.topdir, &software_slug, &parsed.version) {
            Ok(v) => v,
            Err(err) => {
                let reason = format!("failed to evaluate local artifact versions: {err}");
                quarantine_note(bad_spec_dir, &software_slug, &reason);
                return ReportEntry {
                    software: tool.software.clone(),
                    priority: tool.priority,
                    status: "quarantined".to_string(),
                    reason,
                    overlap_recipe: resolved.recipe_name,
                    overlap_reason: resolved.overlap_reason,
                    variant_dir: resolved.variant_dir.display().to_string(),
                    package_name: parsed.package_name,
                    version: parsed.version,
                    payload_spec_path: String::new(),
                    meta_spec_path: String::new(),
                    staged_build_sh: String::new(),
                };
            }
        };
    if let PayloadVersionState::UpToDate { existing_version } = &version_state {
        clear_quarantine_note(bad_spec_dir, &software_slug);
        return ReportEntry {
            software: tool.software.clone(),
            priority: tool.priority,
            status: "up-to-date".to_string(),
            reason: format!(
                "already up-to-date: bioconda version {} already built (latest local payload version {})",
                parsed.version, existing_version
            ),
            overlap_recipe: resolved.recipe_name,
            overlap_reason: resolved.overlap_reason,
            variant_dir: resolved.variant_dir.display().to_string(),
            package_name: parsed.package_name,
            version: parsed.version,
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: String::new(),
        };
    }

    let staged_build_sh_name = format!("bioconda-{}-build.sh", software_slug);
    let staged_build_sh = sources_dir.join(&staged_build_sh_name);
    let precompiled_override = precompiled_binary_override(&software_slug, &parsed);

    if let Some(override_cfg) = precompiled_override.as_ref() {
        log_progress(format!(
            "phase=precompiled-binary status=selected package={} source_url={}",
            software_slug, override_cfg.source_url
        ));
        parsed.source_url = override_cfg.source_url.clone();
        if let Err(err) = fs::write(&staged_build_sh, &override_cfg.build_script) {
            let reason = format!(
                "failed to write precompiled build script {}: {err}",
                staged_build_sh.display()
            );
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            };
        }
    } else if let Some(build_sh_path) = resolved.build_sh_path.as_ref() {
        if let Err(err) = fs::copy(build_sh_path, &staged_build_sh) {
            let reason = format!(
                "failed to stage build.sh {}: {err}",
                build_sh_path.display()
            );
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            };
        }
    } else if let Some(script) = parsed.build_script.as_deref() {
        let generated = synthesize_build_sh_from_meta_script(script);
        if let Err(err) = fs::write(&staged_build_sh, generated) {
            let reason = format!(
                "failed to synthesize build.sh from meta.yaml build.script for {}: {err}",
                resolved.meta_path.display()
            );
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            };
        }
    } else if let Some(generated) = synthesize_fallback_build_sh(&parsed) {
        if let Err(err) = fs::write(&staged_build_sh, generated) {
            let reason = format!(
                "failed to synthesize default build.sh for {}: {err}",
                resolved.meta_path.display()
            );
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            };
        }
    } else {
        let reason =
            "recipe does not provide build.sh and has no supported build.script in meta.yaml"
                .to_string();
        quarantine_note(bad_spec_dir, &software_slug, &reason);
        return ReportEntry {
            software: tool.software.clone(),
            priority: tool.priority,
            status: "quarantined".to_string(),
            reason,
            overlap_recipe: resolved.recipe_name,
            overlap_reason: resolved.overlap_reason,
            variant_dir: resolved.variant_dir.display().to_string(),
            package_name: parsed.package_name,
            version: parsed.version,
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: String::new(),
        };
    }
    if let Err(err) = harden_staged_build_script(&staged_build_sh) {
        let reason = format!(
            "failed to apply staged build.sh hardening {}: {err}",
            staged_build_sh.display()
        );
        quarantine_note(bad_spec_dir, &software_slug, &reason);
        return ReportEntry {
            software: tool.software.clone(),
            priority: tool.priority,
            status: "quarantined".to_string(),
            reason,
            overlap_recipe: resolved.recipe_name,
            overlap_reason: resolved.overlap_reason,
            variant_dir: resolved.variant_dir.display().to_string(),
            package_name: parsed.package_name,
            version: parsed.version,
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: staged_build_sh.display().to_string(),
        };
    }
    #[cfg(unix)]
    if let Err(err) = fs::set_permissions(&staged_build_sh, fs::Permissions::from_mode(0o755)) {
        let reason = format!(
            "failed to set staged build.sh permissions {}: {err}",
            staged_build_sh.display()
        );
        quarantine_note(bad_spec_dir, &software_slug, &reason);
        return ReportEntry {
            software: tool.software.clone(),
            priority: tool.priority,
            status: "quarantined".to_string(),
            reason,
            overlap_recipe: resolved.recipe_name,
            overlap_reason: resolved.overlap_reason,
            variant_dir: resolved.variant_dir.display().to_string(),
            package_name: parsed.package_name,
            version: parsed.version,
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: staged_build_sh.display().to_string(),
        };
    }
    let python_script_hint = match staged_build_script_indicates_python(&staged_build_sh) {
        Ok(v) => v,
        Err(err) => {
            let reason = format!(
                "failed to inspect staged build.sh {} for python policy: {err}",
                staged_build_sh.display()
            );
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
    };
    let r_script_hint = match staged_build_script_indicates_r(&staged_build_sh) {
        Ok(v) => v,
        Err(err) => {
            let reason = format!(
                "failed to inspect staged build.sh {} for R policy: {err}",
                staged_build_sh.display()
            );
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
    };
    let rust_script_hint = match staged_build_script_indicates_rust(&staged_build_sh) {
        Ok(v) => v,
        Err(err) => {
            let reason = format!(
                "failed to inspect staged build.sh {} for Rust policy: {err}",
                staged_build_sh.display()
            );
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
    };
    if recipe_requires_r_runtime(&parsed) || is_r_project_recipe(&parsed) || r_script_hint {
        if let Err(err) = ensure_phoreus_r_bootstrap(build_config, specs_dir) {
            let reason = format!("bootstrapping Phoreus R runtime failed: {err}");
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
    }
    if recipe_requires_rust_runtime(&parsed) || rust_script_hint {
        if let Err(err) = ensure_phoreus_rust_bootstrap(build_config, specs_dir) {
            let reason = format!("bootstrapping Phoreus Rust runtime failed: {err}");
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
    }
    if recipe_requires_nim_runtime(&parsed) {
        if let Err(err) = ensure_phoreus_nim_bootstrap(build_config, specs_dir) {
            let reason = format!("bootstrapping Phoreus Nim runtime failed: {err}");
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
    }

    let staged_patch_sources = match stage_recipe_patches(
        &parsed.source_patches,
        &resolved,
        sources_dir,
        &software_slug,
    ) {
        Ok(v) => v,
        Err(err) => {
            let reason = format!("failed to stage recipe patches: {err}");
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
    };
    if let Err(err) = stage_recipe_support_files(&resolved, sources_dir) {
        let reason = format!("failed to stage recipe support files: {err}");
        quarantine_note(bad_spec_dir, &software_slug, &reason);
        return ReportEntry {
            software: tool.software.clone(),
            priority: tool.priority,
            status: "quarantined".to_string(),
            reason,
            overlap_recipe: resolved.recipe_name,
            overlap_reason: resolved.overlap_reason,
            variant_dir: resolved.variant_dir.display().to_string(),
            package_name: parsed.package_name,
            version: parsed.version,
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: staged_build_sh.display().to_string(),
        };
    }

    let payload_spec_path = specs_dir.join(format!("phoreus-{}.spec", software_slug));
    let meta_spec_path = specs_dir.join(format!("phoreus-{}-default.spec", software_slug));

    let payload_spec = render_payload_spec(
        &software_slug,
        &parsed,
        &staged_build_sh_name,
        &staged_patch_sources,
        &resolved.meta_path,
        &resolved.variant_dir,
        parsed.noarch_python,
        python_script_hint,
        r_script_hint,
        rust_script_hint,
    );
    let meta_version = match next_meta_package_version(&build_config.topdir, &software_slug) {
        Ok(v) => v,
        Err(err) => {
            let reason = format!("failed to determine next meta package version: {err}");
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
    };
    let default_spec = render_default_spec(&software_slug, &parsed, meta_version);

    let write_payload = fs::write(&payload_spec_path, payload_spec);
    let write_meta = fs::write(&meta_spec_path, default_spec);

    if let Err(err) = write_payload.and(write_meta) {
        let reason = format!("failed writing spec files: {err}");
        quarantine_note(bad_spec_dir, &software_slug, &reason);
        return ReportEntry {
            software: tool.software.clone(),
            priority: tool.priority,
            status: "quarantined".to_string(),
            reason,
            overlap_recipe: resolved.recipe_name,
            overlap_reason: resolved.overlap_reason,
            variant_dir: resolved.variant_dir.display().to_string(),
            package_name: parsed.package_name,
            version: parsed.version,
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: staged_build_sh.display().to_string(),
        };
    }
    #[cfg(unix)]
    {
        if let Err(err) = fs::set_permissions(&payload_spec_path, fs::Permissions::from_mode(0o644))
        {
            let reason = format!(
                "failed to set spec permissions {}: {err}",
                payload_spec_path.display()
            );
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: payload_spec_path.display().to_string(),
                meta_spec_path: meta_spec_path.display().to_string(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
        if let Err(err) = fs::set_permissions(&meta_spec_path, fs::Permissions::from_mode(0o644)) {
            let reason = format!(
                "failed to set spec permissions {}: {err}",
                meta_spec_path.display()
            );
            quarantine_note(bad_spec_dir, &software_slug, &reason);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "quarantined".to_string(),
                reason,
                overlap_recipe: resolved.recipe_name,
                overlap_reason: resolved.overlap_reason,
                variant_dir: resolved.variant_dir.display().to_string(),
                package_name: parsed.package_name,
                version: parsed.version,
                payload_spec_path: payload_spec_path.display().to_string(),
                meta_spec_path: meta_spec_path.display().to_string(),
                staged_build_sh: staged_build_sh.display().to_string(),
            };
        }
    }

    if let Err(err) =
        build_spec_chain_in_container(build_config, &payload_spec_path, &software_slug)
    {
        let reason = format!("payload spec build failed in container: {err}");
        quarantine_note(bad_spec_dir, &software_slug, &reason);
        return ReportEntry {
            software: tool.software.clone(),
            priority: tool.priority,
            status: "quarantined".to_string(),
            reason,
            overlap_recipe: resolved.recipe_name,
            overlap_reason: resolved.overlap_reason,
            variant_dir: resolved.variant_dir.display().to_string(),
            package_name: parsed.package_name,
            version: parsed.version,
            payload_spec_path: payload_spec_path.display().to_string(),
            meta_spec_path: meta_spec_path.display().to_string(),
            staged_build_sh: staged_build_sh.display().to_string(),
        };
    }

    if let Err(err) = build_spec_chain_in_container(
        build_config,
        &meta_spec_path,
        &format!("{software_slug}-default"),
    ) {
        let reason = format!("meta spec build failed in container: {err}");
        quarantine_note(bad_spec_dir, &software_slug, &reason);
        return ReportEntry {
            software: tool.software.clone(),
            priority: tool.priority,
            status: "quarantined".to_string(),
            reason,
            overlap_recipe: resolved.recipe_name,
            overlap_reason: resolved.overlap_reason,
            variant_dir: resolved.variant_dir.display().to_string(),
            package_name: parsed.package_name,
            version: parsed.version,
            payload_spec_path: payload_spec_path.display().to_string(),
            meta_spec_path: meta_spec_path.display().to_string(),
            staged_build_sh: staged_build_sh.display().to_string(),
        };
    }

    clear_quarantine_note(bad_spec_dir, &software_slug);

    let success_reason = match version_state {
        PayloadVersionState::Outdated { existing_version } => format!(
            "spec/srpm/rpm generated from bioconda metadata in container (updated payload from {} to {} and bumped meta package)",
            existing_version, parsed.version
        ),
        PayloadVersionState::NotBuilt => {
            "spec/srpm/rpm generated from bioconda metadata in container".to_string()
        }
        PayloadVersionState::UpToDate { .. } => "already up-to-date".to_string(),
    };

    ReportEntry {
        software: tool.software.clone(),
        priority: tool.priority,
        status: "generated".to_string(),
        reason: success_reason,
        overlap_recipe: resolved.recipe_name,
        overlap_reason: resolved.overlap_reason,
        variant_dir: resolved.variant_dir.display().to_string(),
        package_name: parsed.package_name,
        version: parsed.version,
        payload_spec_path: payload_spec_path.display().to_string(),
        meta_spec_path: meta_spec_path.display().to_string(),
        staged_build_sh: staged_build_sh.display().to_string(),
    }
}

fn resolve_recipe_for_tool(
    tool_name: &str,
    recipe_root: &Path,
    recipe_dirs: &[RecipeDir],
) -> Result<Option<ResolvedRecipe>> {
    resolve_recipe_for_tool_mode(tool_name, recipe_root, recipe_dirs, true)
}

fn resolve_recipe_for_tool_mode(
    tool_name: &str,
    recipe_root: &Path,
    recipe_dirs: &[RecipeDir],
    allow_identifier_lookup: bool,
) -> Result<Option<ResolvedRecipe>> {
    let lower = tool_name.trim().to_lowercase();
    let normalized = normalize_name(tool_name);

    if let Some(recipe) = recipe_dirs
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(tool_name))
    {
        return build_resolved(recipe, "exact-directory-match");
    }
    if let Some(recipe) = recipe_dirs.iter().find(|r| r.normalized == normalized) {
        return build_resolved(recipe, "normalized-directory-match");
    }

    let plus_stripped = normalized.replace("-plus", "").replace("-plus-", "-");
    if let Some(recipe) = recipe_dirs.iter().find(|r| r.normalized == plus_stripped) {
        return build_resolved(recipe, "plus-normalization-match");
    }

    if allow_identifier_lookup && let Some(recipe) = select_fallback_recipe(&lower, recipe_dirs) {
        return build_resolved(recipe, "fallback-directory-match");
    }

    if allow_identifier_lookup {
        let key = normalize_identifier_key(&lower);
        if let Some(recipe) = find_recipe_by_identifier(recipe_root, &key)? {
            return build_resolved(&recipe, "identifier-match");
        }
    }

    Ok(None)
}

fn select_fallback_recipe<'a>(
    tool_lower: &str,
    recipe_dirs: &'a [RecipeDir],
) -> Option<&'a RecipeDir> {
    // Prefer script bundles when users request the base tool name.
    let scripts_candidate = format!("{tool_lower}-scripts");
    if let Some(recipe) = recipe_dirs
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(&scripts_candidate))
    {
        return Some(recipe);
    }

    // Prefer explicit package namespaces when users request a base tool name.
    let direct_prefix = format!("{tool_lower}-");
    let direct_matches: Vec<&RecipeDir> = recipe_dirs
        .iter()
        .filter(|r| r.name.to_lowercase().starts_with(&direct_prefix))
        .collect();
    if direct_matches.len() == 1 {
        return direct_matches.first().copied();
    }

    for candidate in [
        format!("r-{tool_lower}"),
        format!("bioconductor-{tool_lower}"),
    ] {
        if let Some(recipe) = recipe_dirs
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(&candidate))
        {
            return Some(recipe);
        }
    }

    None
}

fn build_resolved(recipe: &RecipeDir, overlap_reason: &str) -> Result<Option<ResolvedRecipe>> {
    let variant_dir = select_recipe_variant_dir(&recipe.path)?;
    let meta_path = meta_file_path(&variant_dir)
        .or_else(|| meta_file_path(&recipe.path))
        .with_context(|| format!("missing meta.yaml/meta.yml in {}", recipe.path.display()))?;

    let build_sh_path = {
        let in_variant = variant_dir.join("build.sh");
        if in_variant.exists() {
            Some(in_variant)
        } else {
            let in_root = recipe.path.join("build.sh");
            if in_root.exists() {
                Some(in_root)
            } else {
                None
            }
        }
    };

    Ok(Some(ResolvedRecipe {
        recipe_name: recipe.name.clone(),
        recipe_dir: recipe.path.clone(),
        variant_dir,
        meta_path,
        build_sh_path,
        overlap_reason: overlap_reason.to_string(),
    }))
}

fn find_recipe_by_identifier(recipe_root: &Path, key: &str) -> Result<Option<RecipeDir>> {
    let pattern = format!("biotools:{key}");
    for entry in fs::read_dir(recipe_root)
        .with_context(|| format!("reading recipe root {}", recipe_root.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", recipe_root.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        let meta_path = match meta_file_path(&path) {
            Some(p) => p,
            None => continue,
        };

        let text = match fs::read_to_string(meta_path) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if text.to_lowercase().contains(&pattern) {
            return Ok(Some(RecipeDir {
                normalized: normalize_name(&name),
                name,
                path,
            }));
        }
    }
    Ok(None)
}

fn select_recipe_variant_dir(recipe_dir: &Path) -> Result<PathBuf> {
    let mut candidates: Vec<(String, PathBuf, bool)> = Vec::new();

    if meta_file_path(recipe_dir).is_some() {
        let version = rendered_recipe_version(recipe_dir)
            .or_else(|| {
                recipe_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "0".to_string());
        candidates.push((version, recipe_dir.to_path_buf(), true));
    }

    for entry in fs::read_dir(recipe_dir)
        .with_context(|| format!("reading recipe directory {}", recipe_dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", recipe_dir.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        if !looks_like_version_dir(&name) {
            continue;
        }
        if meta_file_path(&path).is_none() {
            continue;
        }
        let version = rendered_recipe_version(&path).unwrap_or(name);
        candidates.push((version, path, false));
    }

    if candidates.is_empty() {
        return Ok(recipe_dir.to_path_buf());
    }

    candidates.sort_by(|a, b| compare_version_labels(&a.0, &b.0).then_with(|| a.2.cmp(&b.2)));
    Ok(candidates
        .last()
        .map(|(_, p, _)| p.clone())
        .unwrap_or_else(|| recipe_dir.to_path_buf()))
}

fn rendered_recipe_version(dir: &Path) -> Option<String> {
    let meta_path = meta_file_path(dir)?;
    let text = fs::read_to_string(&meta_path).ok()?;
    let selector_ctx = SelectorContext::for_rpm_build();
    let selected_meta = apply_selectors(&text, &selector_ctx);
    let rendered = render_meta_yaml(&selected_meta).ok()?;
    extract_package_scalar(&rendered, "version").or_else(|| {
        serde_yaml::from_str::<Value>(&rendered)
            .ok()
            .and_then(|root| {
                root.get("package")
                    .and_then(Value::as_mapping)
                    .and_then(|pkg| pkg.get(Value::String("version".to_string())))
                    .and_then(value_to_string)
            })
    })
}

fn looks_like_version_dir(name: &str) -> bool {
    name.chars().any(|c| c.is_ascii_digit())
}

fn compare_version_labels(a: &str, b: &str) -> Ordering {
    let a_parts = version_parts(a);
    let b_parts = version_parts(b);

    let max_len = a_parts.len().max(b_parts.len());
    for idx in 0..max_len {
        match (a_parts.get(idx), b_parts.get(idx)) {
            (Some(VersionPart::Num(x)), Some(VersionPart::Num(y))) => {
                let ord = x.cmp(y);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(VersionPart::Text(x)), Some(VersionPart::Text(y))) => {
                let ord = x.cmp(y);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(VersionPart::Num(_)), Some(VersionPart::Text(_))) => return Ordering::Greater,
            (Some(VersionPart::Text(_)), Some(VersionPart::Num(_))) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }

    Ordering::Equal
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionPart {
    Num(u64),
    Text(String),
}

fn version_parts(label: &str) -> Vec<VersionPart> {
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut current_is_num: Option<bool> = None;

    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            let is_num = ch.is_ascii_digit();
            match current_is_num {
                Some(prev) if prev == is_num => {
                    buf.push(ch);
                }
                Some(_) => {
                    push_version_part(&mut parts, &buf, current_is_num.unwrap_or(false));
                    buf.clear();
                    buf.push(ch);
                    current_is_num = Some(is_num);
                }
                None => {
                    buf.push(ch);
                    current_is_num = Some(is_num);
                }
            }
        } else if !buf.is_empty() {
            push_version_part(&mut parts, &buf, current_is_num.unwrap_or(false));
            buf.clear();
            current_is_num = None;
        }
    }

    if !buf.is_empty() {
        push_version_part(&mut parts, &buf, current_is_num.unwrap_or(false));
    }

    parts
}

fn push_version_part(parts: &mut Vec<VersionPart>, piece: &str, is_num: bool) {
    if is_num {
        if let Ok(v) = piece.parse::<u64>() {
            parts.push(VersionPart::Num(v));
            return;
        }
    }
    parts.push(VersionPart::Text(piece.to_lowercase()));
}

fn meta_file_path(dir: &Path) -> Option<PathBuf> {
    let yaml = dir.join("meta.yaml");
    if yaml.exists() {
        return Some(yaml);
    }
    let yml = dir.join("meta.yml");
    if yml.exists() {
        return Some(yml);
    }
    None
}

fn render_meta_yaml(meta: &str) -> Result<String> {
    let mut env = Environment::new();
    env.add_function("compiler", |lang: String| {
        format!("{}-compiler", lang.to_lowercase())
    });
    env.add_function("pin_subpackage", |name: String, _kwargs: Kwargs| name);
    env.add_function("pin_compatible", |name: String, _kwargs: Kwargs| name);

    let template = env
        .template_from_str(meta)
        .context("creating jinja template from meta.yaml")?;

    template
        .render(context! {
            PYTHON => "$PYTHON",
            PIP => "$PIP",
            PREFIX => "$PREFIX",
            SRC_DIR => "$SRC_DIR",
            RECIPE_DIR => "$RECIPE_DIR",
            R => "R",
            cran_mirror => "https://cran.r-project.org",
            environ => context! {
                PREFIX => "$PREFIX",
                RECIPE_DIR => "$RECIPE_DIR",
                PYTHON => "$PYTHON",
                PIP => "$PIP",
                SRC_DIR => "$SRC_DIR",
            },
        })
        .context("rendering meta.yaml jinja template")
}

#[derive(Debug, Clone, Copy)]
struct SelectorContext {
    linux: bool,
    osx: bool,
    win: bool,
    aarch64: bool,
    arm64: bool,
    x86_64: bool,
    py_major: i64,
    py_minor: i64,
}

impl SelectorContext {
    fn for_rpm_build() -> Self {
        let arch = std::env::consts::ARCH;
        let linux = true;
        let osx = false;
        let win = false;
        let aarch64 = arch == "aarch64" || arch == "arm64";
        // In Bioconda selectors, arm64 tracks macOS arm64 rather than Linux aarch64.
        let arm64 = osx && aarch64;
        let x86_64 = arch == "x86_64" || arch == "amd64";
        Self {
            linux,
            osx,
            win,
            aarch64,
            arm64,
            x86_64,
            py_major: 3,
            py_minor: 11,
        }
    }
}

fn apply_selectors(meta: &str, ctx: &SelectorContext) -> String {
    let mut out = String::new();
    for line in meta.lines() {
        if let Some((prefix, selector)) = split_selector(line) {
            if evaluate_selector(selector, ctx) {
                out.push_str(prefix.trim_end());
                out.push('\n');
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn split_selector(line: &str) -> Option<(&str, &str)> {
    let idx = line.find("# [")?;
    let prefix = &line[..idx];
    let rest = &line[idx + 3..];
    let end = rest.find(']')?;
    let selector = &rest[..end];
    Some((prefix, selector.trim()))
}

fn evaluate_selector(selector: &str, ctx: &SelectorContext) -> bool {
    // Minimal selector evaluator that covers common Bioconda cases (linux/osx/win/arch/py).
    selector.split(" or ").any(|or_clause| {
        or_clause
            .split(" and ")
            .all(|term| evaluate_selector_term(term.trim(), ctx))
    })
}

fn evaluate_selector_term(term: &str, ctx: &SelectorContext) -> bool {
    if let Some(stripped) = term.strip_prefix("not ") {
        return !evaluate_selector_term(stripped.trim(), ctx);
    }

    match term {
        "linux" => ctx.linux,
        "osx" => ctx.osx,
        "win" => ctx.win,
        "unix" => ctx.linux || ctx.osx,
        "aarch64" => ctx.aarch64,
        "arm64" => ctx.arm64,
        "linux-aarch64" => ctx.linux && ctx.aarch64,
        "osx-arm64" => ctx.osx && ctx.arm64,
        "x86_64" | "amd64" => ctx.x86_64,
        _ => evaluate_python_selector(term, ctx).unwrap_or(false),
    }
}

fn evaluate_python_selector(term: &str, ctx: &SelectorContext) -> Option<bool> {
    if !term.starts_with("py") {
        return None;
    }

    let ops = [">=", "<=", "==", "!=", ">", "<"];
    for op in ops {
        if let Some(rest) = term.strip_prefix(&format!("py{op}")) {
            let value = rest.trim().parse::<i64>().ok()?;
            let current = ctx.py_major * 100 + ctx.py_minor;
            return Some(match op {
                ">=" => current >= value,
                "<=" => current <= value,
                "==" => current == value,
                "!=" => current != value,
                ">" => current > value,
                "<" => current < value,
                _ => false,
            });
        }
    }
    None
}

fn parse_rendered_meta(rendered: &str) -> Result<ParsedMeta> {
    let root: Value = serde_yaml::from_str(rendered).context("deserializing rendered meta.yaml")?;

    let package = root
        .get("package")
        .and_then(Value::as_mapping)
        .context("missing package section")?;

    let package_name = package
        .get(Value::String("name".to_string()))
        .and_then(value_to_string)
        .or_else(|| extract_package_scalar(rendered, "name"))
        .context("missing package.name")?;

    let version = extract_package_scalar(rendered, "version")
        .or_else(|| {
            package
                .get(Value::String("version".to_string()))
                .and_then(value_to_string)
        })
        .context("missing package.version")?;

    let source_url = extract_source_url(root.get("source")).unwrap_or_default();
    let source_folder = extract_source_folder(root.get("source")).unwrap_or_default();
    let about = root.get("about").and_then(Value::as_mapping);

    let homepage = about
        .and_then(|m| m.get(Value::String("home".to_string())))
        .and_then(value_to_string)
        .unwrap_or_default();

    let license = about
        .and_then(|m| m.get(Value::String("license".to_string())))
        .and_then(value_to_string)
        .unwrap_or_else(|| "NOASSERTION".to_string());

    let summary = about
        .and_then(|m| m.get(Value::String("summary".to_string())))
        .and_then(value_to_string)
        .unwrap_or_else(|| format!("Generated package for {package_name}"));
    let source_patches = extract_source_patches(root.get("source"));
    let build = root.get("build").and_then(Value::as_mapping);
    let build_script = build
        .and_then(|m| m.get(Value::String("script".to_string())))
        .and_then(extract_build_script);
    let build_number = build
        .and_then(|m| m.get(Value::String("number".to_string())))
        .and_then(value_to_string)
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "0".to_string());
    let noarch_python = build
        .and_then(|m| m.get(Value::String("noarch".to_string())))
        .and_then(value_to_string)
        .map(|v| v.trim().eq_ignore_ascii_case("python"))
        .unwrap_or(false);

    let requirements = root.get("requirements").and_then(Value::as_mapping);
    let build_deps = requirements
        .and_then(|m| m.get(Value::String("build".to_string())))
        .map(extract_deps)
        .unwrap_or_default();
    let build_dep_specs_raw = requirements
        .and_then(|m| m.get(Value::String("build".to_string())))
        .map(extract_dep_specs_raw)
        .unwrap_or_default();

    let host_deps = requirements
        .and_then(|m| m.get(Value::String("host".to_string())))
        .map(extract_deps)
        .unwrap_or_default();
    let host_dep_specs_raw = requirements
        .and_then(|m| m.get(Value::String("host".to_string())))
        .map(extract_dep_specs_raw)
        .unwrap_or_default();

    let run_deps = requirements
        .and_then(|m| m.get(Value::String("run".to_string())))
        .map(extract_deps)
        .unwrap_or_default();
    let run_dep_specs_raw = requirements
        .and_then(|m| m.get(Value::String("run".to_string())))
        .map(extract_dep_specs_raw)
        .unwrap_or_default();

    Ok(ParsedMeta {
        package_name,
        version,
        build_number,
        source_url,
        source_folder,
        homepage,
        license,
        summary,
        source_patches,
        build_script,
        noarch_python,
        build_dep_specs_raw,
        host_dep_specs_raw,
        run_dep_specs_raw,
        build_deps,
        host_deps,
        run_deps,
    })
}

fn rendered_meta_declares_build_skip(rendered: &str) -> bool {
    let doc: Value = match serde_yaml::from_str(rendered) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let build = match doc.get("build") {
        Some(v) => v,
        None => return false,
    };
    let skip = match build.get("skip") {
        Some(v) => v,
        None => return false,
    };
    if let Some(b) = skip.as_bool() {
        return b;
    }
    if let Some(s) = skip.as_str() {
        let normalized = s.trim().to_ascii_lowercase();
        return normalized == "true" || normalized == "yes" || normalized == "1";
    }
    false
}

fn extract_dep_specs_raw(node: &Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    match node {
        Value::Sequence(items) => {
            for item in items {
                if let Some(raw) = value_to_string(item)
                    && let Some(spec) = normalize_dep_spec_raw(&raw)
                {
                    out.insert(spec);
                }
            }
        }
        Value::String(raw) => {
            if let Some(spec) = normalize_dep_spec_raw(raw) {
                out.insert(spec);
            }
        }
        _ => {}
    }
    out.into_iter().collect()
}

fn normalize_dep_spec_raw(raw: &str) -> Option<String> {
    let cleaned = raw.trim().trim_matches('"').trim_matches('\'');
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

fn precompiled_binary_override(
    software_slug: &str,
    parsed: &ParsedMeta,
) -> Option<PrecompiledBinaryOverride> {
    match software_slug {
        "k8" => Some(PrecompiledBinaryOverride {
            source_url: format!(
                "https://github.com/attractivechaos/k8/releases/download/v{version}/k8-{version}.tar.bz2",
                version = parsed.version
            ),
            build_script: render_k8_precompiled_build_script(&parsed.version),
        }),
        _ => None,
    }
}

fn render_k8_precompiled_build_script(version: &str) -> String {
    format!(
        "#!/usr/bin/env bash\n\
set -euxo pipefail\n\
\n\
K8_VERSION={version}\n\
platform=\"$(uname -s)\"\n\
arch=\"$(uname -m)\"\n\
candidate=\"\"\n\
case \"${{platform}}/${{arch}}\" in\n\
  Linux/x86_64)\n\
    candidate=\"k8-x86_64-Linux\"\n\
    ;;\n\
  Darwin/arm64|Darwin/aarch64)\n\
    candidate=\"k8-arm64-Darwin\"\n\
    ;;\n\
  *)\n\
    echo \"no upstream precompiled k8 binary for ${{platform}}/${{arch}}; available entries: k8-x86_64-Linux,k8-arm64-Darwin\" >&2\n\
    exit 86\n\
    ;;\n\
esac\n\
\n\
src_bin=\"\"\n\
for path in \\\n\
  \"$SRC_DIR/$candidate\" \\\n\
  \"$SRC_DIR/k8-${{K8_VERSION}}/$candidate\" \\\n\
  \"$candidate\" \\\n\
  \"k8-${{K8_VERSION}}/$candidate\"\n\
do\n\
  if [[ -f \"$path\" ]]; then\n\
    src_bin=\"$path\"\n\
    break\n\
  fi\n\
done\n\
\n\
if [[ -z \"$src_bin\" ]]; then\n\
  echo \"precompiled k8 binary $candidate not found under $SRC_DIR\" >&2\n\
  find \"$SRC_DIR\" -maxdepth 2 -type f -name 'k8-*' -print >&2 || true\n\
  exit 87\n\
fi\n\
\n\
install -d \"$PREFIX/bin\"\n\
install -m 0755 \"$src_bin\" \"$PREFIX/bin/k8\"\n",
        version = version
    )
}

fn is_python_recipe(parsed: &ParsedMeta) -> bool {
    if parsed.noarch_python {
        return true;
    }
    let package = parsed.package_name.trim().replace('_', "-").to_lowercase();
    if package.starts_with("python-") || package.starts_with("py-") {
        return true;
    }
    parsed
        .build_script
        .as_deref()
        .map(|s| {
            let lower = s.to_lowercase();
            lower.contains("pip install") || lower.contains("python -m pip")
        })
        .unwrap_or(false)
}

fn recipe_requires_r_runtime(parsed: &ParsedMeta) -> bool {
    parsed
        .build_deps
        .iter()
        .chain(parsed.host_deps.iter())
        .chain(parsed.run_deps.iter())
        .any(|dep| is_r_ecosystem_dependency_name(dep))
}

fn is_r_base_dependency_name(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    matches!(normalized.as_str(), "r" | "r-base" | "r-essentials")
}

fn is_cran_r_dependency_name(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    normalized.starts_with("r-") && !is_r_base_dependency_name(&normalized)
}

fn build_r_cran_requirements(parsed: &ParsedMeta) -> Vec<String> {
    let mut out = BTreeSet::new();
    for dep in parsed
        .build_deps
        .iter()
        .chain(parsed.host_deps.iter())
        .chain(parsed.run_deps.iter())
    {
        if is_cran_r_dependency_name(dep) {
            let normalized = normalize_dependency_token(dep);
            if let Some(pkg) = normalized.strip_prefix("r-")
                && !pkg.is_empty()
            {
                out.insert(pkg.to_string());
            }
        }
    }
    out.into_iter().collect()
}

fn recipe_requires_rust_runtime(parsed: &ParsedMeta) -> bool {
    parsed
        .build_deps
        .iter()
        .chain(parsed.host_deps.iter())
        .chain(parsed.run_deps.iter())
        .any(|dep| is_rust_ecosystem_dependency_name(dep))
}

fn recipe_requires_nim_runtime(parsed: &ParsedMeta) -> bool {
    parsed
        .build_deps
        .iter()
        .chain(parsed.host_deps.iter())
        .chain(parsed.run_deps.iter())
        .any(|dep| is_nim_ecosystem_dependency_name(dep))
}

fn is_r_project_recipe(parsed: &ParsedMeta) -> bool {
    let package = parsed.package_name.trim().replace('_', "-").to_lowercase();
    package == "r"
        || package == "r-base"
        || package.starts_with("r-")
        || package.starts_with("bioconductor-")
        || parsed
            .build_script
            .as_deref()
            .map(script_text_indicates_r)
            .unwrap_or(false)
}

fn build_python_requirements(parsed: &ParsedMeta) -> Vec<String> {
    let runtime_incompatible = recipe_python_runtime_incompatible(parsed);
    let mut out = BTreeSet::new();
    for raw in parsed
        .host_dep_specs_raw
        .iter()
        .chain(parsed.run_dep_specs_raw.iter())
    {
        if let Some(req) = conda_dep_to_pip_requirement(raw) {
            let normalized_req = if runtime_incompatible {
                relax_pip_requirement_for_runtime(req)
            } else {
                req
            };
            if runtime_incompatible
                && requirement_hits_python_abi_incompatible_legacy_dep(&normalized_req)
            {
                continue;
            }
            out.insert(normalized_req);
        }
    }
    // Legacy pomegranate releases (used by Bioconda CNVKit) are not compatible
    // with Cython 3 / NumPy 2; force compatible caps in the locked venv set.
    if out.iter().any(|req| req.starts_with("pomegranate")) {
        out.insert("cython<3".to_string());
        out.insert("numpy<2".to_string());
    }
    out.into_iter().collect()
}

fn recipe_python_runtime_incompatible(parsed: &ParsedMeta) -> bool {
    parsed
        .build_dep_specs_raw
        .iter()
        .chain(parsed.host_dep_specs_raw.iter())
        .chain(parsed.run_dep_specs_raw.iter())
        .any(|raw| python_dep_spec_conflicts_with_runtime(raw, 3, 11))
}

fn python_dep_spec_conflicts_with_runtime(
    raw: &str,
    runtime_major: u64,
    runtime_minor: u64,
) -> bool {
    let cleaned = raw
        .split('#')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches('"')
        .trim_matches('\'');
    if cleaned.is_empty() {
        return false;
    }

    let mut parts = cleaned.split_whitespace();
    let Some(first_token) = parts.next() else {
        return false;
    };
    let name_token = extract_dependency_name_from_token(first_token);
    if normalize_dependency_token(name_token) != "python" {
        return false;
    }

    let inline_spec = first_token[name_token.len()..].trim();
    let remainder = cleaned[first_token.len()..].trim();
    let spec = if !inline_spec.is_empty() {
        inline_spec
    } else {
        remainder
    };
    if spec.is_empty() {
        return false;
    }

    for clause in spec.split(',') {
        let token = clause.trim();
        if token.is_empty() {
            continue;
        }
        if let Some(version) = token.strip_prefix("<=")
            && let Some((major, minor)) = parse_major_minor(version)
            && (runtime_major, runtime_minor) > (major, minor)
        {
            return true;
        } else if let Some(version) = token.strip_prefix('<')
            && let Some((major, minor)) = parse_major_minor(version)
            && (runtime_major, runtime_minor) >= (major, minor)
        {
            return true;
        } else if let Some(version) = token.strip_prefix("==")
            && let Some((major, minor)) = parse_major_minor(version)
            && (runtime_major, runtime_minor) != (major, minor)
        {
            return true;
        } else if let Some(version) = token.strip_prefix('=')
            && let Some((major, minor)) = parse_major_minor(version)
            && (runtime_major, runtime_minor) != (major, minor)
        {
            return true;
        }
    }

    false
}

fn parse_major_minor(version: &str) -> Option<(u64, u64)> {
    let mut pieces = version
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .split('.');
    let major = pieces.next()?.trim().parse::<u64>().ok()?;
    let minor = pieces
        .next()
        .and_then(|m| m.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Some((major, minor))
}

fn relax_pip_requirement_for_runtime(requirement: String) -> String {
    let split_at = requirement
        .char_indices()
        .find(|(_, c)| ['<', '>', '=', '!', '~'].contains(c))
        .map(|(idx, _)| idx);
    let Some(split_at) = split_at else {
        return requirement;
    };

    let name = requirement[..split_at].trim();
    let spec = requirement[split_at..].trim();
    if name.is_empty() || spec.is_empty() {
        return requirement;
    }

    let mut clauses: Vec<String> = Vec::new();
    for token in spec.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("<=") || trimmed.starts_with('<') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("==") {
            clauses.push(format!(">={}", rest.trim()));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('=')
            && !trimmed.starts_with("=>")
        {
            clauses.push(format!(">={}", rest.trim()));
            continue;
        }
        clauses.push(trimmed.to_string());
    }

    if clauses.is_empty() {
        name.to_string()
    } else {
        format!("{name}{}", clauses.join(","))
    }
}

fn requirement_hits_python_abi_incompatible_legacy_dep(requirement: &str) -> bool {
    let split_at = requirement
        .char_indices()
        .find(|(_, c)| ['<', '>', '=', '!', '~'].contains(c))
        .map(|(idx, _)| idx)
        .unwrap_or(requirement.len());
    let name = requirement[..split_at].trim().to_lowercase();
    matches!(name.as_str(), "fa2" | "mnnpy")
}

fn conda_dep_to_pip_requirement(raw: &str) -> Option<String> {
    let cleaned = raw
        .split('#')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches('"')
        .trim_matches('\'');
    if cleaned.is_empty() {
        return None;
    }

    let mut parts = cleaned.split_whitespace();
    let first_token = parts.next()?;
    let name_token = extract_dependency_name_from_token(first_token);
    if name_token.is_empty() {
        return None;
    }
    let normalized = normalize_dependency_token(name_token);
    if is_phoreus_python_toolchain_dependency(&normalized) {
        return None;
    }
    if is_python_dev_test_dependency_name(&normalized) {
        return None;
    }
    if is_r_ecosystem_dependency_name(&normalized) {
        return None;
    }
    if is_rust_ecosystem_dependency_name(&normalized) {
        return None;
    }
    if is_nim_ecosystem_dependency_name(&normalized) {
        return None;
    }
    if !is_python_ecosystem_dependency_name(&normalized) {
        return None;
    }

    let pip_name = match normalized.as_str() {
        "python-annoy" => "annoy".to_string(),
        "python-kaleido" => "kaleido".to_string(),
        "matplotlib-base" => "matplotlib".to_string(),
        other => other.to_string(),
    };

    let inline_spec = first_token[name_token.len()..].trim();
    let remainder_after_first = cleaned[first_token.len()..].trim();
    let spec_token = if !inline_spec.is_empty() {
        inline_spec
    } else {
        remainder_after_first
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim()
    };
    if spec_token.is_empty() {
        return Some(pip_name);
    }

    let requirement = if spec_token.starts_with(['>', '<', '=', '!', '~']) {
        format!(
            "{pip_name}{}",
            normalize_conda_version_spec_for_pip(spec_token)
        )
    } else if spec_token
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        format!("{pip_name}=={spec_token}")
    } else {
        pip_name
    };

    Some(requirement)
}

fn normalize_conda_version_spec_for_pip(spec: &str) -> String {
    let trimmed = spec.trim();
    if trimmed.starts_with('=') && !trimmed.starts_with("==") {
        format!("=={}", trimmed.trim_start_matches('='))
    } else {
        trimmed.to_string()
    }
}

fn extract_dependency_name_from_token(token: &str) -> &str {
    let trimmed = token.trim().trim_matches(',');
    let split_idx = trimmed
        .find(['<', '>', '=', '!', '~'])
        .unwrap_or(trimmed.len());
    trimmed[..split_idx].trim()
}

fn should_keep_rpm_dependency_for_python(dep: &str) -> bool {
    let normalized = dep.trim().replace('_', "-").to_lowercase();
    !is_python_ecosystem_dependency_name(&normalized)
}

fn is_python_dev_test_dependency_name(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    matches!(
        normalized.as_str(),
        "bats"
            | "black"
            | "coverage"
            | "flake8"
            | "hypothesis"
            | "mypy"
            | "nose"
            | "pre-commit"
            | "pytest"
            | "pytest-cov"
            | "pytest-runner"
            | "ruff"
            | "tox"
    )
}

fn is_python_ecosystem_dependency_name(normalized: &str) -> bool {
    if is_r_ecosystem_dependency_name(normalized) {
        return false;
    }
    if is_phoreus_python_toolchain_dependency(normalized) {
        return true;
    }

    if matches!(
        normalized,
        "bedtools"
            | "samtools"
            | "bcftools"
            | "htslib"
            | "bwa"
            | "blast"
            | "fastqc"
            | "trimmomatic"
            | "star"
            | "gmap"
            | "salmon"
            | "kallisto"
            | "bowtie"
            | "bowtie2"
            | "minimap2"
            | "mummer"
            | "gcc"
            | "gcc-c++"
            | "gcc-gfortran"
            | "golang"
            | "make"
            | "cmake"
            | "ninja"
            | "pkg-config"
            | "patch"
            | "sed"
            | "tar"
            | "gzip"
            | "bzip2"
            | "xz"
            | "unzip"
            | "which"
            | "findutils"
            | "coreutils"
            | "bash"
            | "perl"
            | "rust"
            | "cargo"
            | "java-11-openjdk"
            | "openjdk"
            | "openssl"
            | "openssl-devel"
            | "zlib"
            | "zlib-devel"
            | "bzip2-devel"
            | "xz-devel"
            | "libffi-devel"
            | "sqlite-devel"
            | "ncurses-devel"
            | "glibc"
            | "glibc-devel"
    ) {
        return false;
    }

    if normalized.ends_with("-compiler")
        || normalized.ends_with("-devel")
        || normalized.starts_with("lib")
    {
        return false;
    }

    true
}

fn extract_build_script(node: &Value) -> Option<String> {
    match node {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Sequence(items) => {
            let lines: Vec<String> = items
                .iter()
                .filter_map(value_to_string)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if lines.is_empty() {
                None
            } else {
                Some(lines.join("\n"))
            }
        }
        _ => None,
    }
}

fn synthesize_build_sh_from_meta_script(script: &str) -> String {
    let canonical = canonicalize_meta_build_script(script);
    format!("#!/usr/bin/env bash\nset -euxo pipefail\n{canonical}\n")
}

fn canonicalize_meta_build_script(script: &str) -> String {
    let normalized = script
        .replace("{{ PYTHON }}", "$PYTHON")
        .replace("{{PYTHON}}", "$PYTHON")
        .replace("{{ PIP }}", "$PIP")
        .replace("{{PIP}}", "$PIP");

    let mut lines = Vec::new();
    for raw in normalized.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let rewritten = if line.starts_with("-m ") {
            format!("$PYTHON {line}")
        } else if let Some(rest) = line.strip_prefix("python3 ") {
            format!("$PYTHON {rest}")
        } else if let Some(rest) = line.strip_prefix("python ") {
            format!("$PYTHON {rest}")
        } else if let Some(rest) = line.strip_prefix("pip3 ") {
            format!("$PIP {rest}")
        } else if let Some(rest) = line.strip_prefix("pip ") {
            format!("$PIP {rest}")
        } else {
            line.to_string()
        };
        lines.push(rewritten);
    }

    if lines.is_empty() {
        "$PYTHON -m pip install . --no-deps --no-build-isolation --no-cache-dir -vvv".to_string()
    } else {
        lines.join("\n")
    }
}

fn harden_staged_build_script(path: &Path) -> Result<()> {
    let original = fs::read_to_string(path)
        .with_context(|| format!("reading staged build script {}", path.display()))?;
    let hardened = harden_build_script_text(&original);
    if hardened != original {
        fs::write(path, hardened)
            .with_context(|| format!("writing hardened build script {}", path.display()))?;
    }
    Ok(())
}

fn harden_build_script_text(script: &str) -> String {
    let mut rewritten_lines = Vec::new();
    let mut rewrite_counter = 0usize;

    for line in script.lines() {
        if let Some(expanded) = rewrite_streamed_wget_tar_line(line, rewrite_counter) {
            rewritten_lines.extend(expanded);
            rewrite_counter += 1;
        } else if let Some(expanded) = rewrite_glob_copy_to_prefix_bin_line(line) {
            rewritten_lines.extend(expanded);
        } else if let Some(rewritten) = rewrite_cargo_bundle_licenses_line(line) {
            rewritten_lines.push(rewritten);
        } else {
            rewritten_lines.push(line.to_string());
        }
    }

    let mut out = rewritten_lines.join("\n");
    if script.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn rewrite_cargo_bundle_licenses_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("cargo-bundle-licenses") {
        return None;
    }

    let indent = &line[..line.len() - trimmed.len()];
    Some(format!(
        "{indent}echo \"Skipping cargo-bundle-licenses (not required for RPM payload build)\""
    ))
}

fn rewrite_streamed_wget_tar_line(line: &str, counter: usize) -> Option<Vec<String>> {
    let trimmed = line.trim();
    let (left, right) = trimmed.split_once("| tar -zxf -")?;
    if !right.trim().is_empty() {
        return None;
    }

    let tokens: Vec<&str> = left.split_whitespace().collect();
    if tokens.first().copied() != Some("wget") {
        return None;
    }
    let out_idx = tokens.iter().position(|t| *t == "-O-")?;
    if out_idx + 1 >= tokens.len() {
        return None;
    }

    let wget_opts = tokens[1..out_idx].join(" ");
    let wget_url = tokens[(out_idx + 1)..].join(" ");
    if wget_url.is_empty() {
        return None;
    }

    let indent = &line[..line.len() - line.trim_start().len()];
    let tmp_var = format!("BIOCONDA2RPM_FETCH_{counter}_ARCHIVE");
    let wget_prefix = if wget_opts.is_empty() {
        "wget --no-verbose".to_string()
    } else {
        format!("wget --no-verbose {wget_opts}")
    };

    Some(vec![
        format!("{indent}{tmp_var}=\"$(mktemp -t bioconda2rpm-src.XXXXXX.tar.gz)\""),
        format!("{indent}{wget_prefix} -O \"${{{tmp_var}}}\" {wget_url}"),
        format!("{indent}tar -zxf \"${{{tmp_var}}}\""),
        format!("{indent}rm -f \"${{{tmp_var}}}\""),
    ])
}

fn rewrite_glob_copy_to_prefix_bin_line(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    let (glob, single_quoted) = if let Some(rest) = trimmed.strip_prefix("cp *.") {
        (rest, false)
    } else if let Some(rest) = trimmed.strip_prefix("cp '*.") {
        (rest, true)
    } else {
        return None;
    };
    let (ext, remainder) = if single_quoted {
        let (ext, rem) = glob.split_once('\'')?;
        (ext, rem.trim())
    } else {
        let (ext, rem) = glob.split_once(' ')?;
        (ext, rem.trim())
    };
    if ext.is_empty() {
        return None;
    }
    if remainder != "$PREFIX/bin" && remainder != "\"$PREFIX/bin\"" {
        return None;
    }

    let indent = &line[..line.len() - line.trim_start().len()];
    let pattern = format!("*.{ext}");
    Some(vec![
        format!("{indent}while IFS= read -r -d '' _bioconda2rpm_src; do"),
        format!("{indent}  cp \"$_bioconda2rpm_src\" \"$PREFIX/bin/\""),
        format!("{indent}done < <(find . -maxdepth 2 -type f -name '{pattern}' -print0)"),
    ])
}

fn staged_build_script_indicates_python(path: &Path) -> Result<bool> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading staged build script {}", path.display()))?;
    Ok(script_text_indicates_python(&text))
}

fn staged_build_script_indicates_r(path: &Path) -> Result<bool> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading staged build script {}", path.display()))?;
    Ok(script_text_indicates_r(&text))
}

fn staged_build_script_indicates_rust(path: &Path) -> Result<bool> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading staged build script {}", path.display()))?;
    Ok(script_text_indicates_rust(&text))
}

fn script_text_indicates_python(script: &str) -> bool {
    let lower = script.to_lowercase();
    lower.contains("pip install")
        || lower.contains("python -m pip")
        || lower.contains("python3 -m pip")
        || lower.contains("python setup.py")
        || lower.contains("setup.py install")
}

fn script_text_indicates_r(script: &str) -> bool {
    let lower = script.to_lowercase();
    lower.contains("rscript ")
        || lower.contains(" r -e")
        || lower.contains("r -e ")
        || lower.contains("renv::")
        || lower.contains("install.packages(")
}

fn script_text_indicates_rust(script: &str) -> bool {
    let lower = script.to_lowercase();
    lower.contains("cargo ")
        || lower.contains("cargo\n")
        || lower.contains("rustc ")
        || lower.contains("rustup ")
}

fn extract_package_scalar(rendered: &str, key: &str) -> Option<String> {
    let mut in_package = false;
    for line in rendered.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !line.starts_with(' ') && trimmed == "package:" {
            in_package = true;
            continue;
        }
        if in_package && !line.starts_with(' ') {
            break;
        }
        if in_package {
            let needle = format!("{key}:");
            if let Some(rest) = trimmed.strip_prefix(&needle) {
                let value = rest.trim().trim_matches('\"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn extract_source_url(source: Option<&Value>) -> Option<String> {
    match source {
        Some(Value::Mapping(map)) => {
            if let Some(url) = map
                .get(Value::String("url".to_string()))
                .and_then(extract_first_string_or_sequence_item)
            {
                return Some(url);
            }
            if let Some(git_url) = map
                .get(Value::String("git_url".to_string()))
                .and_then(value_to_string)
            {
                let git_rev = map
                    .get(Value::String("git_rev".to_string()))
                    .and_then(value_to_string);
                return synthesize_git_source_descriptor(&git_url, git_rev.as_deref());
            }
            None
        }
        Some(Value::Sequence(seq)) => seq.iter().find_map(|item| {
            if let Some(s) = extract_first_string_or_sequence_item(item) {
                return Some(s);
            }
            let map = item.as_mapping()?;
            if let Some(url) = map
                .get(Value::String("url".to_string()))
                .and_then(extract_first_string_or_sequence_item)
            {
                return Some(url);
            }
            if let Some(git_url) = map
                .get(Value::String("git_url".to_string()))
                .and_then(value_to_string)
            {
                let git_rev = map
                    .get(Value::String("git_rev".to_string()))
                    .and_then(value_to_string);
                return synthesize_git_source_descriptor(&git_url, git_rev.as_deref());
            }
            None
        }),
        Some(Value::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn synthesize_git_source_descriptor(git_url: &str, git_rev: Option<&str>) -> Option<String> {
    let rev = git_rev?.trim();
    if rev.is_empty() {
        return None;
    }
    let url = git_url.trim().trim_end_matches('/');
    if url.is_empty() {
        return None;
    }
    Some(format!("git+{url}#{rev}"))
}

fn extract_first_string_or_sequence_item(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Sequence(items) => items.iter().find_map(value_to_string),
        _ => value_to_string(value),
    }
}

fn extract_source_folder(source: Option<&Value>) -> Option<String> {
    match source {
        Some(Value::Mapping(map)) => map
            .get(Value::String("folder".to_string()))
            .and_then(value_to_string),
        Some(Value::Sequence(seq)) => seq.iter().find_map(|item| {
            item.as_mapping()
                .and_then(|m| m.get(Value::String("folder".to_string())))
                .and_then(value_to_string)
        }),
        _ => None,
    }
}

fn extract_source_patches(source: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    match source {
        Some(Value::Mapping(map)) => {
            if let Some(patches) = map.get(Value::String("patches".to_string())) {
                out.extend(extract_patch_list(patches));
            }
        }
        Some(Value::Sequence(seq)) => {
            for item in seq {
                if let Some(map) = item.as_mapping()
                    && let Some(patches) = map.get(Value::String("patches".to_string()))
                {
                    out.extend(extract_patch_list(patches));
                }
            }
        }
        _ => {}
    }
    out
}

fn extract_patch_list(node: &Value) -> Vec<String> {
    match node {
        Value::Sequence(items) => items
            .iter()
            .filter_map(value_to_string)
            .filter(|s| !s.trim().is_empty())
            .collect(),
        Value::String(s) => {
            if s.trim().is_empty() {
                Vec::new()
            } else {
                vec![s.clone()]
            }
        }
        _ => Vec::new(),
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn extract_deps(node: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();

    match node {
        Value::Sequence(items) => {
            for item in items {
                if let Some(raw) = value_to_string(item)
                    && let Some(dep) = normalize_dependency_name(&raw)
                {
                    out.insert(dep);
                }
            }
        }
        Value::String(raw) => {
            if let Some(dep) = normalize_dependency_name(raw) {
                out.insert(dep);
            }
        }
        _ => {}
    }

    out
}

fn normalize_dependency_name(raw: &str) -> Option<String> {
    let cleaned = raw.trim().trim_matches('"').trim_matches('\'');
    if cleaned.is_empty() {
        return None;
    }

    let token = cleaned
        .split_whitespace()
        .next()
        .map(extract_dependency_name_from_token)
        .unwrap_or_default();

    if token.is_empty() {
        return None;
    }

    let mut normalized = normalize_dependency_token(token);
    if normalized.starts_with("bioconductor-") || normalized.starts_with("r-") {
        normalized = normalized.replace('.', "-");
    }
    if is_phoreus_python_toolchain_dependency(&normalized) {
        return Some(PHOREUS_PYTHON_PACKAGE.to_string());
    }

    let mapped = match normalized.as_str() {
        "c-compiler" | "ccompiler" => "gcc".to_string(),
        "cxx-compiler" | "cpp-compiler" => "gcc-c++".to_string(),
        "fortran-compiler" => "gcc-gfortran".to_string(),
        "go-compiler" | "gocompiler" => "golang".to_string(),
        "openjdk" => "java-11-openjdk".to_string(),
        other => other.to_string(),
    };

    Some(mapped)
}

fn render_payload_spec(
    software_slug: &str,
    parsed: &ParsedMeta,
    staged_build_sh_name: &str,
    staged_patch_sources: &[String],
    meta_path: &Path,
    variant_dir: &Path,
    noarch_python: bool,
    python_script_hint: bool,
    r_script_hint: bool,
    rust_script_hint: bool,
) -> String {
    let license = spec_escape(&parsed.license);
    let summary = spec_escape(&parsed.summary);
    let homepage = spec_escape_or_default(&parsed.homepage, "https://bioconda.github.io");
    let source_url =
        spec_escape_or_default(&parsed.source_url, "https://example.invalid/source.tar.gz");
    let source_subdir = {
        let folder = parsed.source_folder.trim().trim_matches('/');
        if folder.is_empty() {
            "buildsrc".to_string()
        } else {
            format!("buildsrc/{folder}")
        }
    };
    // Conda-build exposes SRC_DIR as the parent work directory; when source
    // entries specify `folder:`, recipes typically address those as
    // `$SRC_DIR/<folder>`.
    let source_relsubdir = ".".to_string();
    // Python policy is applied when either metadata or staged build script indicates
    // Python packaging/install semantics.
    let python_recipe = is_python_recipe(parsed) || python_script_hint;
    let r_runtime_required = recipe_requires_r_runtime(parsed) || r_script_hint;
    let rust_runtime_required = recipe_requires_rust_runtime(parsed) || rust_script_hint;
    let nim_runtime_required = recipe_requires_nim_runtime(parsed);
    let perl_recipe = normalize_name(&parsed.package_name).starts_with("perl-");
    let runtime_only_metapackage = is_runtime_only_metapackage(parsed);
    let r_project_recipe = is_r_project_recipe(parsed) || r_script_hint;
    let r_cran_requirements = if r_runtime_required {
        build_r_cran_requirements(parsed)
    } else {
        Vec::new()
    };
    let python_requirements = if python_recipe {
        build_python_requirements(parsed)
    } else {
        Vec::new()
    };
    let python_venv_setup = render_python_venv_setup_block(python_recipe, &python_requirements);
    let r_runtime_setup =
        render_r_runtime_setup_block(r_runtime_required, r_project_recipe, &r_cran_requirements);
    let rust_runtime_setup = render_rust_runtime_setup_block(rust_runtime_required);
    let nim_runtime_setup = render_nim_runtime_setup_block(nim_runtime_required);
    let module_lua_env = render_module_lua_env_block(
        python_recipe,
        r_runtime_required,
        rust_runtime_required,
        nim_runtime_required,
    );

    let source_kind = source_archive_kind(&parsed.source_url);
    let git_source = parse_git_source_descriptor(&parsed.source_url);
    let include_source0 = !runtime_only_metapackage && source_kind != SourceArchiveKind::Git;
    let mut source_unpack_prep = if include_source0 {
        render_source_unpack_prep_block(source_kind)
    } else {
        if source_kind == SourceArchiveKind::Git {
            render_source_unpack_prep_block(source_kind)
        } else {
            "rm -rf buildsrc\n\
mkdir -p %{bioconda_source_subdir}\n"
                .to_string()
        }
    };
    // UCSC userApps source archives contain an extra top-level `userApps/`
    // directory after tar extraction. Strip two path components so patch paths
    // rooted at `kent/src/...` resolve correctly.
    if software_slug == "ucsc-bigwigsummary"
        && source_kind == SourceArchiveKind::Tar
        && parsed.source_url.contains("userApps.")
        && parsed.source_url.contains(".src.tgz")
    {
        source_unpack_prep =
            source_unpack_prep.replace("--strip-components=1", "--strip-components=2");
    }

    let mut build_requires = BTreeSet::new();
    build_requires.insert("bash".to_string());
    // Enforce canonical builder policy: every payload build uses Phoreus Python,
    // never the system interpreter.
    build_requires.insert(PHOREUS_PYTHON_PACKAGE.to_string());
    if include_source0 && source_kind == SourceArchiveKind::Zip {
        build_requires.insert("unzip".to_string());
    }
    if source_kind == SourceArchiveKind::Git {
        build_requires.insert("git".to_string());
    }
    if r_runtime_required {
        build_requires.insert(PHOREUS_R_PACKAGE.to_string());
        // Common native stack required by many CRAN/Bioconductor graphics/text packages
        // (for example systemfonts/textshaping/ragg/ggrastr).
        build_requires.insert("cairo-devel".to_string());
        build_requires.insert("fontconfig-devel".to_string());
        build_requires.insert("freetype-devel".to_string());
        build_requires.insert("fribidi-devel".to_string());
        build_requires.insert("harfbuzz-devel".to_string());
        build_requires.insert("libjpeg-turbo-devel".to_string());
        build_requires.insert("libpng-devel".to_string());
        build_requires.insert("libtiff-devel".to_string());
        build_requires.insert("libwebp-devel".to_string());
    }
    if rust_runtime_required {
        build_requires.insert(PHOREUS_RUST_PACKAGE.to_string());
        build_requires.insert("perl".to_string());
        build_requires.insert("perl-FindBin".to_string());
    }
    if nim_runtime_required {
        build_requires.insert(PHOREUS_NIM_PACKAGE.to_string());
        build_requires.insert("git".to_string());
    }
    if software_slug == "r-monocle3" {
        // Monocle3's R dependency chain (sf/spdep/terra/units) needs geospatial
        // development headers from the base OS repositories on EL9.
        build_requires.insert("gdal-devel".to_string());
        build_requires.insert("geos-devel".to_string());
        build_requires.insert("proj-devel".to_string());
        build_requires.insert("sqlite-devel".to_string());
        build_requires.insert("udunits2-devel".to_string());
    }
    build_requires.extend(
        parsed
            .build_deps
            .iter()
            .filter(|dep| !is_conda_only_dependency(dep))
            .filter(|dep| !python_recipe || should_keep_rpm_dependency_for_python(dep))
            .map(|d| map_build_dependency(d)),
    );
    build_requires.extend(
        parsed
            .host_deps
            .iter()
            .filter(|dep| !is_conda_only_dependency(dep))
            .filter(|dep| !python_recipe || should_keep_rpm_dependency_for_python(dep))
            .map(|d| map_build_dependency(d)),
    );
    if !python_recipe && !perl_recipe && !runtime_only_metapackage {
        build_requires.extend(
            parsed
                .run_deps
                .iter()
                .filter(|dep| !is_conda_only_dependency(dep))
                .filter(|dep| {
                    !is_python_ecosystem_dependency_name(&normalize_dependency_token(dep))
                })
                .map(|d| map_build_dependency(d)),
        );
    }
    if software_slug == "igv" {
        // IGV's Gradle build enforces Java toolchain languageVersion=21.
        build_requires.remove("java-11-openjdk");
        build_requires.insert("java-21-openjdk-devel".to_string());
    }
    if software_slug == "spades" {
        // SPAdes pulls ncbi_vdb_ext via ExternalProject git clone at configure time.
        build_requires.insert("git".to_string());
    }

    let mut runtime_requires = BTreeSet::new();
    runtime_requires.insert("phoreus".to_string());
    if python_recipe {
        runtime_requires.insert(PHOREUS_PYTHON_PACKAGE.to_string());
        if r_runtime_required {
            runtime_requires.insert(PHOREUS_R_PACKAGE.to_string());
        }
        runtime_requires.extend(
            parsed
                .run_deps
                .iter()
                .filter(|dep| !is_conda_only_dependency(dep))
                .filter(|dep| should_keep_rpm_dependency_for_python(dep))
                .map(|d| map_runtime_dependency(d)),
        );
    } else {
        if r_runtime_required {
            runtime_requires.insert(PHOREUS_R_PACKAGE.to_string());
        }
        runtime_requires.extend(
            parsed
                .run_deps
                .iter()
                .filter(|dep| !is_conda_only_dependency(dep))
                .map(|d| map_runtime_dependency(d)),
        );
    }
    if software_slug == "igv" {
        runtime_requires.remove("java-11-openjdk");
        runtime_requires.insert("java-21-openjdk".to_string());
    }

    let build_requires_lines = format_dep_lines("BuildRequires", &build_requires);
    let requires_lines = format_dep_lines("Requires", &runtime_requires);
    let source0_line = if include_source0 {
        format!("Source0:        {source_url}\n")
    } else {
        String::new()
    };
    let source_git_macros = if let Some((url, rev)) = git_source.as_ref() {
        format!(
            "%global bioconda_source_git_url {}\n%global bioconda_source_git_rev {}\n",
            spec_escape(url),
            spec_escape(rev)
        )
    } else {
        String::new()
    };
    let patch_source_lines = render_patch_source_lines(staged_patch_sources);
    let patch_apply_lines =
        render_patch_apply_lines(staged_patch_sources, "%{bioconda_source_subdir}");
    let changelog_date = rpm_changelog_date();
    let build_arch_line = if noarch_python && !python_recipe {
        "BuildArch:      noarch\n".to_string()
    } else {
        String::new()
    };

    format!(
        "%global debug_package %{{nil}}\n\
    %global __brp_mangle_shebangs %{{nil}}\n\
    \n\
    %global tool {tool}\n\
    %global upstream_version {version}\n\
    %global bioconda_source_subdir {source_subdir}\n\
    %global bioconda_source_relsubdir {source_relsubdir}\n\
    {source_git_macros}\
    \n\
    Name:           phoreus-%{{tool}}-%{{upstream_version}}\n\
    Version:        %{{upstream_version}}\n\
    Release:        1%{{?dist}}\n\
    Provides:       %{{tool}} = %{{version}}-%{{release}}\n\
    Summary:        {summary}\n\
    License:        {license}\n\
    URL:            {homepage}\n\
    {build_arch}\
    {source0_line}\
    Source1:        {build_sh}\n\
    {patch_sources}\n\
    {build_requires}\n\
    {requires}\n\
    %global phoreus_prefix /usr/local/phoreus/%{{tool}}/%{{version}}\n\
    %global phoreus_moddir /usr/local/phoreus/modules/%{{tool}}\n\
    \n\
    %description\n\
    Auto-generated from Bioconda metadata only.\n\
    Recipe metadata source: {meta_path}\n\
    Variant selected: {variant_dir}\n\
    \n\
    %prep\n\
    {source_unpack_prep}\
    cp %{{SOURCE1}} buildsrc/build.sh\n\
    chmod 0755 buildsrc/build.sh\n\
    {patch_apply}\
    \n\
    %build\n\
    cd buildsrc\n\
    %ifarch aarch64\n\
    export BIOCONDA_TARGET_ARCH=aarch64\n\
    export target_platform=linux-aarch64\n\
    %else\n\
    export BIOCONDA_TARGET_ARCH=x86_64\n\
    export target_platform=linux-64\n\
    %endif\n\
    export CPU_COUNT=1\n\
    export MAKEFLAGS=-j1\n\
    \n\
    %install\n\
    rm -rf %{{buildroot}}\n\
    mkdir -p %{{buildroot}}%{{phoreus_prefix}}\n\
    cd buildsrc\n\
    %ifarch aarch64\n\
    export BIOCONDA_TARGET_ARCH=aarch64\n\
    export target_platform=linux-aarch64\n\
    %else\n\
    export BIOCONDA_TARGET_ARCH=x86_64\n\
    export target_platform=linux-64\n\
    %endif\n\
    export PREFIX=%{{buildroot}}%{{phoreus_prefix}}\n\
    export SRC_DIR=$(pwd)/%{{bioconda_source_relsubdir}}\n\
    export CPU_COUNT=1\n\
    export MAKEFLAGS=-j1\n\
    export CMAKE_BUILD_PARALLEL_LEVEL=1\n\
    export NINJAFLAGS=-j1\n\
    \n\
    # Compatibility shim for the legacy BLAST 2.5.0 configure parser.\n\
    # Its NCBI configure script cannot parse modern two-digit GCC majors.\n\
    if [[ \"%{{tool}}\" == \"blast\" ]]; then\n\
    real_gcc=$(command -v gcc || true)\n\
    real_gxx=$(command -v g++ || true)\n\
    wrap_dir=\"$(pwd)/.bioconda2rpm-toolchain-wrap\"\n\
    rm -rf \"$wrap_dir\"\n\
    mkdir -p \"$wrap_dir\"\n\
    if [[ -n \"$real_gcc\" ]]; then\n\
    cat > \"$wrap_dir/gcc\" <<'EOF'\n\
    #!/usr/bin/env bash\n\
    real=\"__BIOCONDA2RPM_REAL_GCC__\"\n\
    if [[ \"${{1:-}}\" == \"-dumpversion\" ]]; then\n\
    ver=\"$($real -dumpfullversion 2>/dev/null || $real -dumpversion 2>/dev/null || echo 9.0.0)\"\n\
    major=\"$(printf '%s' \"$ver\" | cut -d. -f1)\"\n\
    if [[ \"$major\" =~ ^[0-9]+$ ]] && (( major >= 10 )); then\n\
    rest=\"${{ver#*.}}\"\n\
    if [[ \"$rest\" == \"$ver\" ]]; then\n\
      ver=\"9.0.0\"\n\
    else\n\
      ver=\"9.${{rest}}\"\n\
    fi\n\
    fi\n\
    printf '%s\\n' \"$ver\"\n\
    exit 0\n\
    fi\n\
    exec \"$real\" \"$@\"\n\
    EOF\n\
    sed -i \"s|__BIOCONDA2RPM_REAL_GCC__|$real_gcc|g\" \"$wrap_dir/gcc\"\n\
    chmod 0755 \"$wrap_dir/gcc\"\n\
    fi\n\
    if [[ -n \"$real_gxx\" ]]; then\n\
    cat > \"$wrap_dir/g++\" <<'EOF'\n\
    #!/usr/bin/env bash\n\
    real=\"__BIOCONDA2RPM_REAL_GXX__\"\n\
    if [[ \"${{1:-}}\" == \"-dumpversion\" ]]; then\n\
    ver=\"$($real -dumpfullversion 2>/dev/null || $real -dumpversion 2>/dev/null || echo 9.0.0)\"\n\
    major=\"$(printf '%s' \"$ver\" | cut -d. -f1)\"\n\
    if [[ \"$major\" =~ ^[0-9]+$ ]] && (( major >= 10 )); then\n\
    rest=\"${{ver#*.}}\"\n\
    if [[ \"$rest\" == \"$ver\" ]]; then\n\
      ver=\"9.0.0\"\n\
    else\n\
      ver=\"9.${{rest}}\"\n\
    fi\n\
    fi\n\
    printf '%s\\n' \"$ver\"\n\
    exit 0\n\
    fi\n\
    exec \"$real\" \"$@\"\n\
    EOF\n\
    sed -i \"s|__BIOCONDA2RPM_REAL_GXX__|$real_gxx|g\" \"$wrap_dir/g++\"\n\
    chmod 0755 \"$wrap_dir/g++\"\n\
    fi\n\
    export PATH=\"$wrap_dir:$PATH\"\n\
    fi\n\
    \n\
    export CC=${{CC:-gcc}}\n\
    export CXX=${{CXX:-g++}}\n\
    # Some Bioconda R recipes write FC/F77 directly into ~/.R/Makevars.\n\
    # Keep deterministic defaults so Fortran compilation never falls back to\n\
    # an empty command token when gcc-gfortran is present in dependencies.\n\
    if command -v gfortran >/dev/null 2>&1; then\n\
    export FC=\"${{FC:-gfortran}}\"\n\
    export F77=\"${{F77:-gfortran}}\"\n\
    fi\n\
    export CFLAGS=\"${{CFLAGS:-}}\"\n\
    export CXXFLAGS=\"${{CXXFLAGS:-}}\"\n\
    export CPPFLAGS=\"${{CPPFLAGS:-}}\"\n\
    export LDFLAGS=\"${{LDFLAGS:-}}\"\n\
    export AR=\"${{AR:-ar}}\"\n\
    export STRIP=\"${{STRIP:-strip}}\"\n\
    \n\
    # Canonical Python toolchain for Phoreus builds: never rely on system Python.\n\
    export PHOREUS_PYTHON_PREFIX=/usr/local/phoreus/python/{phoreus_python_version}\n\
    if [[ ! -x \"$PHOREUS_PYTHON_PREFIX/bin/python{phoreus_python_version}\" ]]; then\n\
    echo \"missing Phoreus Python runtime at $PHOREUS_PYTHON_PREFIX\" >&2\n\
    exit 41\n\
    fi\n\
    export PATH=\"$PHOREUS_PYTHON_PREFIX/bin:$PATH\"\n\
    export LD_LIBRARY_PATH=\"$PHOREUS_PYTHON_PREFIX/lib${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\"\n\
    export PYTHON=\"$PHOREUS_PYTHON_PREFIX/bin/python{phoreus_python_version}\"\n\
    export PYTHON3=\"$PHOREUS_PYTHON_PREFIX/bin/python{phoreus_python_version}\"\n\
    export PIP=\"$PHOREUS_PYTHON_PREFIX/bin/pip{phoreus_python_version}\"\n\
    export PYTHONNOUSERSITE=1\n\
    export RECIPE_DIR=/work/SOURCES\n\
    export PKG_NAME=\"${{PKG_NAME:-{conda_pkg_name}}}\"\n\
    export PKG_VERSION=\"${{PKG_VERSION:-{conda_pkg_version}}}\"\n\
    export PKG_BUILDNUM=\"${{PKG_BUILDNUM:-{conda_pkg_build_number}}}\"\n\
    export PKG_BUILD_STRING=\"${{PKG_BUILD_STRING:-${{PKG_BUILDNUM}}}}\"\n\
# Some autotools recipes abort if CONFIG_SITE is the literal token NONE.\n\
    if [[ \"${{CONFIG_SITE:-}}\" == \"NONE\" ]]; then\n\
    unset CONFIG_SITE\n\
    fi\n\
    export PERL_MM_OPT=\"${{PERL_MM_OPT:+$PERL_MM_OPT }}INSTALL_BASE=$PREFIX\"\n\
    export PERL_MB_OPT=\"${{PERL_MB_OPT:+$PERL_MB_OPT }}--install_base $PREFIX\"\n\
    \n\
    # Prefer Autoconf 2.71 toolchain when present (EL9 autoconf271 package).\n\
    if [[ -x /opt/rh/autoconf271/bin/autoconf ]]; then\n\
    export PATH=\"/opt/rh/autoconf271/bin:$PATH\"\n\
    fi\n\
    \n\
    # EL9 OpenMPI installs wrappers and pkg-config files in a non-default prefix.\n\
    # Surface them so CMake/Autotools recipes can discover MPI consistently.\n\
    if [[ -d /usr/lib64/openmpi/bin ]]; then\n\
    export PATH=\"/usr/lib64/openmpi/bin:$PATH\"\n\
    fi\n\
    if [[ -d /usr/lib64/openmpi/include ]]; then\n\
    export CPATH=\"/usr/lib64/openmpi/include${{CPATH:+:$CPATH}}\"\n\
    fi\n\
    if [[ -d /usr/lib64/openmpi/lib ]]; then\n\
    export LIBRARY_PATH=\"/usr/lib64/openmpi/lib${{LIBRARY_PATH:+:$LIBRARY_PATH}}\"\n\
    export LD_LIBRARY_PATH=\"/usr/lib64/openmpi/lib${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\"\n\
    export PKG_CONFIG_PATH=\"/usr/lib64/openmpi/lib/pkgconfig${{PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}}\"\n\
    fi\n\
\n\
# Make locally installed Phoreus Perl dependency trees visible during build.\n\
if [[ -d /usr/local/phoreus ]]; then\n\
while IFS= read -r -d '' perl_lib; do\n\
  case \":${{PERL5LIB:-}}:\" in\n\
    *\":$perl_lib:\"*) ;;\n\
    *) export PERL5LIB=\"$perl_lib${{PERL5LIB:+:$PERL5LIB}}\" ;;\n\
    esac\n\
  case \" ${{PERL5OPT:-}} \" in\n\
    *\" -I$perl_lib \"*) ;;\n\
    *) export PERL5OPT=\"${{PERL5OPT:+$PERL5OPT }}-I$perl_lib\" ;;\n\
  esac\n\
done < <(find /usr/local/phoreus -maxdepth 6 -type d \\( -path '*/lib/perl5' -o -path '*/lib64/perl5' \\) -print0 2>/dev/null)\n\
fi\n\
\n\
# Expose include/lib/pkg-config roots from already-installed Phoreus payloads\n\
# so dependent recipes can resolve headers and link targets without conda-style\n\
# shared PREFIX assumptions.\n\
if [[ -d /usr/local/phoreus ]]; then\n\
while IFS= read -r -d '' dep_include; do\n\
  case \":${{CPATH:-}}:\" in\n\
    *\":$dep_include:\"*) ;;\n\
    *) export CPATH=\"$dep_include${{CPATH:+:$CPATH}}\" ;;\n\
  esac\n\
done < <(find /usr/local/phoreus -mindepth 3 -maxdepth 3 -type d -name include -print0 2>/dev/null)\n\
while IFS= read -r -d '' dep_lib; do\n\
  case \":${{LIBRARY_PATH:-}}:\" in\n\
    *\":$dep_lib:\"*) ;;\n\
    *) export LIBRARY_PATH=\"$dep_lib${{LIBRARY_PATH:+:$LIBRARY_PATH}}\" ;;\n\
  esac\n\
  case \":${{LD_LIBRARY_PATH:-}}:\" in\n\
    *\":$dep_lib:\"*) ;;\n\
    *) export LD_LIBRARY_PATH=\"$dep_lib${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\" ;;\n\
  esac\n\
  case \" ${{LDFLAGS:-}} \" in\n\
    *\" -L$dep_lib \"*) ;;\n\
    *) export LDFLAGS=\"-L$dep_lib ${{LDFLAGS:-}}\" ;;\n\
  esac\n\
done < <(find /usr/local/phoreus -mindepth 3 -maxdepth 3 -type d -name lib -print0 2>/dev/null)\n\
while IFS= read -r -d '' dep_pc; do\n\
  case \":${{PKG_CONFIG_PATH:-}}:\" in\n\
    *\":$dep_pc:\"*) ;;\n\
    *) export PKG_CONFIG_PATH=\"$dep_pc${{PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}}\" ;;\n\
  esac\n\
done < <(find /usr/local/phoreus -maxdepth 6 -type d -name pkgconfig -print0 2>/dev/null)\n\
fi\n\
\n\
# Ensure common install subdirectories exist for build.sh scripts that assume them.\n\
mkdir -p \"$PREFIX/lib\" \"$PREFIX/bin\" \"$PREFIX/include\"\n\
export BUILD_PREFIX=\"${{BUILD_PREFIX:-$PREFIX}}\"\n\
mkdir -p \"$BUILD_PREFIX/share/gnuconfig\"\n\
if [[ ! -f \"$BUILD_PREFIX/share/gnuconfig/config.guess\" || ! -f \"$BUILD_PREFIX/share/gnuconfig/config.sub\" ]]; then\n\
  cfg_dir=$(find /usr/share -maxdepth 4 -type f -name config.guess -print 2>/dev/null | head -n 1 | xargs -r dirname)\n\
  if [[ -n \"$cfg_dir\" && -f \"$cfg_dir/config.guess\" && -f \"$cfg_dir/config.sub\" ]]; then\n\
    cp -f \"$cfg_dir/config.guess\" \"$BUILD_PREFIX/share/gnuconfig/config.guess\" || true\n\
    cp -f \"$cfg_dir/config.sub\" \"$BUILD_PREFIX/share/gnuconfig/config.sub\" || true\n\
  fi\n\
fi\n\
\n\
# Conda recipes often assume host/build dependencies are co-located in one PREFIX.\n\
# Phoreus keeps dependencies in versioned prefixes, so stage compatibility symlinks.\n\
if [[ -d /usr/local/phoreus ]]; then\n\
while IFS= read -r -d '' dep_include; do\n\
  for entry in \"$dep_include\"/*; do\n\
    [[ -e \"$entry\" ]] || continue\n\
    target=\"$PREFIX/include/$(basename \"$entry\")\"\n\
    [[ -e \"$target\" ]] && continue\n\
    ln -snf \"$entry\" \"$target\" || true\n\
  done\n\
done < <(find /usr/local/phoreus -mindepth 3 -maxdepth 3 -type d -name include -print0 2>/dev/null)\n\
while IFS= read -r -d '' dep_lib; do\n\
  for lib in \"$dep_lib\"/*; do\n\
    [[ -e \"$lib\" ]] || continue\n\
    target=\"$PREFIX/lib/$(basename \"$lib\")\"\n\
    [[ -e \"$target\" ]] && continue\n\
    ln -snf \"$lib\" \"$target\" || true\n\
  done\n\
done < <(find /usr/local/phoreus -mindepth 3 -maxdepth 3 -type d -name lib -print0 2>/dev/null)\n\
fi\n\
\n\
# EL9 ships some HDF5 headers under /usr/include/hdf5/serial.\n\
if [[ -d /usr/include/hdf5/serial ]]; then\n\
  export CPPFLAGS=\"-I/usr/include/hdf5/serial ${{CPPFLAGS:-}}\"\n\
fi\n\
if [[ -f /usr/include/H5pubconf-32.h && ! -e \"$PREFIX/include/H5pubconf.h\" ]]; then\n\
  ln -snf /usr/include/H5pubconf-32.h \"$PREFIX/include/H5pubconf.h\"\n\
fi\n\
    \n\
{python_venv_setup}\
\n\
{r_runtime_setup}\
\n\
{rust_runtime_setup}\
\n\
{nim_runtime_setup}\
\n\
# BLAST recipes in Bioconda assume a conda-style shared prefix where ncbi-vdb\n\
    # lives under the same PREFIX. In Phoreus, ncbi-vdb is a separate payload.\n\
    # Retarget the generated build.sh argument to the newest installed ncbi-vdb prefix.\n\
    if [[ \"%{{tool}}\" == \"blast\" ]]; then\n\
    # BLAST 2.5.0 source is not compatible with EL9 Boost.Test API updates.\n\
    # Force configure cache to treat Boost.Test as unavailable so test_boost.cpp\n\
    # is not compiled as part of the payload build graph.\n\
    export ncbi_cv_lib_boost_test=no\n\
    vdb_prefix=$(find /usr/local/phoreus/ncbi-vdb -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort | tail -n 1 || true)\n\
    if [[ -n \"$vdb_prefix\" ]]; then\n\
    sed -i 's|--with-vdb=$PREFIX|--with-vdb='\\\"$vdb_prefix\\\"'|g' ./build.sh\n\
    fi\n\
    # BLAST's Bioconda script hard-codes n_workers=8 on aarch64, which has shown\n\
    # unstable flat-make behavior in containerized RPM builds. Enforce single-core\n\
    # policy so the orchestrator remains deterministic across all architectures.\n\
    export CPU_COUNT=1\n\
    sed -i 's|n_workers=8|n_workers=${{CPU_COUNT:-1}}|g' ./build.sh\n\
    # Bioconda's BLAST script removes a temporary linker path with plain `rm`,\n\
    # but in our staged prefix this path may be materialized as a directory.\n\
    # Ensure cleanup succeeds for both symlink and directory forms.\n\
    sed -i 's|^rm \"\\$LIB_INSTALL_DIR\"$|rm -rf \"\\$LIB_INSTALL_DIR\"|g' ./build.sh\n\
    # Newer BLAST source trees can ship subdirectories (for example `lib/outside`).\n\
    # Bioconda's flat `cp $RESULT_PATH/lib/*` fails when a directory is present.\n\
    # Use recursive copy semantics so payload installation remains stable.\n\
    sed -i 's|^cp \\$RESULT_PATH/lib/\\* \"\\$LIB_INSTALL_DIR\"$|cp -r \\$RESULT_PATH/lib/* \"\\$LIB_INSTALL_DIR\"|g' ./build.sh\n\
    fi\n\
    \n\
    # GMAP upstream release tarballs already ship generated configure scripts.\n\
    # Running autoreconf on EL9 toolchains has produced broken configure outputs\n\
    # for recent GMAP snapshots; prefer the bundled configure script as Bioconda does.\n\
    if [[ \"%{{tool}}\" == \"gmap\" ]]; then\n\
    sed -i 's|^autoreconf -if$|# autoreconf -if (disabled by bioconda2rpm for EL9 compatibility)|g' ./build.sh || true\n\
    sed -i 's|export LC_ALL=\"en_US.UTF-8\"|export LC_ALL=C|g' ./build.sh || true\n\
    fi\n\
    \n\
    # UCSC userApps build system can force -liconv, but EL9/glibc toolchains do\n\
    # not ship a standalone libiconv and provide iconv via libc. Drop -liconv\n\
    # from generated make fragments for deterministic Linux builds.\n\
    if [[ \"%{{tool}}\" == \"ucsc-bigwigsummary\" ]]; then\n\
    find kent/src -type f \\( -name '*.mk' -o -name makefile \\) | while read -r mk; do\n\
      sed -i 's/[[:space:]]-liconv//g' \"$mk\" || true\n\
    done\n\
    fi\n\
    \n\
    # Samtools recipes often request --with-htslib=system, but in this workflow\n\
    # HTSlib is provided by the versioned Phoreus prefix rather than /usr.\n\
    # Rewrite the configure target and inject matching include/lib/pkg-config flags.\n\
    if [[ \"%{{tool}}\" == \"samtools\" ]]; then\n\
    hts_prefix=$(find /usr/local/phoreus/htslib -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort | tail -n 1 || true)\n\
    if [[ -n \"$hts_prefix\" ]]; then\n\
    sed -i \"s|--with-htslib=system|--with-htslib=$hts_prefix|g\" ./build.sh || true\n\
    export CPPFLAGS=\"-I$hts_prefix/include ${{CPPFLAGS:-}}\"\n\
    export LDFLAGS=\"-L$hts_prefix/lib ${{LDFLAGS:-}}\"\n\
    export PKG_CONFIG_PATH=\"$hts_prefix/lib/pkgconfig${{PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}}\"\n\
    fi\n\
    # Ensure CURSES_LIB is passed as an environment assignment to configure.\n\
    sed -i 's|^\\./configure |CURSES_LIB=\"$CURSES_LIB\" ./configure |' ./build.sh || true\n\
    # Normalize Bioconda's conda-oriented wide-curses flags for EL9 toolchains.\n\
    if ! ldconfig -p 2>/dev/null | grep -q 'libtinfow\\\\.so'; then\n\
    sed -i 's|-ltinfow|-ltinfo|g' ./build.sh || true\n\
    fi\n\
    if ! ldconfig -p 2>/dev/null | grep -q 'libncursesw\\\\.so'; then\n\
    sed -i 's|-lncursesw|-lncurses|g' ./build.sh || true\n\
    fi\n\
    fi\n\
    \n\
    # STAR can hit container OOM/SIGKILL during final link on constrained hosts.\n\
    # Keep canonical single-core policy but also reduce memory pressure.\n\
    if [[ \"%{{tool}}\" == \"star\" ]]; then\n\
    sed -i 's/-O3/-O1/g' ./build.sh || true\n\
    sed -i 's/-march=armv8-a//g' ./build.sh || true\n\
    sed -i 's/-march=armv8.4-a//g' ./build.sh || true\n\
    sed -i 's/-march=x86-64-v3//g' ./build.sh || true\n\
    export LDFLAGS=\"-Wl,--no-keep-memory ${{LDFLAGS:-}}\"\n\
    fi\n\
    \n\
    # RNA-SeQC v2.4.2 bundles a BWA fork that hard-requires x86 SSE headers\n\
    # (emmintrin.h) and is not portable to Linux/aarch64 in this release.\n\
    if [[ \"%{{tool}}\" == \"rna-seqc\" && \"${{BIOCONDA_TARGET_ARCH:-}}\" == \"aarch64\" ]]; then\n\
    echo \"rna-seqc upstream source requires x86 SSE (emmintrin.h); no Linux/aarch64 build path in this release\" >&2\n\
    exit 86\n\
    fi\n\
    \n\
    # Bcftools recipes often pass GSL_LIBS=-lgsl only, but EL9's GSL\n\
    # requires explicit CBLAS linkage at link time.\n\
    if [[ \"%{{tool}}\" == \"bcftools\" ]]; then\n\
    sed -i 's|GSL_LIBS=-lgsl|GSL_LIBS=\"-lgsl -lopenblas\"|g' ./build.sh || true\n\
    fi\n\
    \n\
    # EBSeq currently pulls modern BH headers that require at least C++14.\n\
    # Some recipe scripts only wire CXX/CXX11 without an explicit std level,\n\
    # which can fall back to older defaults on EL9 and fail in Boost headers.\n\
    if [[ \"%{{tool}}\" == \"bioconductor-ebseq\" ]]; then\n\
    export CXX=\"${{CXX:-g++}} -std=gnu++14\"\n\
    export CXXFLAGS=\"-std=gnu++14 ${{CXXFLAGS:-}}\"\n\
    export CXX11=\"${{CXX11:-${{CXX:-g++}} -std=gnu++14}}\"\n\
    export CXX14=\"${{CXX14:-${{CXX:-g++}} -std=gnu++14}}\"\n\
    sed -i 's|^CXX11=\\$CXX$|CXX11=$CXX -std=gnu++14|g' ./build.sh || true\n\
    sed -i 's|^CXX14=\\$CXX$|CXX14=$CXX -std=gnu++14|g' ./build.sh || true\n\
    if [[ -f src/Makevars ]]; then\n\
    if ! grep -q '^CXX_STD[[:space:]]*=' src/Makevars; then\n\
    printf '\\nCXX_STD = CXX14\\n' >> src/Makevars\n\
    fi\n\
    fi\n\
    fi\n\
    \n\
    # Salmon's Bioconda recipe forces Boost lookup to $PREFIX only.\n\
    # In RPM builds we install boost-devel via system repos, so allow\n\
    # standard CMake discovery roots to satisfy Boost components.\n\
    if [[ \"%{{tool}}\" == \"salmon\" ]]; then\n\
    sed -i 's|-DBOOST_ROOT=\"${{PREFIX}}\"|-DBOOST_ROOT=\"/usr\"|g' ./build.sh || true\n\
    sed -i 's|-DBoost_NO_SYSTEM_PATHS=ON|-DBoost_NO_SYSTEM_PATHS=OFF|g' ./build.sh || true\n\
    # Salmon's libstaden ExternalProject hardcodes ${{BUILD_PREFIX}}/share/gnuconfig.\n\
    # Ensure a stable system path exists and rewrite to it for non-conda RPM builds.\n\
    cfg_dir=$(find /usr/share -maxdepth 3 -type f -name config.guess -print 2>/dev/null | head -n 1 | xargs -r dirname)\n\
    if [[ -n \"$cfg_dir\" && -f \"$cfg_dir/config.guess\" && -f \"$cfg_dir/config.sub\" ]]; then\n\
      mkdir -p /usr/share/gnuconfig\n\
      cp -f \"$cfg_dir/config.guess\" /usr/share/gnuconfig/config.guess\n\
      cp -f \"$cfg_dir/config.sub\" /usr/share/gnuconfig/config.sub\n\
      sed -i 's|${{BUILD_PREFIX}}/share/gnuconfig|/usr/share/gnuconfig|g' CMakeLists.txt || true\n\
      perl -0pi -e 's@\\n\\s*cp .*gnuconfig.*\\n@\\n      cp -f /usr/share/gnuconfig/config.guess staden-io_lib/config.guess &&\\n      cp -f /usr/share/gnuconfig/config.sub staden-io_lib/config.sub &&\\n@' CMakeLists.txt || true\n\
      sed -i 's|set(JEMALLOC_FLAGS \"CC=${{CMAKE_C_COMPILER}} CFLAGS=\\\\\\\"-fPIC ${{SCHAR_FLAG}}\\\\\\\" CPPFLAGS=\\\\\\\"-fPIC ${{SCHAR_FLAG}}\\\\\\\"\")|set(JEMALLOC_FLAGS \"CC=${{CMAKE_C_COMPILER}} CFLAGS=-fPIC CPPFLAGS=-fPIC\")|g' CMakeLists.txt || true\n\
      perl -0pi -e 's@if\\(CONDA_BUILD\\)\\n\\s*set\\(JEMALLOC_FLAGS .*?\\nelse\\(\\)\\n\\s*set\\(JEMALLOC_FLAGS .*?\\nendif\\(\\)@set(JEMALLOC_FLAGS \"CC=${{CMAKE_C_COMPILER}} CFLAGS=-fPIC CPPFLAGS=-fPIC\")@s' CMakeLists.txt || true\n\
    fi\n\
    fi\n\
    \n\
    # IGV Gradle builds require Java 21 toolchain resolution.
    # Prefer the packaged EL9 JDK location and make it explicit for Gradle.
    if [[ \"%{{tool}}\" == \"igv\" ]]; then\n\
    if [[ -d /usr/lib/jvm/java-21-openjdk ]]; then\n\
      export JAVA_HOME=/usr/lib/jvm/java-21-openjdk\n\
      export PATH=\"$JAVA_HOME/bin:$PATH\"\n\
      export ORG_GRADLE_JAVA_HOME=\"$JAVA_HOME\"\n\
    fi\n\
    fi\n\
    \n\
    # Many conda build scripts set en_US.UTF-8 explicitly, but minimal EL9\n\
    # containers may not generate that locale. Normalize to C to avoid\n\
    # noisy failures in shell/R startup locale checks.\n\
    sed -i 's|export LC_ALL=\"en_US.UTF-8\"|export LC_ALL=C|g' ./build.sh || true\n\
    \n\
    # A number of upstream scripts hardcode aggressive THREADS values;\n\
    # force single-core policy for deterministic container builds.\n\
    sed -i -E 's/THREADS=\"-j[0-9]+\"/THREADS=\"-j1\"/g' ./build.sh || true\n\
    \n\
    # Capture a pristine buildsrc snapshot so serial retries run from a clean tree,\n\
    # not from a partially mutated/failed first attempt.\n\
    chmod -R u+rwX . 2>/dev/null || true\n\
    retry_snapshot=\"$(pwd)/.bioconda2rpm-retry-snapshot.tar\"\n\
    rm -f \"$retry_snapshot\"\n\
    tar --exclude='.bioconda2rpm-retry-snapshot.tar' -cf \"$retry_snapshot\" .\n\
    \n\
    # Canonical fallback for flaky parallel builds: retry once serially.\n\
    # Enforce fail-fast shell behavior for staged recipe scripts so downstream\n\
    # commands do not mask the primary failure reason.\n\
    if bash -eo pipefail ./build.sh; then\n\
    :\n\
    else\n\
    rc=$?\n\
    # Do not retry deterministic policy failures (missing pinned runtimes,\n\
    # unsupported precompiled binary arch, missing precompiled payload).\n\
    if [[ \"$rc\" == \"41\" || \"$rc\" == \"42\" || \"$rc\" == \"43\" || \"$rc\" == \"44\" || \"$rc\" == \"86\" || \"$rc\" == \"87\" ]]; then\n\
    exit \"$rc\"\n\
    fi\n\
    if [[ \"${{BIOCONDA2RPM_RETRIED_SERIAL:-0}}\" == \"1\" ]]; then\n\
    exit 1\n\
    fi\n\
    export BIOCONDA2RPM_RETRIED_SERIAL=1\n\
    export CPU_COUNT=1\n\
    export MAKEFLAGS=-j1\n\
    find . -mindepth 1 -maxdepth 1 ! -name \"$(basename \"$retry_snapshot\")\" -exec rm -rf {{}} +\n\
    tar -xf \"$retry_snapshot\"\n\
    bash -eo pipefail ./build.sh\n\
    fi\n\
    rm -f \"$retry_snapshot\"\n\
    \n\
    # Some Bioconda build scripts emit absolute symlinks into %{{buildroot}}.\n\
    # Rewrite those targets so RPM payload validation does not see buildroot leaks.\n\
    while IFS= read -r -d '' link_path; do\n\
    link_target=$(readlink \"$link_path\" || true)\n\
    case \"$link_target\" in\n\
    %{{buildroot}}/*)\n\
      fixed_target=\"${{link_target#%{{buildroot}}}}\"\n\
      ln -snf \"$fixed_target\" \"$link_path\"\n\
      ;;\n\
    esac\n\
    done < <(find %{{buildroot}}%{{phoreus_prefix}} -type l -print0 2>/dev/null)\n\
    \n\
    # Python virtualenv and some installers may record temporary buildroot prefixes\n\
    # in script shebangs/config files; rewrite to final install prefix for RPM checks.\n\
    buildroot_prefix=\"%{{buildroot}}%{{phoreus_prefix}}\"\n\
    final_prefix=\"%{{phoreus_prefix}}\"\n\
    while IFS= read -r -d '' text_path; do\n\
    sed -i \"s|$buildroot_prefix|$final_prefix|g\" \"$text_path\" || true\n\
    done < <(grep -RIlZ -- \"$buildroot_prefix\" %{{buildroot}}%{{phoreus_prefix}} 2>/dev/null || true)\n\
    \n\
    # Perl installs often emit perllocal.pod entries that embed buildroot paths.\n\
    # Drop those files to satisfy RPM check-buildroot validation.\n\
    find %{{buildroot}}%{{phoreus_prefix}} -type f -name perllocal.pod -delete 2>/dev/null || true\n\
    \n\
    mkdir -p %{{buildroot}}%{{phoreus_moddir}}\n\
    cat > %{{buildroot}}%{{phoreus_moddir}}/%{{version}}.lua <<'LUAEOF'\n\
    help([[ {summary} ]])\n\
    whatis(\"Name: {tool}\")\n\
    whatis(\"Version: {version}\")\n\
    whatis(\"URL: {homepage}\")\n\
    local prefix = \"/usr/local/phoreus/{tool}/{version}\"\n\
    {module_lua_env}\
    LUAEOF\n\
    chmod 0644 %{{buildroot}}%{{phoreus_moddir}}/%{{version}}.lua\n\
    \n\
    %files\n\
    %{{phoreus_prefix}}/\n\
    %{{phoreus_moddir}}/%{{version}}.lua\n\
    \n\
    %changelog\n\
    * {changelog_date} bioconda2rpm <packaging@bioconda2rpm.local> - {version}-1\n\
    - Auto-generated from Bioconda metadata and build.sh\n",
        tool = software_slug,
        version = spec_escape(&parsed.version),
        source_subdir = spec_escape(&source_subdir),
        source_relsubdir = spec_escape(&source_relsubdir),
        source_git_macros = source_git_macros,
        summary = summary,
        license = license,
        homepage = homepage,
        source0_line = source0_line,
        build_sh = spec_escape(staged_build_sh_name),
        patch_sources = patch_source_lines,
        patch_apply = patch_apply_lines,
        source_unpack_prep = source_unpack_prep,
        build_requires = build_requires_lines,
        requires = requires_lines,
        build_arch = build_arch_line,
        python_venv_setup = python_venv_setup,
        module_lua_env = module_lua_env,
        changelog_date = changelog_date,
        meta_path = spec_escape(&meta_path.display().to_string()),
        variant_dir = spec_escape(&variant_dir.display().to_string()),
        phoreus_python_version = PHOREUS_PYTHON_VERSION,
        conda_pkg_name = spec_escape(&parsed.package_name),
        conda_pkg_version = spec_escape(&parsed.version),
        conda_pkg_build_number = spec_escape(&parsed.build_number),
        r_runtime_setup = r_runtime_setup,
        rust_runtime_setup = rust_runtime_setup,
        nim_runtime_setup = nim_runtime_setup,
    )
}

fn synthesize_fallback_build_sh(parsed: &ParsedMeta) -> Option<String> {
    let package = normalize_name(&parsed.package_name);
    if package == "r"
        || package == "r-base"
        || package.starts_with("r-")
        || package.starts_with("bioconductor-")
    {
        return Some(
            "#!/usr/bin/env bash\n\
set -euxo pipefail\n\
\"$R\" CMD INSTALL --build .\n"
                .to_string(),
        );
    }
    if is_runtime_only_metapackage(parsed) {
        return Some(
            "#!/usr/bin/env bash\n\
set -euxo pipefail\n\
echo \"bioconda2rpm metapackage fallback: no payload build steps required\"\n"
                .to_string(),
        );
    }
    None
}

fn is_runtime_only_metapackage(parsed: &ParsedMeta) -> bool {
    parsed.build_deps.is_empty()
        && parsed.host_deps.is_empty()
        && !parsed.run_deps.is_empty()
        && parsed.source_url.trim().is_empty()
}

fn parse_git_source_descriptor(source_url: &str) -> Option<(String, String)> {
    let raw = source_url.trim();
    let remainder = raw.strip_prefix("git+")?;
    let (url, rev) = remainder.split_once('#')?;
    let url = url.trim().to_string();
    let rev = rev.trim().to_string();
    if url.is_empty() || rev.is_empty() {
        return None;
    }
    Some((url, rev))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceArchiveKind {
    Tar,
    Zip,
    File,
    Git,
}

fn source_archive_kind(source_url: &str) -> SourceArchiveKind {
    if source_url.trim().starts_with("git+") {
        return SourceArchiveKind::Git;
    }
    let lowered = source_url.trim().to_ascii_lowercase();
    let trimmed = lowered
        .split_once('?')
        .map(|(base, _)| base)
        .unwrap_or(lowered.as_str());
    let trimmed = trimmed
        .split_once('#')
        .map(|(base, _)| base)
        .unwrap_or(trimmed);
    if trimmed.ends_with(".zip") {
        return SourceArchiveKind::Zip;
    }
    if trimmed.ends_with(".tar")
        || trimmed.ends_with(".tar.gz")
        || trimmed.ends_with(".tgz")
        || trimmed.ends_with(".tar.bz2")
        || trimmed.ends_with(".tbz")
        || trimmed.ends_with(".tbz2")
        || trimmed.ends_with(".tar.xz")
        || trimmed.ends_with(".txz")
        || trimmed.ends_with(".tar.zst")
        || trimmed.ends_with(".tzst")
    {
        return SourceArchiveKind::Tar;
    }
    SourceArchiveKind::File
}

fn render_source_unpack_prep_block(source_kind: SourceArchiveKind) -> String {
    match source_kind {
        SourceArchiveKind::Tar => "rm -rf buildsrc\n\
mkdir -p %{bioconda_source_subdir}\n\
tar -xf %{SOURCE0} -C %{bioconda_source_subdir} --strip-components=1\n"
            .to_string(),
        SourceArchiveKind::Zip => "rm -rf buildsrc\n\
mkdir -p %{bioconda_source_subdir}\n\
zip_unpack_dir=buildsrc/.bioconda2rpm-unpack\n\
rm -rf \"$zip_unpack_dir\"\n\
mkdir -p \"$zip_unpack_dir\"\n\
unzip -q %{SOURCE0} -d \"$zip_unpack_dir\"\n\
zip_root=\"$zip_unpack_dir\"\n\
zip_top_dirs=$(find \"$zip_unpack_dir\" -mindepth 1 -maxdepth 1 -type d | wc -l)\n\
zip_top_files=$(find \"$zip_unpack_dir\" -mindepth 1 -maxdepth 1 -type f | wc -l)\n\
if [[ \"$zip_top_dirs\" -eq 1 && \"$zip_top_files\" -eq 0 ]]; then\n\
  zip_root=$(find \"$zip_unpack_dir\" -mindepth 1 -maxdepth 1 -type d | head -n 1)\n\
fi\n\
cp -a \"$zip_root\"/. %{bioconda_source_subdir}/\n\
rm -rf \"$zip_unpack_dir\"\n"
            .to_string(),
        SourceArchiveKind::File => "rm -rf buildsrc\n\
mkdir -p %{bioconda_source_subdir}\n\
cp -f %{SOURCE0} %{bioconda_source_subdir}/\n"
            .to_string(),
        SourceArchiveKind::Git => "rm -rf buildsrc\n\
git_url=\"%{bioconda_source_git_url}\"\n\
git_rev=\"%{bioconda_source_git_rev}\"\n\
git clone --recursive \"$git_url\" buildsrc\n\
cd buildsrc\n\
git checkout \"$git_rev\"\n\
git submodule update --init --recursive || true\n\
cd ..\n"
            .to_string(),
    }
}

fn render_python_venv_setup_block(python_recipe: bool, python_requirements: &[String]) -> String {
    if !python_recipe {
        return String::new();
    }

    let legacy_pomegranate_mode = python_requirements
        .iter()
        .any(|req| req.starts_with("pomegranate"));
    let requirements_install = if python_requirements.is_empty() {
        String::new()
    } else {
        let requirements_body = python_requirements.join("\n");
        let preinstall_legacy_build_bits = if legacy_pomegranate_mode {
            "\"$PIP\" install \"cython<3\" \"numpy<2\" \"scipy<2\"\n"
        } else {
            ""
        };
        let compile_flags = if legacy_pomegranate_mode {
            " --pip-args \"--no-build-isolation\""
        } else {
            ""
        };
        let install_flags = if legacy_pomegranate_mode {
            " --no-build-isolation"
        } else {
            ""
        };
        format!(
            "cat > requirements.in <<'REQEOF'\n\
{requirements_body}\n\
REQEOF\n\
{preinstall_legacy_build_bits}\
\"$PIP\" install pip-tools\n\
pip-compile --generate-hashes requirements.in --output-file requirements.lock{compile_flags}\n\
\"$PIP\" install{install_flags} --require-hashes -r requirements.lock\n",
            requirements_body = requirements_body,
            preinstall_legacy_build_bits = preinstall_legacy_build_bits,
            compile_flags = compile_flags,
            install_flags = install_flags
        )
    };

    format!(
        "# Charter-compliant Python dependency handling: build hermetic venv and lock deps.\n\
mkdir -p \"$PREFIX/venv\"\n\
\"$PYTHON\" -m venv --copies \"$PREFIX/venv\"\n\
export VIRTUAL_ENV=\"$PREFIX/venv\"\n\
export PATH=\"$VIRTUAL_ENV/bin:$PATH\"\n\
export PYTHON=\"$VIRTUAL_ENV/bin/python\"\n\
export PYTHON3=\"$VIRTUAL_ENV/bin/python\"\n\
export PIP=\"$VIRTUAL_ENV/bin/pip\"\n\
export PIP_DISABLE_PIP_VERSION_CHECK=1\n\
\"$PIP\" install --upgrade pip setuptools wheel\n\
{requirements_install}",
        requirements_install = requirements_install
    )
}

fn render_r_runtime_setup_block(
    r_runtime_required: bool,
    r_project_recipe: bool,
    cran_requirements: &[String],
) -> String {
    if !r_runtime_required {
        return String::new();
    }

    let requested_pkgs = if cran_requirements.is_empty() {
        "character()".to_string()
    } else {
        let pkgs = cran_requirements
            .iter()
            .map(|pkg| format!("\"{pkg}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("c({pkgs})")
    };
    let cran_restore = format!(
        "cat > ./.bioconda2rpm-r-deps.R <<'REOF'\n\
base_pkgs <- c(\"R\", \"base\", \"stats\", \"utils\", \"methods\", \"graphics\", \"grDevices\", \"datasets\", \"tools\", \"grid\", \"compiler\", \"parallel\", \"splines\", \"tcltk\")\n\
req <- unique({requested_pkgs})\n\
desc_candidates <- c(\"DESCRIPTION\", file.path(Sys.getenv(\"SRC_DIR\", \".\"), \"DESCRIPTION\"))\n\
desc_path <- desc_candidates[file.exists(desc_candidates)][1]\n\
if (!is.na(desc_path) && nzchar(desc_path)) {{\n\
  d <- tryCatch(read.dcf(desc_path), error = function(e) NULL)\n\
  if (!is.null(d) && nrow(d) > 0) {{\n\
    fields <- intersect(c(\"Depends\", \"Imports\", \"LinkingTo\"), colnames(d))\n\
    for (f in fields) {{\n\
      raw <- d[1, f]\n\
      if (is.na(raw) || !nzchar(raw)) next\n\
      pieces <- unlist(strsplit(raw, \",\", fixed = TRUE), use.names = FALSE)\n\
      pieces <- trimws(gsub(\"\\\\s*\\\\([^)]*\\\\)\", \"\", pieces))\n\
      pieces <- pieces[nzchar(pieces)]\n\
      req <- unique(c(req, pieces))\n\
    }}\n\
  }}\n\
}}\n\
req <- setdiff(req[nzchar(req)], base_pkgs)\n\
if (!length(req)) quit(save = \"no\", status = 0)\n\
lib <- Sys.getenv(\"R_LIBS_USER\")\n\
if (!nzchar(lib)) lib <- .libPaths()[1]\n\
if (!dir.exists(lib)) dir.create(lib, recursive = TRUE, showWarnings = FALSE)\n\
if (!requireNamespace(\"BiocManager\", quietly = TRUE)) {{\n\
  install.packages(\"BiocManager\", repos = \"https://cloud.r-project.org\", lib = lib)\n\
}}\n\
repos <- tryCatch(BiocManager::repositories(), error = function(e) c(CRAN = \"https://cloud.r-project.org\"))\n\
avail <- tryCatch(rownames(available.packages(repos = repos)), error = function(e) character())\n\
resolve_case <- function(pkg) {{\n\
  if (!length(avail)) return(pkg)\n\
  if (pkg %in% avail) return(pkg)\n\
  hit <- avail[tolower(avail) == tolower(pkg)]\n\
  if (length(hit)) return(hit[[1]])\n\
  pkg\n\
}}\n\
resolved <- unique(vapply(req, resolve_case, character(1)))\n\
installed <- rownames(installed.packages(lib.loc = unique(c(.libPaths(), lib))))\n\
missing <- setdiff(resolved, installed)\n\
if (length(missing)) {{\n\
  BiocManager::install(missing, ask = FALSE, update = FALSE, lib = lib, Ncpus = 1)\n\
}}\n\
installed_after <- rownames(installed.packages(lib.loc = unique(c(.libPaths(), lib))))\n\
still_missing <- setdiff(resolved, installed_after)\n\
install_from_cran_archive <- function(pkg, lib) {{\n\
  archive_url <- sprintf(\"https://cran.r-project.org/src/contrib/Archive/%s/\", pkg)\n\
  idx <- tryCatch(suppressWarnings(readLines(archive_url, warn = FALSE)), error = function(e) character())\n\
  if (!length(idx)) return(FALSE)\n\
  patt <- sprintf(\"%s_[^\\\"']+\\\\.tar\\\\.gz\", pkg)\n\
  hits <- regmatches(idx, gregexpr(patt, idx, perl = TRUE))\n\
  files <- unique(unlist(hits, use.names = FALSE))\n\
  if (!length(files)) return(FALSE)\n\
  tarball <- tail(sort(files), 1)\n\
  ok <- tryCatch({{\n\
    install.packages(paste0(archive_url, tarball), repos = NULL, type = \"source\", lib = lib)\n\
    TRUE\n\
  }}, error = function(e) FALSE)\n\
  ok\n\
}}\n\
if (length(still_missing)) {{\n\
  for (pkg in still_missing) {{\n\
    try(install.packages(pkg, repos = \"https://cloud.r-project.org\", lib = lib), silent = TRUE)\n\
  }}\n\
  installed_after <- rownames(installed.packages(lib.loc = unique(c(.libPaths(), lib))))\n\
  still_missing <- setdiff(resolved, installed_after)\n\
}}\n\
if (length(still_missing)) {{\n\
  for (pkg in still_missing) {{\n\
    try(install_from_cran_archive(pkg, lib), silent = TRUE)\n\
  }}\n\
  installed_after <- rownames(installed.packages(lib.loc = unique(c(.libPaths(), lib))))\n\
  still_missing <- setdiff(resolved, installed_after)\n\
}}\n\
if (length(still_missing)) {{\n\
  message(\"bioconda2rpm unresolved R deps after restore (continuing): \", paste(still_missing, collapse = \",\"))\n\
}}\n\
REOF\n\
\"$RSCRIPT\" ./.bioconda2rpm-r-deps.R\n\
rm -f ./.bioconda2rpm-r-deps.R\n",
        requested_pkgs = requested_pkgs
    );

    let renv_restore = if r_project_recipe {
        "if [[ -f \"renv.lock\" ]]; then\n\
  \"$PHOREUS_R_PREFIX/bin/Rscript\" -e 'install.packages(\"renv\", repos=\"https://cran.r-project.org\")'\n\
  \"$PHOREUS_R_PREFIX/bin/Rscript\" -e 'renv::restore(lockfile = \"renv.lock\", prompt = FALSE)'\n\
fi\n"
            .to_string()
    } else {
        String::new()
    };

    format!(
        "# Charter-compliant R runtime handling: route all R dependency roots through Phoreus R.\n\
export PHOREUS_R_PREFIX=/usr/local/phoreus/r/{phoreus_r_version}\n\
if [[ ! -x \"$PHOREUS_R_PREFIX/bin/Rscript\" ]]; then\n\
  echo \"missing Phoreus R runtime at $PHOREUS_R_PREFIX\" >&2\n\
  exit 42\n\
fi\n\
export PATH=\"$PHOREUS_R_PREFIX/bin:$PATH\"\n\
export R=\"$PHOREUS_R_PREFIX/bin/R\"\n\
export RSCRIPT=\"$PHOREUS_R_PREFIX/bin/Rscript\"\n\
export R_ARGS=\"${{R_ARGS:-}}\"\n\
export LD_LIBRARY_PATH=\"$PHOREUS_R_PREFIX/lib64:$PHOREUS_R_PREFIX/lib${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\"\n\
export R_HOME=\"$PHOREUS_R_PREFIX/lib64/R\"\n\
export R_LIBS_USER=\"$PREFIX/R/library\"\n\
mkdir -p \"$R_LIBS_USER\"\n\
r_lib_paths=(\"$R_LIBS_USER\")\n\
while IFS= read -r -d '' rlib; do\n\
  if [[ -n \"$rlib\" && \"$rlib\" != \"$R_LIBS_USER\" ]]; then\n\
    r_lib_paths+=(\"$rlib\")\n\
  fi\n\
done < <(find /usr/local/phoreus -maxdepth 6 -type d -path '*/R/library' -print0 2>/dev/null || true)\n\
export R_LIBS=\"$(IFS=:; echo \"${{r_lib_paths[*]}}\")\"\n\
export R_LIBS_SITE=\"$R_LIBS\"\n\
{cran_restore}\
{renv_restore}",
        phoreus_r_version = PHOREUS_R_VERSION,
        cran_restore = cran_restore,
        renv_restore = renv_restore
    )
}

fn render_rust_runtime_setup_block(rust_runtime_required: bool) -> String {
    if !rust_runtime_required {
        return String::new();
    }

    format!(
        "# Charter-compliant Rust runtime handling: route rustc/cargo through Phoreus Rust.\n\
export PHOREUS_RUST_PREFIX=/usr/local/phoreus/rust/{phoreus_rust_minor}\n\
if [[ ! -x \"$PHOREUS_RUST_PREFIX/bin/rustc\" || ! -x \"$PHOREUS_RUST_PREFIX/bin/cargo\" ]]; then\n\
  echo \"missing Phoreus Rust runtime at $PHOREUS_RUST_PREFIX\" >&2\n\
  exit 43\n\
fi\n\
export PATH=\"$PHOREUS_RUST_PREFIX/bin:$PATH\"\n\
export CARGO_HOME=\"$PHOREUS_RUST_PREFIX\"\n\
export RUSTUP_HOME=\"$PHOREUS_RUST_PREFIX/.rustup\"\n\
export CARGO_BUILD_JOBS=1\n\
export CARGO_INCREMENTAL=0\n\
export CARGO_TARGET_DIR=\"$(pwd)/.cargo-target\"\n",
        phoreus_rust_minor = PHOREUS_RUST_MINOR
    )
}

fn render_nim_runtime_setup_block(nim_runtime_required: bool) -> String {
    if !nim_runtime_required {
        return String::new();
    }

    format!(
        "# Charter-compliant Nim runtime handling: route nim/nimble through Phoreus Nim.\n\
export PHOREUS_NIM_PREFIX=/usr/local/phoreus/nim/{phoreus_nim_series}\n\
if [[ ! -x \"$PHOREUS_NIM_PREFIX/bin/nim\" || ! -x \"$PHOREUS_NIM_PREFIX/bin/nimble\" ]]; then\n\
  echo \"missing Phoreus Nim runtime at $PHOREUS_NIM_PREFIX\" >&2\n\
  exit 44\n\
fi\n\
export PATH=\"$PHOREUS_NIM_PREFIX/bin:$PATH\"\n\
export NIMBLE_DIR=\"$PREFIX/.nimble\"\n\
mkdir -p \"$NIMBLE_DIR\"\n",
        phoreus_nim_series = PHOREUS_NIM_SERIES
    )
}

fn render_module_lua_env_block(
    python_recipe: bool,
    r_runtime_required: bool,
    rust_runtime_required: bool,
    nim_runtime_required: bool,
) -> String {
    let mut out = String::new();
    if python_recipe {
        out.push_str(
            "setenv(\"VIRTUAL_ENV\", pathJoin(prefix, \"venv\"))\n\
prepend_path(\"PATH\", pathJoin(prefix, \"venv/bin\"))\n\
prepend_path(\"LD_LIBRARY_PATH\", pathJoin(prefix, \"lib\"))\n",
        );
    } else {
        out.push_str(
            "prepend_path(\"PATH\", pathJoin(prefix, \"bin\"))\n\
prepend_path(\"LD_LIBRARY_PATH\", pathJoin(prefix, \"lib\"))\n\
prepend_path(\"MANPATH\", pathJoin(prefix, \"share/man\"))\n",
        );
    }

    if r_runtime_required {
        out.push_str(&format!(
            "setenv(\"PHOREUS_R_VERSION\", \"{phoreus_r_version}\")\n\
setenv(\"R_HOME\", \"/usr/local/phoreus/r/{phoreus_r_version}/lib64/R\")\n\
setenv(\"R_LIBS_USER\", pathJoin(prefix, \"R/library\"))\n",
            phoreus_r_version = PHOREUS_R_VERSION
        ));
    }

    if rust_runtime_required {
        out.push_str(&format!(
            "setenv(\"PHOREUS_RUST_VERSION\", \"{phoreus_rust_version}\")\n\
setenv(\"CARGO_HOME\", pathJoin(prefix, \".cargo\"))\n\
setenv(\"RUSTUP_HOME\", pathJoin(prefix, \".rustup\"))\n",
            phoreus_rust_version = PHOREUS_RUST_VERSION
        ));
    }

    if nim_runtime_required {
        out.push_str(&format!(
            "setenv(\"PHOREUS_NIM_VERSION\", \"{phoreus_nim_series}\")\n\
setenv(\"NIMBLE_DIR\", pathJoin(prefix, \".nimble\"))\n",
            phoreus_nim_series = PHOREUS_NIM_SERIES
        ));
    }

    out
}

fn render_default_spec(software_slug: &str, parsed: &ParsedMeta, meta_version: u64) -> String {
    let license = spec_escape(&parsed.license);
    let version = spec_escape(&parsed.version);
    let changelog_date = rpm_changelog_date();

    format!(
        "%global tool {tool}\n\
%global upstream_version {version}\n\
\n\
Name:           phoreus-%{{tool}}\n\
Version:        {meta_version}\n\
Release:        1%{{?dist}}\n\
Summary:        Default validated {tool} for Phoreus\n\
License:        {license}\n\
BuildArch:      noarch\n\
\n\
Requires:       phoreus\n\
Requires:       phoreus-%{{tool}}-%{{upstream_version}} = %{{upstream_version}}-1%{{?dist}}\n\
\n\
%global phoreus_moddir /usr/local/phoreus/modules/%{{tool}}\n\
\n\
%description\n\
Meta package that tracks the currently validated default %{tool} version.\n\
\n\
%prep\n\
# No source archive required.\n\
\n\
%build\n\
# No build step required.\n\
\n\
%install\n\
rm -rf %{{buildroot}}\n\
mkdir -p %{{buildroot}}%{{phoreus_moddir}}\n\
ln -sfn %{{upstream_version}}.lua %{{buildroot}}%{{phoreus_moddir}}/default.lua\n\
\n\
%files\n\
%{{phoreus_moddir}}/default.lua\n\
\n\
%changelog\n\
* {changelog_date} bioconda2rpm <packaging@bioconda2rpm.local> - {meta_version}-1\n\
- Auto-generated default pointer for {tool} {version}\n",
        tool = software_slug,
        version = version,
        meta_version = meta_version,
        changelog_date = changelog_date,
        license = license,
    )
}

fn format_dep_lines(prefix: &str, deps: &BTreeSet<String>) -> String {
    deps.iter()
        .flat_map(|dep| {
            dep.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|dep| format!("{prefix}:  {dep}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_patch_source_lines(staged_patch_sources: &[String]) -> String {
    if staged_patch_sources.is_empty() {
        String::new()
    } else {
        staged_patch_sources
            .iter()
            .enumerate()
            .map(|(idx, src)| format!("Source{}:        {}", idx + 2, spec_escape(src)))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn render_patch_apply_lines(staged_patch_sources: &[String], source_dir: &str) -> String {
    if staged_patch_sources.is_empty() {
        String::new()
    } else {
        let mut out = String::new();
        out.push_str(&format!("cd {source_dir}\n"));
        for (idx, _) in staged_patch_sources.iter().enumerate() {
            out.push_str(&format!(
                "(patch --batch -p1 -i %{{SOURCE{}}} || patch --batch -p0 -i %{{SOURCE{}}})\n",
                idx + 2,
                idx + 2
            ));
        }
        out
    }
}

fn stage_recipe_patches(
    source_patches: &[String],
    resolved: &ResolvedRecipe,
    sources_dir: &Path,
    software_slug: &str,
) -> Result<Vec<String>> {
    let mut staged = Vec::new();
    for (idx, patch_entry) in source_patches.iter().enumerate() {
        let raw = patch_entry.trim();
        if raw.is_empty() {
            continue;
        }

        if raw.starts_with("http://") || raw.starts_with("https://") || raw.starts_with("ftp://") {
            staged.push(raw.to_string());
            continue;
        }

        let patch_name = raw.split('#').next().unwrap_or(raw).trim();
        let candidates = [
            resolved.variant_dir.join(patch_name),
            resolved.recipe_dir.join(patch_name),
            resolved
                .meta_path
                .parent()
                .unwrap_or(&resolved.variant_dir)
                .join(patch_name),
        ];

        let Some(src_path) = candidates.into_iter().find(|p| p.exists() && p.is_file()) else {
            anyhow::bail!(
                "patch '{}' not found in variant or recipe directory",
                patch_name
            );
        };

        let base = Path::new(patch_name)
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("patch.patch");
        let staged_name = format!("bioconda-{}-patch-{}-{}", software_slug, idx + 1, base);
        let staged_path = sources_dir.join(&staged_name);
        fs::copy(&src_path, &staged_path).with_context(|| {
            format!(
                "copying patch '{}' to '{}'",
                src_path.display(),
                staged_path.display()
            )
        })?;
        #[cfg(unix)]
        fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("setting patch permissions {}", staged_path.display()))?;
        staged.push(staged_name);
    }
    Ok(staged)
}

fn stage_recipe_support_files(resolved: &ResolvedRecipe, sources_dir: &Path) -> Result<()> {
    stage_recipe_support_files_from_dir(&resolved.recipe_dir, sources_dir)?;
    if resolved.variant_dir != resolved.recipe_dir {
        stage_recipe_support_files_from_dir(&resolved.variant_dir, sources_dir)?;
    }
    Ok(())
}

fn stage_recipe_support_files_from_dir(dir: &Path, sources_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if matches!(name, "meta.yaml" | "meta.yml" | "build.sh") {
            continue;
        }
        let destination = sources_dir.join(name);
        fs::copy(&path, &destination).with_context(|| {
            format!(
                "copying recipe support file {} -> {}",
                path.display(),
                destination.display()
            )
        })?;
        #[cfg(unix)]
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("setting permissions on {}", destination.display()))?;
    }
    Ok(())
}

fn spec_escape(input: &str) -> String {
    input
        .replace('%', "%%")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn spec_escape_or_default(input: &str, fallback: &str) -> String {
    if input.trim().is_empty() {
        fallback.to_string()
    } else {
        spec_escape(input)
    }
}

fn normalize_name(name: &str) -> String {
    let mut input = name.trim().to_lowercase();
    input = input.replace('+', "-plus-");
    let mut out = String::new();
    let mut last_dash = false;

    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }

    out.trim_matches('-').to_string()
}

fn normalize_dependency_token(dep: &str) -> String {
    dep.trim().replace('_', "-").to_lowercase()
}

fn normalize_identifier_key(name: &str) -> String {
    normalize_name(name).replace("-plus", "")
}

fn rpm_changelog_date() -> String {
    Utc::now().format("%a %b %d %Y").to_string()
}

fn map_build_dependency(dep: &str) -> String {
    if dep == "r-bpcells" {
        return "phoreus-r-bpcells".to_string();
    }
    if dep == "r-monocle3" {
        return "phoreus-r-monocle3".to_string();
    }
    if let Some(mapped) = map_perl_core_dependency(dep) {
        return mapped;
    }
    if is_r_ecosystem_dependency_name(dep) {
        if is_r_base_dependency_name(dep) {
            return PHOREUS_R_PACKAGE.to_string();
        }
        let normalized = normalize_dependency_token(dep);
        if normalized.starts_with("bioconductor-") {
            return normalized;
        }
        if normalized.starts_with("r-") {
            return PHOREUS_R_PACKAGE.to_string();
        }
        return PHOREUS_R_PACKAGE.to_string();
    }
    if is_rust_ecosystem_dependency_name(dep) {
        return PHOREUS_RUST_PACKAGE.to_string();
    }
    if is_nim_ecosystem_dependency_name(dep) {
        return PHOREUS_NIM_PACKAGE.to_string();
    }
    if is_phoreus_python_toolchain_dependency(dep) {
        return PHOREUS_PYTHON_PACKAGE.to_string();
    }
    if dep == "gsl" {
        // GSL on EL9 links through CBLAS; ensure BLAS headers/libs are present.
        return "gsl-devel openblas-devel".to_string();
    }
    match dep {
        "autoconf" => "autoconf271".to_string(),
        "boost-cpp" => "boost-devel".to_string(),
        "bzip2" => "bzip2-devel".to_string(),
        "cereal" => "cereal-devel".to_string(),
        "clangdev" => "clang-devel".to_string(),
        "eigen" => "eigen3-devel".to_string(),
        "font-ttf-dejavu-sans-mono" => "dejavu-sans-mono-fonts".to_string(),
        "fonts-conda-ecosystem" => "fontconfig".to_string(),
        "mscorefonts" => "dejavu-sans-fonts".to_string(),
        "glib" => "glib2-devel".to_string(),
        "hdf5" => "hdf5-devel".to_string(),
        "go-compiler" => "golang".to_string(),
        "gnuconfig" => "automake".to_string(),
        "isa-l" => "isa-l-devel".to_string(),
        "jansson" => "jansson-devel".to_string(),
        "libcurl" => "libcurl-devel".to_string(),
        "libgd" => "gd-devel".to_string(),
        "libblas" => "openblas-devel".to_string(),
        "libdeflate" => "libdeflate-devel".to_string(),
        "liblzma-devel" => "xz-devel".to_string(),
        "liblapack" => "lapack-devel".to_string(),
        "libhwy" => "highway-devel".to_string(),
        "libiconv" => "glibc-devel".to_string(),
        "libpng" => "libpng-devel".to_string(),
        "libuuid" => "libuuid-devel".to_string(),
        "libopenssl-static" => "openssl-devel".to_string(),
        "lz4-c" => "lz4-devel".to_string(),
        "mysql-connector-c" => "mariadb-connector-c-devel".to_string(),
        "ncurses" => "ncurses-devel".to_string(),
        "ninja" => "ninja-build".to_string(),
        "openssl" => "openssl-devel".to_string(),
        "openmpi" => "openmpi-devel".to_string(),
        "llvmdev" => "llvm-devel".to_string(),
        "xorg-libxfixes" => "libXfixes-devel".to_string(),
        "xz" => "xz-devel".to_string(),
        "zlib" => "zlib-devel".to_string(),
        "zstd" => "libzstd-devel".to_string(),
        other => other.to_string(),
    }
}

fn map_runtime_dependency(dep: &str) -> String {
    if dep == "r-bpcells" {
        return "phoreus-r-bpcells".to_string();
    }
    if dep == "r-monocle3" {
        return "phoreus-r-monocle3".to_string();
    }
    if let Some(mapped) = map_perl_core_dependency(dep) {
        return mapped;
    }
    if is_r_ecosystem_dependency_name(dep) {
        if is_r_base_dependency_name(dep) {
            return PHOREUS_R_PACKAGE.to_string();
        }
        let normalized = normalize_dependency_token(dep);
        if normalized.starts_with("bioconductor-") {
            return normalized;
        }
        if normalized.starts_with("r-") {
            return PHOREUS_R_PACKAGE.to_string();
        }
        return PHOREUS_R_PACKAGE.to_string();
    }
    if is_rust_ecosystem_dependency_name(dep) {
        return PHOREUS_RUST_PACKAGE.to_string();
    }
    if is_nim_ecosystem_dependency_name(dep) {
        return PHOREUS_NIM_PACKAGE.to_string();
    }
    if is_phoreus_python_toolchain_dependency(dep) {
        return PHOREUS_PYTHON_PACKAGE.to_string();
    }
    if dep == "gsl" {
        return "gsl".to_string();
    }
    match dep {
        "k8" => "nodejs".to_string(),
        "boost-cpp" => "boost".to_string(),
        "biopython" => "python3-biopython".to_string(),
        "cereal" => "cereal-devel".to_string(),
        "clangdev" => "clang".to_string(),
        "eigen" => "eigen3-devel".to_string(),
        "font-ttf-dejavu-sans-mono" => "dejavu-sans-mono-fonts".to_string(),
        "fonts-conda-ecosystem" => "fontconfig".to_string(),
        "mscorefonts" => "dejavu-sans-fonts".to_string(),
        "glib" => "glib2".to_string(),
        "gnuconfig" => "automake".to_string(),
        "libblas" => "openblas".to_string(),
        "libhwy" => "highway".to_string(),
        "libiconv" => "glibc".to_string(),
        "libgd" => "gd".to_string(),
        "liblzma-devel" => "xz".to_string(),
        "liblapack" => "lapack".to_string(),
        "mysql-connector-c" => "mariadb-connector-c".to_string(),
        "llvmdev" => "llvm".to_string(),
        "ninja" => "ninja-build".to_string(),
        "xorg-libxfixes" => "libXfixes".to_string(),
        other => other.to_string(),
    }
}

fn is_phoreus_python_toolchain_dependency(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    matches!(
        normalized.as_str(),
        "python"
            | "python3"
            | "python2"
            | "python-abi"
            | "python-abi3"
            | "pip"
            | "setuptools"
            | "wheel"
            | PHOREUS_PYTHON_PACKAGE
    )
}

fn is_conda_only_dependency(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    matches!(
        normalized.as_str(),
        "bioconductor-data-packages" | "go-licenses"
    )
}

fn is_r_ecosystem_dependency_name(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    normalized == "r"
        || normalized == "r-base"
        || normalized == "r-essentials"
        || normalized.starts_with("r-")
        || normalized.starts_with("bioconductor-")
        || normalized == PHOREUS_R_PACKAGE
}

fn is_rust_ecosystem_dependency_name(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    normalized == "rust"
        || normalized == "rustc"
        || normalized == "cargo"
        || normalized == "rustup"
        || normalized.starts_with("rust-")
        || normalized.starts_with("cargo-")
        || normalized == PHOREUS_RUST_PACKAGE
}

fn is_nim_ecosystem_dependency_name(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    normalized == "nim"
        || normalized == "nimble"
        || normalized.starts_with("nim-")
        || normalized == PHOREUS_NIM_PACKAGE
}

fn sync_reference_python_specs(specs_dir: &Path) -> Result<()> {
    let source_dir = Path::new(REFERENCE_PYTHON_SPECS_DIR);
    if !source_dir.exists() {
        anyhow::bail!(
            "reference python specs directory not found: {}",
            source_dir.display()
        );
    }

    for entry in
        fs::read_dir(source_dir).with_context(|| format!("reading {}", source_dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", source_dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if !(name.starts_with("phoreus-python-") && name.ends_with(".spec")) {
            continue;
        }
        let destination = specs_dir.join(name);
        fs::copy(&path, &destination).with_context(|| {
            format!(
                "copying reference python spec {} -> {}",
                path.display(),
                destination.display()
            )
        })?;
        #[cfg(unix)]
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("setting permissions on {}", destination.display()))?;
    }
    Ok(())
}

fn ensure_phoreus_python_bootstrap(build_config: &BuildConfig, specs_dir: &Path) -> Result<()> {
    if topdir_has_package_artifact(&build_config.topdir, PHOREUS_PYTHON_PACKAGE)? {
        return Ok(());
    }

    let spec_name = format!("{}.spec", PHOREUS_PYTHON_PACKAGE);
    let spec_path = specs_dir.join(&spec_name);
    if !spec_path.exists() {
        anyhow::bail!(
            "required bootstrap spec missing: {} (sync from {})",
            spec_path.display(),
            REFERENCE_PYTHON_SPECS_DIR
        );
    }
    build_spec_chain_in_container(build_config, &spec_path, PHOREUS_PYTHON_PACKAGE)
        .with_context(|| format!("building bootstrap package {}", PHOREUS_PYTHON_PACKAGE))?;
    Ok(())
}

fn ensure_phoreus_r_bootstrap(build_config: &BuildConfig, specs_dir: &Path) -> Result<()> {
    let lock = PHOREUS_R_BOOTSTRAP_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("phoreus R bootstrap lock poisoned"))?;

    if topdir_has_package_artifact(&build_config.topdir, PHOREUS_R_PACKAGE)? {
        return Ok(());
    }

    let spec_name = format!("{PHOREUS_R_PACKAGE}.spec");
    let spec_path = specs_dir.join(&spec_name);
    let spec_body = render_phoreus_r_bootstrap_spec();
    fs::write(&spec_path, spec_body)
        .with_context(|| format!("writing R bootstrap spec {}", spec_path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(&spec_path, fs::Permissions::from_mode(0o644))
        .with_context(|| format!("setting permissions on {}", spec_path.display()))?;

    build_spec_chain_in_container(build_config, &spec_path, PHOREUS_R_PACKAGE)
        .with_context(|| format!("building bootstrap package {}", PHOREUS_R_PACKAGE))?;
    Ok(())
}

fn ensure_phoreus_rust_bootstrap(build_config: &BuildConfig, specs_dir: &Path) -> Result<()> {
    let lock = PHOREUS_RUST_BOOTSTRAP_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("phoreus Rust bootstrap lock poisoned"))?;

    if topdir_has_package_artifact(&build_config.topdir, PHOREUS_RUST_PACKAGE)? {
        return Ok(());
    }

    let spec_name = format!("{PHOREUS_RUST_PACKAGE}.spec");
    let spec_path = specs_dir.join(&spec_name);
    let spec_body = render_phoreus_rust_bootstrap_spec();
    fs::write(&spec_path, spec_body)
        .with_context(|| format!("writing Rust bootstrap spec {}", spec_path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(&spec_path, fs::Permissions::from_mode(0o644))
        .with_context(|| format!("setting permissions on {}", spec_path.display()))?;

    build_spec_chain_in_container(build_config, &spec_path, PHOREUS_RUST_PACKAGE)
        .with_context(|| format!("building bootstrap package {}", PHOREUS_RUST_PACKAGE))?;
    Ok(())
}

fn ensure_phoreus_nim_bootstrap(build_config: &BuildConfig, specs_dir: &Path) -> Result<()> {
    let lock = PHOREUS_NIM_BOOTSTRAP_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("phoreus Nim bootstrap lock poisoned"))?;

    if topdir_has_package_artifact(&build_config.topdir, PHOREUS_NIM_PACKAGE)? {
        return Ok(());
    }

    let spec_name = format!("{PHOREUS_NIM_PACKAGE}.spec");
    let spec_path = specs_dir.join(&spec_name);
    let spec_body = render_phoreus_nim_bootstrap_spec();
    fs::write(&spec_path, spec_body)
        .with_context(|| format!("writing Nim bootstrap spec {}", spec_path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(&spec_path, fs::Permissions::from_mode(0o644))
        .with_context(|| format!("setting permissions on {}", spec_path.display()))?;

    build_spec_chain_in_container(build_config, &spec_path, PHOREUS_NIM_PACKAGE)
        .with_context(|| format!("building bootstrap package {}", PHOREUS_NIM_PACKAGE))?;
    Ok(())
}

fn render_phoreus_r_bootstrap_spec() -> String {
    let changelog_date = rpm_changelog_date();
    format!(
        "%global r_minor {r_minor}\n\
%global debug_package %{{nil}}\n\
%global __brp_mangle_shebangs %{{nil}}\n\
\n\
Name:           {name}\n\
Version:        {version}\n\
Release:        1%{{?dist}}\n\
Summary:        Phoreus R {r_minor} runtime built from CRAN source\n\
License:        GPL-2.0-or-later\n\
URL:            https://cran.r-project.org/\n\
Source0:        https://cran.r-project.org/src/base/R-4/R-%{{version}}.tar.gz\n\
\n\
Requires:       phoreus\n\
Provides:       phoreus-R-{version} = %{{version}}-%{{release}}\n\
Provides:       phoreus-r = %{{version}}-%{{release}}\n\
\n\
%global phoreus_tool r\n\
%global phoreus_prefix /usr/local/phoreus/%{{phoreus_tool}}/{version}\n\
%global phoreus_moddir /usr/local/phoreus/modules/%{{phoreus_tool}}\n\
\n\
BuildRequires:  gcc\n\
BuildRequires:  gcc-c++\n\
BuildRequires:  gcc-gfortran\n\
BuildRequires:  make\n\
BuildRequires:  readline-devel\n\
BuildRequires:  pcre2-devel\n\
BuildRequires:  libcurl-devel\n\
BuildRequires:  zlib-devel\n\
BuildRequires:  bzip2-devel\n\
BuildRequires:  xz-devel\n\
BuildRequires:  libjpeg-turbo-devel\n\
BuildRequires:  libpng-devel\n\
BuildRequires:  cairo-devel\n\
\n\
%description\n\
Phoreus R runtime package for R {version}. Builds R from upstream CRAN source\n\
into a dedicated Phoreus prefix for hermetic R-dependent bioinformatics tools.\n\
\n\
%prep\n\
%autosetup -n R-%{{version}}\n\
\n\
%build\n\
./configure \\\n\
  --prefix=%{{phoreus_prefix}} \\\n\
  --enable-R-shlib \\\n\
  --with-x=no\n\
make -s %{{?_smp_mflags}}\n\
\n\
%install\n\
rm -rf %{{buildroot}}\n\
make install DESTDIR=%{{buildroot}}\n\
\n\
mkdir -p %{{buildroot}}%{{phoreus_moddir}}\n\
cat > %{{buildroot}}%{{phoreus_moddir}}/{r_minor}.lua <<'LUAEOF'\n\
help([[ Phoreus R {r_minor} runtime module ]])\n\
whatis(\"Name: r\")\n\
whatis(\"Version: {r_minor}\")\n\
local prefix = \"/usr/local/phoreus/r/{version}\"\n\
setenv(\"PHOREUS_R_VERSION\", \"{version}\")\n\
setenv(\"R_HOME\", pathJoin(prefix, \"lib64/R\"))\n\
prepend_path(\"PATH\", pathJoin(prefix, \"bin\"))\n\
prepend_path(\"LD_LIBRARY_PATH\", pathJoin(prefix, \"lib64\"))\n\
LUAEOF\n\
chmod 0644 %{{buildroot}}%{{phoreus_moddir}}/{r_minor}.lua\n\
\n\
%files\n\
%{{phoreus_prefix}}/\n\
%{{phoreus_moddir}}/{r_minor}.lua\n\
\n\
%changelog\n\
* {changelog_date} bioconda2rpm <packaging@bioconda2rpm.local> - {version}-1\n\
- Build R {version} from upstream CRAN source under Phoreus prefix\n",
        name = PHOREUS_R_PACKAGE,
        version = PHOREUS_R_VERSION,
        r_minor = PHOREUS_R_MINOR,
        changelog_date = changelog_date
    )
}

fn render_phoreus_rust_bootstrap_spec() -> String {
    let changelog_date = rpm_changelog_date();
    format!(
        "%global rust_minor {rust_minor}\n\
%global debug_package %{{nil}}\n\
%global __brp_mangle_shebangs %{{nil}}\n\
\n\
Name:           {name}\n\
Version:        {version}\n\
Release:        1%{{?dist}}\n\
Summary:        Phoreus Rust {rust_minor} runtime with pinned cargo toolchain\n\
License:        Apache-2.0 OR MIT\n\
URL:            https://www.rust-lang.org/\n\
\n\
Requires:       phoreus\n\
Provides:       phoreus-rust = %{{version}}-%{{release}}\n\
\n\
%global phoreus_tool rust\n\
%global phoreus_prefix /usr/local/phoreus/%{{phoreus_tool}}/{rust_minor}\n\
%global phoreus_moddir /usr/local/phoreus/modules/%{{phoreus_tool}}\n\
\n\
BuildRequires:  bash\n\
BuildRequires:  curl\n\
BuildRequires:  ca-certificates\n\
\n\
%description\n\
Phoreus Rust runtime package for Rust {version}. Installs a pinned Rust toolchain\n\
and cargo using upstream rustup-init into a dedicated Phoreus prefix.\n\
\n\
%prep\n\
# No source archive required.\n\
\n\
%build\n\
# No build step required.\n\
\n\
%install\n\
rm -rf %{{buildroot}}\n\
mkdir -p %{{buildroot}}%{{phoreus_prefix}}\n\
export PREFIX=%{{buildroot}}%{{phoreus_prefix}}\n\
export CARGO_HOME=\"$PREFIX\"\n\
export RUSTUP_HOME=\"$PREFIX/.rustup\"\n\
mkdir -p \"$CARGO_HOME/bin\" \"$RUSTUP_HOME\"\n\
\n\
case \"%{{_arch}}\" in\n\
  x86_64)\n\
    rustup_target=\"x86_64-unknown-linux-gnu\"\n\
    ;;\n\
  aarch64)\n\
    rustup_target=\"aarch64-unknown-linux-gnu\"\n\
    ;;\n\
  *)\n\
    echo \"unsupported architecture for phoreus-rust bootstrap: %{{_arch}}\" >&2\n\
    exit 88\n\
    ;;\n\
esac\n\
\n\
rustup_url=\"https://static.rust-lang.org/rustup/dist/${{rustup_target}}/rustup-init\"\n\
curl -fsSL \"$rustup_url\" -o rustup-init\n\
chmod 0755 rustup-init\n\
./rustup-init -y --no-modify-path --profile minimal --default-toolchain {version}\n\
\"$CARGO_HOME/bin/rustc\" --version\n\
\"$CARGO_HOME/bin/cargo\" --version\n\
rm -f rustup-init\n\
\n\
# rustup emits helper env files with absolute install paths. During rpmbuild\n\
# these include %{{buildroot}} and must be normalized to final runtime prefix.\n\
buildroot_prefix=\"%{{buildroot}}%{{phoreus_prefix}}\"\n\
final_prefix=\"%{{phoreus_prefix}}\"\n\
while IFS= read -r -d '' text_path; do\n\
  sed -i \"s|$buildroot_prefix|$final_prefix|g\" \"$text_path\" || true\n\
done < <(grep -RIlZ -- \"$buildroot_prefix\" \"$PREFIX\" 2>/dev/null || true)\n\
\n\
mkdir -p %{{buildroot}}%{{phoreus_moddir}}\n\
cat > %{{buildroot}}%{{phoreus_moddir}}/{rust_minor}.lua <<'LUAEOF'\n\
help([[ Phoreus Rust {rust_minor} runtime module ]])\n\
whatis(\"Name: rust\")\n\
whatis(\"Version: {version}\")\n\
local prefix = \"/usr/local/phoreus/rust/{rust_minor}\"\n\
setenv(\"PHOREUS_RUST_VERSION\", \"{version}\")\n\
setenv(\"CARGO_HOME\", prefix)\n\
setenv(\"RUSTUP_HOME\", pathJoin(prefix, \".rustup\"))\n\
prepend_path(\"PATH\", pathJoin(prefix, \"bin\"))\n\
LUAEOF\n\
chmod 0644 %{{buildroot}}%{{phoreus_moddir}}/{rust_minor}.lua\n\
\n\
%files\n\
%{{phoreus_prefix}}/\n\
%{{phoreus_moddir}}/{rust_minor}.lua\n\
\n\
%changelog\n\
* {changelog_date} bioconda2rpm <packaging@bioconda2rpm.local> - {version}-1\n\
- Install pinned Rust {version} runtime and cargo toolchain under Phoreus prefix\n",
        name = PHOREUS_RUST_PACKAGE,
        version = PHOREUS_RUST_VERSION,
        rust_minor = PHOREUS_RUST_MINOR,
        changelog_date = changelog_date
    )
}

fn render_phoreus_nim_bootstrap_spec() -> String {
    let changelog_date = rpm_changelog_date();
    format!(
        "%global nim_series {nim_series}\n\
%global debug_package %{{nil}}\n\
%global __brp_mangle_shebangs %{{nil}}\n\
\n\
Name:           {name}\n\
Version:        {nim_series}\n\
Release:        1%{{?dist}}\n\
Summary:        Phoreus Nim %{{nim_series}} runtime with nimble\n\
License:        MIT\n\
URL:            https://nim-lang.org/\n\
\n\
Requires:       phoreus\n\
Provides:       phoreus-nim = %{{version}}-%{{release}}\n\
\n\
%global phoreus_tool nim\n\
%global phoreus_prefix /usr/local/phoreus/%{{phoreus_tool}}/{nim_series}\n\
%global phoreus_moddir /usr/local/phoreus/modules/%{{phoreus_tool}}\n\
\n\
BuildRequires:  bash\n\
BuildRequires:  curl\n\
BuildRequires:  tar\n\
BuildRequires:  xz\n\
\n\
%description\n\
Phoreus Nim runtime package for Nim %{{nim_series}}. Installs upstream Nim\n\
precompiled toolchain bundles (including nimble) into a dedicated Phoreus prefix.\n\
\n\
%prep\n\
# No source archive required.\n\
\n\
%build\n\
# No build step required.\n\
\n\
%install\n\
rm -rf %{{buildroot}}\n\
mkdir -p %{{buildroot}}%{{phoreus_prefix}}\n\
export PREFIX=%{{buildroot}}%{{phoreus_prefix}}\n\
\n\
case \"%{{_arch}}\" in\n\
  x86_64)\n\
    nim_asset=\"linux_x64.tar.xz\"\n\
    ;;\n\
  aarch64)\n\
    nim_asset=\"linux_arm64.tar.xz\"\n\
    ;;\n\
  *)\n\
    echo \"unsupported architecture for phoreus-nim bootstrap: %{{_arch}}\" >&2\n\
    exit 89\n\
    ;;\n\
esac\n\
\n\
nim_url=\"https://github.com/nim-lang/nightlies/releases/download/latest-version-2-2/${{nim_asset}}\"\n\
curl -fsSL \"$nim_url\" -o nim.tar.xz\n\
tar -xf nim.tar.xz\n\
nim_root=$(find . -maxdepth 1 -mindepth 1 -type d -name 'nim-*' | sort | tail -n 1)\n\
if [[ -z \"$nim_root\" ]]; then\n\
  echo \"failed to locate extracted nim root directory\" >&2\n\
  exit 90\n\
fi\n\
cp -a \"$nim_root\"/. \"$PREFIX\"/\n\
chmod 0755 \"$PREFIX/bin/\"* || true\n\
\"$PREFIX/bin/nim\" --version\n\
\"$PREFIX/bin/nimble\" --version || true\n\
\n\
mkdir -p %{{buildroot}}%{{phoreus_moddir}}\n\
cat > %{{buildroot}}%{{phoreus_moddir}}/{nim_series}.lua <<'LUAEOF'\n\
help([[ Phoreus Nim {nim_series} runtime module ]])\n\
whatis(\"Name: nim\")\n\
whatis(\"Version: {nim_series}\")\n\
local prefix = \"/usr/local/phoreus/nim/{nim_series}\"\n\
setenv(\"PHOREUS_NIM_VERSION\", \"{nim_series}\")\n\
prepend_path(\"PATH\", pathJoin(prefix, \"bin\"))\n\
LUAEOF\n\
chmod 0644 %{{buildroot}}%{{phoreus_moddir}}/{nim_series}.lua\n\
\n\
%files\n\
%{{phoreus_prefix}}/\n\
%{{phoreus_moddir}}/{nim_series}.lua\n\
\n\
%changelog\n\
* {changelog_date} bioconda2rpm <packaging@bioconda2rpm.local> - {nim_series}-1\n\
- Install Nim {nim_series} toolchain bundle under Phoreus prefix\n",
        name = PHOREUS_NIM_PACKAGE,
        nim_series = PHOREUS_NIM_SERIES,
        changelog_date = changelog_date
    )
}

fn topdir_has_package_artifact(topdir: &Path, package_name: &str) -> Result<bool> {
    for file_name in artifact_filenames(topdir)? {
        if file_name.starts_with(&format!("{package_name}-")) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn map_perl_core_dependency(dep: &str) -> Option<String> {
    let mapped = match dep {
        "perl-extutils-makemaker" => "perl-ExtUtils-MakeMaker",
        "perl-compress-raw-bzip2" => "perl-Compress-Raw-Bzip2",
        "perl-compress-raw-zlib" => "perl-Compress-Raw-Zlib",
        "perl-scalar-list-utils" => "perl-Scalar-List-Utils",
        "perl-carp" => "perl-Carp",
        "perl-exporter" => "perl-Exporter",
        "perl-file-path" => "perl-File-Path",
        "perl-file-temp" => "perl-File-Temp",
        "perl-module-build" => "perl-Module-Build",
        "perl-autoloader" => "perl-AutoLoader",
        "perl-pathtools" => "perl-PathTools",
        "perl-test" => "perl-Test",
        "perl-test-harness" => "perl-Test-Harness",
        "perl-test-nowarnings" => "perl-Test-NoWarnings",
        "perl-test-simple" => "perl-Test-Simple",
        "perl-number-compare" => "perl-Number-Compare",
        "perl-module-load" => "perl-Module-Load",
        "perl-params-check" => "perl-Params-Check",
        "perl-test-more" => "perl-Test-Simple",
        "perl-storable" => "perl-Storable",
        "perl-encode" => "perl-Encode",
        "perl-exporter-tiny" => "perl-Exporter-Tiny",
        "perl-test-leaktrace" => "perl-Test-LeakTrace",
        "perl-canary-stability" => "perl-Canary-Stability",
        "perl-types-serialiser" => "perl-Types-Serialiser",
        "perl-data-dumper" => "perl-Data-Dumper",
        "perl-xml-parser" => "perl-XML-Parser",
        "perl-importer" => "perl-Importer",
        _ => return None,
    };
    Some(mapped.to_string())
}

fn payload_version_state(
    topdir: &Path,
    software_slug: &str,
    target_version: &str,
) -> Result<PayloadVersionState> {
    let Some(existing) = latest_existing_payload_version(topdir, software_slug)? else {
        return Ok(PayloadVersionState::NotBuilt);
    };
    let ord = compare_version_labels(&existing, target_version);
    if ord == Ordering::Less {
        Ok(PayloadVersionState::Outdated {
            existing_version: existing,
        })
    } else {
        Ok(PayloadVersionState::UpToDate {
            existing_version: existing,
        })
    }
}

fn latest_existing_payload_version(topdir: &Path, software_slug: &str) -> Result<Option<String>> {
    let mut versions = BTreeSet::new();
    for name in artifact_filenames(topdir)? {
        if let Some(version) = extract_payload_version_from_name(&name, software_slug) {
            versions.insert(version);
        }
    }
    if versions.is_empty() {
        return Ok(None);
    }
    let latest = versions
        .iter()
        .max_by(|a, b| compare_version_labels(a, b))
        .cloned();
    Ok(latest)
}

fn next_meta_package_version(topdir: &Path, software_slug: &str) -> Result<u64> {
    let mut max_meta = 0u64;
    for name in artifact_filenames(topdir)? {
        if let Some(v) = extract_meta_package_version_from_name(&name, software_slug)
            && v > max_meta
        {
            max_meta = v;
        }
    }
    Ok(max_meta.saturating_add(1).max(1))
}

fn artifact_filenames(topdir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let candidates = [
        topdir.join("RPMS"),
        topdir.join("SRPMS"),
        topdir.join("SPECS"),
    ];

    for root in candidates {
        if !root.exists() {
            continue;
        }
        collect_artifact_names(&root, &mut names)?;
    }
    Ok(names)
}

fn collect_artifact_names(dir: &Path, names: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_artifact_names(&path, names)?;
            continue;
        }
        if let Some(name) = path.file_name().and_then(|v| v.to_str()) {
            names.push(name.to_string());
        }
    }
    Ok(())
}

fn extract_payload_version_from_name(name: &str, software_slug: &str) -> Option<String> {
    let prefix = format!("phoreus-{software_slug}-");
    if !name.starts_with(&prefix) {
        return None;
    }
    let rest = name
        .trim_end_matches(".src.rpm")
        .trim_end_matches(".rpm")
        .strip_prefix(&prefix)?;
    let parts: Vec<&str> = rest.split('-').collect();
    if parts.len() < 2 {
        return None;
    }
    if parts[0] == parts[1] {
        return Some(parts[0].to_string());
    }
    None
}

fn extract_meta_package_version_from_name(name: &str, software_slug: &str) -> Option<u64> {
    let prefix = format!("phoreus-{software_slug}-");
    if !name.starts_with(&prefix) {
        return None;
    }
    let rest = name
        .trim_end_matches(".src.rpm")
        .trim_end_matches(".rpm")
        .strip_prefix(&prefix)?;
    let parts: Vec<&str> = rest.split('-').collect();
    if parts.len() < 2 {
        return None;
    }
    if parts[0] == parts[1] {
        return None;
    }
    parts[0].parse::<u64>().ok()
}

fn ensure_container_engine_available(engine: &str) -> Result<()> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {engine} >/dev/null 2>&1"))
        .status()
        .with_context(|| format!("checking container engine '{engine}'"))?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("container engine not found: {engine}");
    }
}

fn build_spec_chain_in_container(
    build_config: &BuildConfig,
    spec_path: &Path,
    label: &str,
) -> Result<()> {
    let spec_name = spec_path
        .file_name()
        .and_then(|v| v.to_str())
        .context("spec filename missing")?;
    let spec_in_container = format!("/work/SPECS/{spec_name}");
    let work_mount = format!("{}:/work", build_config.topdir.display());
    let build_label = label.replace('\'', "_");
    let stage_started = Instant::now();
    log_progress(format!(
        "phase=container-build status=queued label={} spec={} image={}",
        build_label, spec_name, build_config.container_image
    ));
    let logs_dir = build_config.reports_dir.join("build_logs");
    fs::create_dir_all(&logs_dir)
        .with_context(|| format!("creating build logs dir {}", logs_dir.display()))?;
    let final_log_path = logs_dir.join(format!("{}.log", sanitize_label(&build_label)));

    let script = format!(
        "set -euo pipefail\n\
sanitize_field() {{\n\
  printf '%s' \"$1\" | tr '\\n' ' ' | tr '|' '/'\n\
}}\n\
emit_depgraph() {{\n\
  local dep status source provider detail\n\
  dep=$(sanitize_field \"$1\")\n\
  status=$(sanitize_field \"$2\")\n\
  source=$(sanitize_field \"$3\")\n\
  provider=$(sanitize_field \"$4\")\n\
  detail=$(sanitize_field \"$5\")\n\
  printf 'DEPGRAPH|%s|%s|%s|%s|%s\\n' \"$dep\" \"$status\" \"$source\" \"$provider\" \"$detail\"\n\
}}\n\
build_root=/work/.build-work/{label}\n\
rm -rf \"$build_root\"\n\
mkdir -p \"$build_root\"/BUILD \"$build_root\"/BUILDROOT \"$build_root\"/RPMS \"$build_root\"/SOURCES \"$build_root\"/SPECS \"$build_root\"/SRPMS\n\
mkdir -p /work/RPMS /work/SRPMS /work/SOURCES /work/SPECS\n\
if ! command -v rpmbuild >/dev/null 2>&1; then\n\
  if command -v dnf >/dev/null 2>&1; then dnf -y install rpm-build rpmdevtools >/dev/null; \\\n\
  elif command -v microdnf >/dev/null 2>&1; then microdnf -y install rpm-build rpmdevtools >/dev/null; \\\n\
  elif command -v yum >/dev/null 2>&1; then yum -y install rpm-build rpmdevtools >/dev/null; \\\n\
  else echo 'no supported package manager for rpm-build install' >&2; exit 2; fi\n\
fi\n\
if ! command -v spectool >/dev/null 2>&1; then\n\
  if command -v dnf >/dev/null 2>&1; then dnf -y install rpmdevtools >/dev/null; \\\n\
  elif command -v microdnf >/dev/null 2>&1; then microdnf -y install rpmdevtools >/dev/null; \\\n\
  elif command -v yum >/dev/null 2>&1; then yum -y install rpmdevtools >/dev/null; \\\n\
  else echo 'spectool unavailable and rpmdevtools cannot be installed' >&2; exit 3; fi\n\
fi\n\
touch /work/.build-start-{label}.ts\n\
rpm_single_core_flags=(--define '_smp_mflags -j1' --define '_smp_build_ncpus 1')\n\
source0_url=$(awk '/^Source0:[[:space:]]+/ {{print $2; exit}}' '{spec}' || true)\n\
source_candidates=()\n\
if [[ -n \"$source0_url\" ]]; then\n\
  source_candidates+=(\"$source0_url\")\n\
fi\n\
if [[ \"$source0_url\" =~ ^https://bioconductor.org/packages/.*/bioc/src/contrib/([^/]+)_[^/]+\\.tar\\.gz$ ]]; then\n\
  bioc_pkg=\"${{BASH_REMATCH[1]}}\"\n\
  archive_url=$(printf '%s' \"$source0_url\" | sed -E \"s#(/bioc/src/contrib/)#\\\\1Archive/$bioc_pkg/#\")\n\
  source_candidates+=(\"$archive_url\")\n\
fi\n\
if [[ \"$source0_url\" =~ ^(.*/)([^/]+)-([0-9][0-9\\.]*)-([0-9]+)\\.zip$ ]]; then\n\
  source_prefix=\"${{BASH_REMATCH[1]}}\"\n\
  source_name=\"${{BASH_REMATCH[2]}}\"\n\
  source_version=\"${{BASH_REMATCH[3]}}\"\n\
  source_build=\"${{BASH_REMATCH[4]}}\"\n\
  source_candidates+=(\"${{source_prefix}}${{source_name}}-${{source_version}}.zip\")\n\
  if [[ \"$source_build\" =~ ^[0-9]+$ ]]; then\n\
    build_num=$source_build\n\
    while (( build_num > 1 )); do\n\
      build_num=$((build_num - 1))\n\
      source_candidates+=(\"${{source_prefix}}${{source_name}}-${{source_version}}-${{build_num}}.zip\")\n\
    done\n\
  fi\n\
fi\n\
spectool_ok=0\n\
if [[ -z \"$source0_url\" ]]; then\n\
  spectool_ok=1\n\
else\n\
  dedup_source_candidates=()\n\
  for candidate in \"${{source_candidates[@]}}\"; do\n\
    if [[ -z \"$candidate\" ]]; then\n\
      continue\n\
    fi\n\
    duplicate=0\n\
    for existing in \"${{dedup_source_candidates[@]:-}}\"; do\n\
      if [[ \"$existing\" == \"$candidate\" ]]; then\n\
        duplicate=1\n\
        break\n\
      fi\n\
    done\n\
    if [[ \"$duplicate\" -eq 0 ]]; then\n\
      dedup_source_candidates+=(\"$candidate\")\n\
    fi\n\
  done\n\
  source_candidates=(\"${{dedup_source_candidates[@]}}\")\n\
  if [[ \"${{#source_candidates[@]}}\" -eq 0 ]]; then\n\
    echo 'no Source0 URL found in spec' >&2\n\
    exit 6\n\
  fi\n\
  for candidate in \"${{source_candidates[@]}}\"; do\n\
    escaped_candidate=$(printf '%s' \"$candidate\" | sed 's/[\\/&]/\\\\&/g')\n\
    sed -i \"s/^Source0:[[:space:]].*$/Source0:        $escaped_candidate/\" '{spec}'\n\
    echo \"Downloading: $candidate\"\n\
    for attempt in 1 2 3; do\n\
      if spectool -g -R --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' '{spec}'; then\n\
        candidate_file=\"$candidate\"\n\
        candidate_file=\"${{candidate_file%%\\#*}}\"\n\
        candidate_file=\"${{candidate_file%%\\?*}}\"\n\
        candidate_file=\"${{candidate_file##*/}}\"\n\
        if [[ -n \"$candidate_file\" && -s \"/work/SOURCES/$candidate_file\" ]]; then\n\
          spectool_ok=1\n\
          break 2\n\
        fi\n\
        echo \"source download did not produce /work/SOURCES/$candidate_file\" >&2\n\
      fi\n\
      sleep $((attempt * 2))\n\
    done\n\
  done\n\
fi\n\
if [[ \"$spectool_ok\" -ne 1 ]]; then\n\
  if [[ \"$source0_url\" == ftp://* ]]; then\n\
    ftp_file=\"$source0_url\"\n\
    ftp_file=\"${{ftp_file%%\\#*}}\"\n\
    ftp_file=\"${{ftp_file%%\\?*}}\"\n\
    ftp_file=\"${{ftp_file##*/}}\"\n\
    if [[ -n \"$ftp_file\" ]]; then\n\
      echo \"Attempting FTP prefetch fallback: $source0_url\"\n\
      if command -v wget >/dev/null 2>&1; then\n\
        wget -O \"/work/SOURCES/$ftp_file\" \"$source0_url\" || true\n\
      elif command -v curl >/dev/null 2>&1; then\n\
        curl -L --fail --output \"/work/SOURCES/$ftp_file\" \"$source0_url\" || true\n\
      fi\n\
      if [[ -s \"/work/SOURCES/$ftp_file\" ]]; then\n\
        spectool_ok=1\n\
      fi\n\
    fi\n\
  fi\n\
fi\n\
if [[ \"$spectool_ok\" -ne 1 ]]; then\n\
  echo 'source download failed after retries' >&2\n\
  exit 6\n\
fi\n\
find /work/SPECS -type f -name '*.spec' -exec chmod 0644 {{}} + || true\n\
find /work/SOURCES -type f -exec chmod 0644 {{}} + || true\n\
rpmbuild -bs --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' \"${{rpm_single_core_flags[@]}}\" '{spec}'\n\
srpm_path=$(find \"$build_root/SRPMS\" -type f -name '*.src.rpm' | sort | tail -n 1)\n\
if [[ -z \"${{srpm_path}}\" ]]; then\n\
  echo 'no SRPM produced from spec build step' >&2\n\
  exit 4\n\
fi\n\
\n\
pm=''\n\
if command -v dnf >/dev/null 2>&1; then\n\
  pm='dnf'\n\
elif command -v microdnf >/dev/null 2>&1; then\n\
  pm='microdnf'\n\
elif command -v yum >/dev/null 2>&1; then\n\
  pm='yum'\n\
fi\n\
if [[ -z \"$pm\" ]]; then\n\
  echo 'no supported package manager for dependency preflight' >&2\n\
  exit 5\n\
fi\n\
pm_install() {{\n\
  \"$pm\" -y --setopt='*.skip_if_unavailable=true' --disablerepo=dropworm install \"$@\"\n\
}}\n\
\n\
declare -A local_candidates\n\
while IFS= read -r -d '' rpmf; do\n\
  name=$(rpm -qp --qf '%{{NAME}}\\n' \"$rpmf\" 2>/dev/null || true)\n\
  if [[ -n \"$name\" && -z \"${{local_candidates[$name]:-}}\" ]]; then\n\
    local_candidates[\"$name\"]=\"$rpmf\"\n\
  fi\n\
  while IFS= read -r provide; do\n\
    key=$(printf '%s' \"$provide\" | awk '{{print $1}}')\n\
    if [[ -n \"$key\" && -z \"${{local_candidates[$key]:-}}\" ]]; then\n\
      local_candidates[\"$key\"]=\"$rpmf\"\n\
    fi\n\
  done < <(rpm -qp --provides \"$rpmf\" 2>/dev/null || true)\n\
done < <(find /work/RPMS -type f -name '*.rpm' -print0 2>/dev/null)\n\
\n\
declare -A local_installed\n\
install_local_with_hydration() {{\n\
  local req_key=\"$1\"\n\
  local local_rpm=\"${{local_candidates[$req_key]:-}}\"\n\
  if [[ -z \"$local_rpm\" ]]; then\n\
    return 1\n\
  fi\n\
  local queue=(\"$local_rpm\")\n\
  while [[ \"${{#queue[@]}}\" -gt 0 ]]; do\n\
    local rpmf=\"${{queue[0]}}\"\n\
    queue=(\"${{queue[@]:1}}\")\n\
    if [[ -z \"$rpmf\" || -n \"${{local_installed[$rpmf]:-}}\" ]]; then\n\
      continue\n\
    fi\n\
    if ! rpm -Uvh --nodeps --force \"$rpmf\" >>\"$dep_log\" 2>&1; then\n\
      return 1\n\
    fi\n\
    local_installed[\"$rpmf\"]=1\n\
    mapfile -t local_requires < <(rpm -qpR \"$rpmf\" 2>/dev/null | awk '{{print $1}}' | sed '/^$/d' | sort -u)\n\
    for req in \"${{local_requires[@]}}\"; do\n\
      case \"$req\" in\n\
        \"\"|rpmlib*|rtld*|ld-linux*|phoreus)\n\
          continue\n\
          ;;\n\
      esac\n\
      candidate=\"$req\"\n\
      if [[ \"$candidate\" == *\"(\"* || \"$candidate\" == *\")\"* || \"$candidate\" == *\":\"* ]]; then\n\
        if [[ \"$candidate\" == lib*.so* ]]; then\n\
          candidate=\"${{candidate%%.so*}}\"\n\
        else\n\
          pm_install \"$req\" >>\"$dep_log\" 2>&1 || true\n\
          continue\n\
        fi\n\
      fi\n\
      if [[ \"$candidate\" == /* ]]; then\n\
        continue\n\
      fi\n\
      if rpm -q --whatprovides \"$req\" >/dev/null 2>&1 || rpm -q --whatprovides \"$candidate\" >/dev/null 2>&1; then\n\
        continue\n\
      fi\n\
      nested_local_rpm=\"${{local_candidates[$req]:-}}\"\n\
      if [[ -z \"$nested_local_rpm\" ]]; then\n\
        nested_local_rpm=\"${{local_candidates[$candidate]:-}}\"\n\
      fi\n\
      if [[ -n \"$nested_local_rpm\" ]]; then\n\
        if [[ -z \"${{local_installed[$nested_local_rpm]:-}}\" ]]; then\n\
          queue+=(\"$nested_local_rpm\")\n\
        fi\n\
        continue\n\
      fi\n\
      if ! pm_install \"$candidate\" >>\"$dep_log\" 2>&1; then\n\
        if [[ \"$candidate\" == perl-* ]]; then\n\
          perl_cap=$(printf '%s' \"${{candidate#perl-}}\" | awk -F- '{{for (i=1; i<=NF; i++) {{$i=toupper(substr($i,1,1)) substr($i,2)}}; out=$1; for (i=2; i<=NF; i++) {{out=out \"::\" $i}}; print out}}')\n\
          if [[ -n \"$perl_cap\" ]]; then\n\
            pm_install \"perl($perl_cap)\" >>\"$dep_log\" 2>&1 || true\n\
          fi\n\
        fi\n\
      fi\n\
    done\n\
  done\n\
  return 0\n\
}}\n\
\n\
mapfile -t build_requires < <(rpmspec -q --buildrequires --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' --define '_smp_build_ncpus 1' '{spec}' | awk '{{print $1}}' | sed '/^$/d' | sort -u)\n\
dep_log=\"/tmp/bioconda2rpm-dep-{label}.log\"\n\
for dep in \"${{build_requires[@]}}\"; do\n\
  if rpm -q --whatprovides \"$dep\" >/dev/null 2>&1; then\n\
    provider=$(rpm -q --whatprovides \"$dep\" | head -n 1 || true)\n\
    emit_depgraph \"$dep\" 'resolved' 'installed' \"$provider\" 'already_installed'\n\
    continue\n\
  fi\n\
\n\
  local_rpm=\"${{local_candidates[$dep]:-}}\"\n\
  if [[ -n \"$local_rpm\" ]]; then\n\
    if pm_install \"$local_rpm\" >\"$dep_log\" 2>&1; then\n\
      if rpm -q --whatprovides \"$dep\" >/dev/null 2>&1; then\n\
        provider=$(rpm -q --whatprovides \"$dep\" | head -n 1 || true)\n\
        emit_depgraph \"$dep\" 'resolved' 'local_rpm' \"$provider\" \"installed_from_$(basename \"$local_rpm\")\"\n\
        continue\n\
      fi\n\
    elif install_local_with_hydration \"$dep\"; then\n\
      # Attempt best-effort hydration of runtime deps after nodeps install so\n\
      # local RPM reuse remains functional even when non-repo capabilities\n\
      # (for example 'phoreus') block strict package-manager resolution.\n\
      if rpm -q --whatprovides \"$dep\" >/dev/null 2>&1; then\n\
        provider=$(rpm -q --whatprovides \"$dep\" | head -n 1 || true)\n\
        emit_depgraph \"$dep\" 'resolved' 'local_rpm' \"$provider\" \"installed_nodeps_from_$(basename \"$local_rpm\")_with_repo_hydration\"\n\
        continue\n\
      fi\n\
    fi\n\
  fi\n\
\n\
  if pm_install \"$dep\" >\"$dep_log\" 2>&1; then\n\
    provider=$(rpm -q --whatprovides \"$dep\" | head -n 1 || true)\n\
    emit_depgraph \"$dep\" 'resolved' 'repo' \"$provider\" 'installed_from_repo'\n\
  else\n\
    if [[ \"$dep\" == perl-* ]]; then\n\
      perl_cap=$(printf '%s' \"${{dep#perl-}}\" | awk -F- '{{for (i=1; i<=NF; i++) {{$i=toupper(substr($i,1,1)) substr($i,2)}}; out=$1; for (i=2; i<=NF; i++) {{out=out \"::\" $i}}; print out}}')\n\
      if [[ -n \"$perl_cap\" ]] && pm_install \"perl($perl_cap)\" >\"$dep_log\" 2>&1; then\n\
        provider=$(rpm -q --whatprovides \"perl($perl_cap)\" | head -n 1 || true)\n\
        emit_depgraph \"$dep\" 'resolved' 'repo' \"$provider\" \"installed_from_repo_via_perl($perl_cap)\"\n\
        continue\n\
      fi\n\
    fi\n\
    detail=$(tail -n 3 \"$dep_log\" | tr '\\n' ';' | sed 's/;/; /g')\n\
    emit_depgraph \"$dep\" 'unresolved' 'unresolved' '-' \"$detail\"\n\
  fi\n\
done\n\
\n\
rpmbuild --rebuild --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' \"${{rpm_single_core_flags[@]}}\" \"${{srpm_path}}\"\n\
find \"$build_root/SRPMS\" -type f -name '*.src.rpm' -exec cp -f {{}} /work/SRPMS/ \\;\n\
while IFS= read -r rpmf; do\n\
  rel=\"${{rpmf#$build_root/RPMS/}}\"\n\
  dst=\"/work/RPMS/$(dirname \"$rel\")\"\n\
  mkdir -p \"$dst\"\n\
  cp -f \"$rpmf\" \"$dst/\"\n\
done < <(find \"$build_root/RPMS\" -type f -name '*.rpm')\n",
        label = build_label,
        spec = sh_single_quote(&spec_in_container),
    );

    let run_once = |attempt: usize| -> Result<(std::process::ExitStatus, String)> {
        let step_started = Instant::now();
        log_progress(format!(
            "phase=container-build status=started label={} spec={} attempt={} image={}",
            build_label, spec_name, attempt, build_config.container_image
        ));
        let attempt_log_path = logs_dir.join(format!(
            "{}.attempt{}.log",
            sanitize_label(&build_label),
            attempt
        ));
        let stdout_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&attempt_log_path)
            .with_context(|| format!("opening attempt log {}", attempt_log_path.display()))?;
        let stderr_file = stdout_file
            .try_clone()
            .with_context(|| format!("cloning attempt log {}", attempt_log_path.display()))?;

        let mut cmd = Command::new(&build_config.container_engine);
        cmd.arg("run")
            .arg("--rm")
            .arg("-v")
            .arg(&work_mount)
            .arg("-w")
            .arg("/work")
            .arg("--user")
            .arg("0:0");

        cmd.arg(&build_config.container_image)
            .arg("bash")
            .arg("-lc")
            .arg(&script);
        cmd.stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "running container build chain for {} using image {}",
                spec_name, build_config.container_image
            )
        })?;

        let mut next_heartbeat = Duration::from_secs(60);
        loop {
            if child
                .try_wait()
                .with_context(|| format!("polling container build chain for {}", spec_name))?
                .is_some()
            {
                break;
            }
            std::thread::sleep(Duration::from_secs(5));
            let elapsed = step_started.elapsed();
            if elapsed >= next_heartbeat {
                log_progress(format!(
                    "phase=container-build status=running label={} spec={} attempt={} elapsed={}",
                    build_label,
                    spec_name,
                    attempt,
                    format_elapsed(elapsed)
                ));
                next_heartbeat += Duration::from_secs(60);
            }
        }

        let status = child
            .wait()
            .with_context(|| format!("waiting for container build output for {}", spec_name))?;
        let combined = String::from_utf8_lossy(
            &fs::read(&attempt_log_path)
                .with_context(|| format!("reading attempt log {}", attempt_log_path.display()))?,
        )
        .into_owned();
        log_progress(format!(
            "phase=container-build status=finished label={} spec={} attempt={} elapsed={} exit={}",
            build_label,
            spec_name,
            attempt,
            format_elapsed(step_started.elapsed()),
            status
        ));
        Ok((status, combined))
    };

    let (mut status, mut combined) = run_once(1)?;
    if !status.success() && is_source_permission_denied(&combined) {
        log_progress(format!(
            "phase=container-build status=retrying label={} spec={} reason=source-permission-denied",
            build_label, spec_name
        ));
        fix_host_source_permissions(&build_config.topdir.join("SOURCES"))?;
        let retry = run_once(2)?;
        status = retry.0;
        combined = retry.1;
    }

    let dep_events = parse_dependency_events(&combined);
    let dep_summary = persist_dependency_graph(
        &build_config.reports_dir,
        &build_label,
        &spec_name.replace(".spec", ""),
        &dep_events,
    )
    .ok()
    .flatten();
    if let Some(summary) = dep_summary.as_ref() {
        log_progress(format!(
            "phase=dependency-resolution spec={} total_events={} unresolved={} graph_md={} graph_json={}",
            spec_name,
            dep_events.len(),
            summary.unresolved.len(),
            summary.md_path.display(),
            summary.json_path.display()
        ));
        if !summary.unresolved.is_empty() {
            log_progress(format!(
                "phase=dependency-resolution spec={} unresolved_deps={}",
                spec_name,
                summary.unresolved.join(",")
            ));
        }
    }

    fs::write(&final_log_path, &combined)
        .with_context(|| format!("writing build log {}", final_log_path.display()))?;

    if !status.success() {
        let arch_policy =
            classify_arch_policy(&combined, &build_config.host_arch).unwrap_or("unknown");
        let tail = tail_lines(&combined, 20);
        log_progress(format!(
            "phase=container-build status=failed label={} spec={} elapsed={} arch_policy={} failure_hint={}",
            build_label,
            spec_name,
            format_elapsed(stage_started.elapsed()),
            arch_policy,
            compact_reason(&tail, 280)
        ));
        let dep_hint = dep_summary
            .as_ref()
            .map(|summary| {
                format!(
                    " dependency_graph_json={} dependency_graph_md={} unresolved_deps={}",
                    summary.json_path.display(),
                    summary.md_path.display(),
                    if summary.unresolved.is_empty() {
                        "none".to_string()
                    } else {
                        summary.unresolved.join(",")
                    }
                )
            })
            .unwrap_or_default();
        anyhow::bail!(
            "container build chain failed for {} (exit status: {}) elapsed={} arch_policy={} log={} tail={}{}",
            spec_name,
            status,
            format_elapsed(stage_started.elapsed()),
            arch_policy,
            final_log_path.display(),
            tail,
            dep_hint
        );
    }

    log_progress(format!(
        "phase=container-build status=completed label={} spec={} elapsed={}",
        build_label,
        spec_name,
        format_elapsed(stage_started.elapsed())
    ));
    Ok(())
}

fn sh_single_quote(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
}

fn sanitize_label(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn tail_lines(text: &str, line_count: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !looks_like_transfer_progress(trimmed)
        })
        .collect();
    let start = lines.len().saturating_sub(line_count);
    lines[start..].join(" | ")
}

fn looks_like_transfer_progress(line: &str) -> bool {
    // Filters repetitive progress rows from wget/curl style output so BAD_SPEC
    // tails retain the actionable error lines.
    let starts_with_digit = line
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false);
    (line.contains("..........") && line.contains('%'))
        || (starts_with_digit && line.contains("...") && line.contains('%'))
}

fn classify_arch_policy(build_log: &str, host_arch: &str) -> Option<&'static str> {
    let lower = build_log.to_lowercase();
    if (host_arch == "aarch64" || host_arch == "arm64")
        && lower.contains("no upstream precompiled k8 binary for linux/aarch64")
    {
        return Some("amd64_only");
    }

    let x86_intrinsics = lower.contains("emmintrin.h")
        || lower.contains("xmmintrin.h")
        || lower.contains("pmmintrin.h")
        || lower.contains("immintrin.h");
    if (host_arch == "aarch64" || host_arch == "arm64") && x86_intrinsics {
        return Some("amd64_only");
    }

    let arm_intrinsics = lower.contains("arm_neon.h") || lower.contains("neon");
    if (host_arch == "x86_64" || host_arch == "amd64") && arm_intrinsics {
        return Some("aarch64_only");
    }

    None
}

fn is_source_permission_denied(build_log: &str) -> bool {
    let lower = build_log.to_lowercase();
    lower.contains("bad file: /work/sources/") && lower.contains("permission denied")
}

fn fix_host_source_permissions(sources_dir: &Path) -> Result<()> {
    if !sources_dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(sources_dir)
        .with_context(|| format!("reading sources directory {}", sources_dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", sources_dir.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("setting source permissions {}", path.display()))?;
    }
    Ok(())
}

fn quarantine_note(bad_spec_dir: &Path, slug: &str, reason: &str) {
    let note_path = bad_spec_dir.join(format!("{slug}.txt"));
    let body = format!("status=quarantined\nreason={reason}\n");
    let _ = fs::write(note_path, body);
}

fn clear_quarantine_note(bad_spec_dir: &Path, slug: &str) {
    let note_path = bad_spec_dir.join(format!("{slug}.txt"));
    if note_path.exists() {
        let _ = fs::remove_file(note_path);
    }
}

fn parse_dependency_events(build_log: &str) -> Vec<DependencyResolutionEvent> {
    build_log
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('|');
            if parts.next()? != "DEPGRAPH" {
                return None;
            }
            let dependency = parts.next()?.trim().to_string();
            let status = parts.next()?.trim().to_string();
            let source = parts.next()?.trim().to_string();
            let provider = parts.next().unwrap_or_default().trim().to_string();
            let detail = parts.next().unwrap_or_default().trim().to_string();
            Some(DependencyResolutionEvent {
                dependency,
                status,
                source,
                provider,
                detail,
            })
        })
        .collect()
}

fn persist_dependency_graph(
    reports_dir: &Path,
    label: &str,
    spec_name: &str,
    events: &[DependencyResolutionEvent],
) -> Result<Option<DependencyGraphSummary>> {
    if events.is_empty() {
        return Ok(None);
    }

    let dep_graph_dir = reports_dir.join("dependency_graphs");
    fs::create_dir_all(&dep_graph_dir)
        .with_context(|| format!("creating dependency graph dir {}", dep_graph_dir.display()))?;

    let slug = sanitize_label(label);
    let json_path = dep_graph_dir.join(format!("{slug}.json"));
    let md_path = dep_graph_dir.join(format!("{slug}.md"));

    let payload =
        serde_json::to_string_pretty(events).context("serializing dependency graph events")?;
    fs::write(&json_path, payload)
        .with_context(|| format!("writing dependency graph json {}", json_path.display()))?;

    let mut unresolved = BTreeSet::new();
    let mut resolved_count = 0usize;
    let mut md = String::new();
    md.push_str("# Dependency Resolution Graph\n\n");
    md.push_str(&format!("- Spec: `{}`\n", spec_name));
    md.push_str(&format!("- Total dependencies: {}\n", events.len()));
    for event in events {
        if event.status == "unresolved" {
            unresolved.insert(event.dependency.clone());
        } else if event.status == "resolved" {
            resolved_count += 1;
        }
    }
    md.push_str(&format!("- Resolved dependencies: {}\n", resolved_count));
    md.push_str(&format!(
        "- Unresolved dependencies: {}\n\n",
        unresolved.len()
    ));
    md.push_str("| Dependency | Status | Source | Provider | Detail |\n");
    md.push_str("|---|---|---|---|---|\n");
    for event in events {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            event.dependency.replace('|', "\\|"),
            event.status.replace('|', "\\|"),
            event.source.replace('|', "\\|"),
            event.provider.replace('|', "\\|"),
            event.detail.replace('|', "\\|")
        ));
    }
    fs::write(&md_path, md)
        .with_context(|| format!("writing dependency graph markdown {}", md_path.display()))?;

    Ok(Some(DependencyGraphSummary {
        json_path,
        md_path,
        unresolved: unresolved.into_iter().collect(),
    }))
}

fn write_reports(
    entries: &[ReportEntry],
    json_path: &Path,
    csv_path: &Path,
    md_path: &Path,
) -> Result<()> {
    let json = serde_json::to_string_pretty(entries).context("serializing json report")?;
    fs::write(json_path, json)
        .with_context(|| format!("writing json report {}", json_path.display()))?;

    let mut writer = Writer::from_path(csv_path)
        .with_context(|| format!("opening csv report {}", csv_path.display()))?;
    for entry in entries {
        writer.serialize(entry).context("writing csv row")?;
    }
    writer.flush().context("flushing csv writer")?;

    let generated = entries.iter().filter(|e| e.status == "generated").count();
    let quarantined = entries.len().saturating_sub(generated);

    let mut md = String::new();
    md.push_str("# Priority SPEC Generation Summary\n\n");
    md.push_str(&format!("- Requested: {}\n", entries.len()));
    md.push_str(&format!("- Generated: {}\n", generated));
    md.push_str(&format!("- Quarantined: {}\n\n", quarantined));
    md.push_str("| Software | Priority | Status | Overlap Recipe | Version | Reason |\n");
    md.push_str("|---|---:|---|---|---|---|\n");
    for e in entries {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            e.software,
            e.priority,
            e.status,
            if e.overlap_recipe.is_empty() {
                "-"
            } else {
                &e.overlap_recipe
            },
            if e.version.is_empty() {
                "-"
            } else {
                &e.version
            },
            e.reason.replace('|', "\\|")
        ));
    }

    fs::write(md_path, md).with_context(|| format!("writing md report {}", md_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn normalize_dependency_maps_compilers() {
        assert_eq!(
            normalize_dependency_name("c-compiler"),
            Some("gcc".to_string())
        );
        assert_eq!(
            normalize_dependency_name("cxx-compiler"),
            Some("gcc-c++".to_string())
        );
        assert_eq!(
            normalize_dependency_name("openjdk >=11.0.1"),
            Some("java-11-openjdk".to_string())
        );
        assert_eq!(
            normalize_dependency_name("pandas>=0.21,<0.24"),
            Some("pandas".to_string())
        );
        assert_eq!(
            normalize_dependency_name("bioconductor-ucsc.utils >=1.2.0"),
            Some("bioconductor-ucsc-utils".to_string())
        );
    }

    #[test]
    fn conda_only_dependencies_include_go_licenses() {
        assert!(is_conda_only_dependency("go-licenses"));
    }

    #[test]
    fn dependency_mapping_handles_conda_aliases() {
        assert_eq!(map_build_dependency("boost-cpp"), "boost-devel".to_string());
        assert_eq!(map_build_dependency("autoconf"), "autoconf271".to_string());
        assert_eq!(map_build_dependency("hdf5"), "hdf5-devel".to_string());
        assert_eq!(map_runtime_dependency("boost-cpp"), "boost".to_string());
        assert_eq!(map_build_dependency("eigen"), "eigen3-devel".to_string());
        assert_eq!(
            map_runtime_dependency("biopython"),
            "python3-biopython".to_string()
        );
        assert_eq!(
            map_build_dependency("libdeflate"),
            "libdeflate-devel".to_string()
        );
        assert_eq!(
            map_build_dependency("libopenssl-static"),
            "openssl-devel".to_string()
        );
        assert_eq!(
            map_build_dependency("mysql-connector-c"),
            "mariadb-connector-c-devel".to_string()
        );
        assert_eq!(map_build_dependency("zlib"), "zlib-devel".to_string());
        assert_eq!(map_build_dependency("openssl"), "openssl-devel".to_string());
        assert_eq!(map_build_dependency("bzip2"), "bzip2-devel".to_string());
        assert_eq!(
            map_build_dependency("xorg-libxfixes"),
            "libXfixes-devel".to_string()
        );
        assert_eq!(map_build_dependency("isa-l"), "isa-l-devel".to_string());
        assert_eq!(map_build_dependency("xz"), "xz-devel".to_string());
        assert_eq!(map_build_dependency("libcurl"), "libcurl-devel".to_string());
        assert_eq!(map_build_dependency("libpng"), "libpng-devel".to_string());
        assert_eq!(map_build_dependency("libuuid"), "libuuid-devel".to_string());
        assert_eq!(map_build_dependency("libhwy"), "highway-devel".to_string());
        assert_eq!(
            map_build_dependency("libblas"),
            "openblas-devel".to_string()
        );
        assert_eq!(
            map_build_dependency("liblapack"),
            "lapack-devel".to_string()
        );
        assert_eq!(
            map_build_dependency("liblzma-devel"),
            "xz-devel".to_string()
        );
        assert_eq!(map_build_dependency("ninja"), "ninja-build".to_string());
        assert_eq!(map_build_dependency("cereal"), "cereal-devel".to_string());
        assert_eq!(map_build_dependency("gnuconfig"), "automake".to_string());
        assert_eq!(map_build_dependency("glib"), "glib2-devel".to_string());
        assert_eq!(map_build_dependency("libiconv"), "glibc-devel".to_string());
        assert_eq!(
            map_build_dependency("font-ttf-dejavu-sans-mono"),
            "dejavu-sans-mono-fonts".to_string()
        );
        assert_eq!(
            map_runtime_dependency("font-ttf-dejavu-sans-mono"),
            "dejavu-sans-mono-fonts".to_string()
        );
        assert_eq!(
            map_build_dependency("gsl"),
            "gsl-devel openblas-devel".to_string()
        );
        assert_eq!(map_runtime_dependency("gsl"), "gsl".to_string());
        assert_eq!(
            map_build_dependency("fonts-conda-ecosystem"),
            "fontconfig".to_string()
        );
        assert_eq!(
            map_runtime_dependency("fonts-conda-ecosystem"),
            "fontconfig".to_string()
        );
        assert_eq!(map_runtime_dependency("ninja"), "ninja-build".to_string());
        assert_eq!(map_runtime_dependency("cereal"), "cereal-devel".to_string());
        assert_eq!(map_runtime_dependency("k8"), "nodejs".to_string());
        assert_eq!(map_runtime_dependency("gnuconfig"), "automake".to_string());
        assert_eq!(map_runtime_dependency("libblas"), "openblas".to_string());
        assert_eq!(map_runtime_dependency("libhwy"), "highway".to_string());
        assert_eq!(map_runtime_dependency("libiconv"), "glibc".to_string());
        assert_eq!(map_runtime_dependency("glib"), "glib2".to_string());
        assert_eq!(map_runtime_dependency("liblapack"), "lapack".to_string());
        assert_eq!(map_runtime_dependency("liblzma-devel"), "xz".to_string());
        assert_eq!(
            map_runtime_dependency("xorg-libxfixes"),
            "libXfixes".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-canary-stability"),
            "perl-Canary-Stability".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-types-serialiser"),
            "perl-Types-Serialiser".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-autoloader"),
            "perl-AutoLoader".to_string()
        );
        assert_eq!(map_build_dependency("perl-test"), "perl-Test".to_string());
        assert_eq!(
            map_build_dependency("perl-test-nowarnings"),
            "perl-Test-NoWarnings".to_string()
        );
        assert_eq!(
            map_build_dependency("python"),
            PHOREUS_PYTHON_PACKAGE.to_string()
        );
        assert_eq!(
            map_build_dependency("r-bpcells"),
            "phoreus-r-bpcells".to_string()
        );
        assert_eq!(
            map_build_dependency("r-monocle3"),
            "phoreus-r-monocle3".to_string()
        );
        assert_eq!(
            map_runtime_dependency("python"),
            PHOREUS_PYTHON_PACKAGE.to_string()
        );
        assert_eq!(
            map_runtime_dependency("r-bpcells"),
            "phoreus-r-bpcells".to_string()
        );
        assert_eq!(
            map_runtime_dependency("r-monocle3"),
            "phoreus-r-monocle3".to_string()
        );
        assert_eq!(
            map_build_dependency("setuptools"),
            PHOREUS_PYTHON_PACKAGE.to_string()
        );
        assert_eq!(
            map_runtime_dependency("setuptools"),
            PHOREUS_PYTHON_PACKAGE.to_string()
        );
        assert_eq!(map_build_dependency("nim"), PHOREUS_NIM_PACKAGE.to_string());
        assert_eq!(
            map_runtime_dependency("nimble"),
            PHOREUS_NIM_PACKAGE.to_string()
        );
        assert_eq!(
            normalize_dependency_name("python_abi 3.11.* *_cp311"),
            Some(PHOREUS_PYTHON_PACKAGE.to_string())
        );
    }

    #[test]
    fn parse_meta_extracts_source_patches() {
        let rendered = r#"
package:
  name: blast
  version: 2.5.0
source:
  url: http://example.invalid/src.tar.gz
  patches:
    - boost_106400.patch
about:
  license: Public-Domain
requirements:
  build:
    - c-compiler
"#;
        let parsed = parse_rendered_meta(rendered).expect("parse rendered meta");
        assert_eq!(
            parsed.source_patches,
            vec!["boost_106400.patch".to_string()]
        );
    }

    #[test]
    fn payload_spec_renders_patch_sources_and_apply_steps() {
        let parsed = ParsedMeta {
            package_name: "blast".to_string(),
            version: "2.5.0".to_string(),
            build_number: "0".to_string(),
            source_url: "http://example.invalid/src.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "http://example.invalid".to_string(),
            license: "Public-Domain".to_string(),
            summary: "blast".to_string(),
            source_patches: vec!["boost_106400.patch".to_string()],
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: Vec::new(),
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };
        let spec = render_payload_spec(
            "blast",
            &parsed,
            "bioconda-blast-build.sh",
            &["bioconda-blast-patch-1-boost_106400.patch".to_string()],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains("Source2:"));
        assert!(spec.contains("patch --batch -p1 -i %{SOURCE2}"));
        assert!(spec.contains("bash -eo pipefail ./build.sh"));
        assert!(spec.contains("retry_snapshot=\"$(pwd)/.bioconda2rpm-retry-snapshot.tar\""));
        assert!(spec.contains("export CPU_COUNT=1"));
        assert!(spec.contains("export MAKEFLAGS=-j1"));
        assert!(spec.contains("/opt/rh/autoconf271/bin/autoconf"));
        assert!(
            spec.contains("find /usr/local/phoreus -mindepth 3 -maxdepth 3 -type d -name include")
        );
        assert!(spec.contains("export CPATH=\"$dep_include${CPATH:+:$CPATH}\""));
        assert!(spec.contains("disabled by bioconda2rpm for EL9 compatibility"));
        assert!(spec.contains("if [[ \"${CONFIG_SITE:-}\" == \"NONE\" ]]; then"));
        assert!(spec.contains("export PKG_NAME=\"${PKG_NAME:-blast}\""));
        assert!(spec.contains("export PKG_VERSION=\"${PKG_VERSION:-2.5.0}\""));
        assert!(spec.contains("export PKG_BUILDNUM=\"${PKG_BUILDNUM:-0}\""));
        assert!(spec.contains("export ncbi_cv_lib_boost_test=no"));
    }

    #[test]
    fn source_archive_kind_detection_handles_queries_and_fragments() {
        assert_eq!(
            source_archive_kind("https://example.invalid/fastqc_v0.12.1.zip"),
            SourceArchiveKind::Zip
        );
        assert_eq!(
            source_archive_kind("https://example.invalid/fastqc_v0.12.1.zip?download=1#section"),
            SourceArchiveKind::Zip
        );
        assert_eq!(
            source_archive_kind("https://example.invalid/tool-1.0.tar.gz"),
            SourceArchiveKind::Tar
        );
        assert_eq!(
            source_archive_kind("https://example.invalid/nextflow"),
            SourceArchiveKind::File
        );
    }

    #[test]
    fn payload_spec_uses_unzip_for_zip_sources() {
        let parsed = ParsedMeta {
            package_name: "fastqc".to_string(),
            version: "0.12.1".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/fastqc_v0.12.1.zip".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/fastqc".to_string(),
            license: "GPL-3.0-or-later".to_string(),
            summary: "fastqc".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: Vec::new(),
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let spec = render_payload_spec(
            "fastqc",
            &parsed,
            "bioconda-fastqc-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains("BuildRequires:  unzip"));
        assert!(spec.contains("unzip -q %{SOURCE0} -d \"$zip_unpack_dir\""));
        assert!(
            !spec.contains("tar -xf %{SOURCE0} -C %{bioconda_source_subdir} --strip-components=1")
        );
    }

    #[test]
    fn payload_spec_copies_single_file_sources() {
        let parsed = ParsedMeta {
            package_name: "nextflow".to_string(),
            version: "25.10.4".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/nextflow".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/nextflow".to_string(),
            license: "Apache-2.0".to_string(),
            summary: "nextflow".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: Vec::new(),
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let spec = render_payload_spec(
            "nextflow",
            &parsed,
            "bioconda-nextflow-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains("cp -f %{SOURCE0} %{bioconda_source_subdir}/"));
        assert!(!spec.contains("tar -xf %{SOURCE0}"));
        assert!(!spec.contains("unzip -q %{SOURCE0}"));
    }

    #[test]
    fn parse_meta_extracts_build_script_and_noarch_python() {
        let rendered = r#"
package:
  name: multiqc
  version: "1.33"
source:
  url: https://example.invalid/multiqc.tar.gz
build:
  noarch: python
  script: $PYTHON -m pip install . --no-deps
about:
  license: GPL-3.0-or-later
"#;
        let parsed = parse_rendered_meta(rendered).expect("parse rendered meta");
        assert_eq!(
            parsed.build_script.as_deref(),
            Some("$PYTHON -m pip install . --no-deps")
        );
        assert!(parsed.noarch_python);
    }

    #[test]
    fn rendered_meta_build_skip_detection_handles_true_and_false() {
        let skipped = r#"
build:
  skip: true
"#;
        let not_skipped = r#"
build:
  skip: false
"#;
        assert!(rendered_meta_declares_build_skip(skipped));
        assert!(!rendered_meta_declares_build_skip(not_skipped));
    }

    #[test]
    fn parse_meta_preserves_raw_run_dependency_specs() {
        let rendered = r#"
package:
  name: multiqc
  version: "1.33"
source:
  url: https://example.invalid/multiqc.tar.gz
requirements:
  run:
    - python >=3.8,!=3.14.1
    - jinja2 >=3.0.0
    - python-kaleido ==0.2.1
"#;
        let parsed = parse_rendered_meta(rendered).expect("parse rendered meta");
        assert!(
            parsed
                .run_dep_specs_raw
                .contains(&"jinja2 >=3.0.0".to_string())
        );
        assert!(
            parsed
                .run_dep_specs_raw
                .contains(&"python-kaleido ==0.2.1".to_string())
        );
    }

    #[test]
    fn parse_meta_reads_first_source_url_from_url_list() {
        let rendered = r#"
package:
  name: bioconductor-edger
  version: "4.4.0"
source:
  url:
    - https://bioconductor.org/packages/3.20/bioc/src/contrib/edgeR_4.4.0.tar.gz
    - https://bioarchive.galaxyproject.org/edgeR_4.4.0.tar.gz
  md5: db45a60f88cb89ea135743c1eb39b99c
"#;
        let parsed = parse_rendered_meta(rendered).expect("parse rendered meta");
        assert_eq!(
            parsed.source_url,
            "https://bioconductor.org/packages/3.20/bioc/src/contrib/edgeR_4.4.0.tar.gz"
        );
    }

    #[test]
    fn parse_meta_synthesizes_github_archive_from_git_source() {
        let rendered = r#"
package:
  name: nanopolish
  version: "0.14.0"
source:
  git_url: https://github.com/jts/nanopolish.git
  git_rev: v0.14.0
"#;
        let parsed = parse_rendered_meta(rendered).expect("parse rendered meta");
        assert_eq!(
            parsed.source_url,
            "git+https://github.com/jts/nanopolish.git#v0.14.0"
        );
    }

    #[test]
    fn python_requirements_are_converted_to_pip_specs() {
        assert_eq!(
            conda_dep_to_pip_requirement("jinja2 >=3.0.0"),
            Some("jinja2>=3.0.0".to_string())
        );
        assert_eq!(
            conda_dep_to_pip_requirement("python-kaleido ==0.2.1"),
            Some("kaleido==0.2.1".to_string())
        );
        assert_eq!(
            conda_dep_to_pip_requirement("python-annoy >=1.11.5"),
            Some("annoy>=1.11.5".to_string())
        );
        assert_eq!(
            conda_dep_to_pip_requirement("matplotlib-base >=3.5.2"),
            Some("matplotlib>=3.5.2".to_string())
        );
        assert_eq!(
            conda_dep_to_pip_requirement("pandas>=0.21,<0.24"),
            Some("pandas>=0.21,<0.24".to_string())
        );
        assert_eq!(
            conda_dep_to_pip_requirement("scanpy=1.9.3"),
            Some("scanpy==1.9.3".to_string())
        );
        assert_eq!(conda_dep_to_pip_requirement("bedtools"), None);
        assert_eq!(conda_dep_to_pip_requirement("bats"), None);
        assert_eq!(conda_dep_to_pip_requirement("python >=3.8"), None);
        assert_eq!(conda_dep_to_pip_requirement("c-compiler"), None);
    }

    #[test]
    fn python_requirement_relaxation_for_runtime_conflict() {
        let rendered = r#"
package:
  name: scanpy-scripts
  version: 1.9.301
requirements:
  host:
    - python <3.10
  run:
    - scanpy =1.9.3
    - scipy <1.9.0
    - bbknn >=1.5.0,<1.6.0
    - fa2
    - mnnpy >=0.1.9.5
"#;
        let parsed = parse_rendered_meta(rendered).expect("parse meta");
        let reqs = build_python_requirements(&parsed);
        assert!(reqs.contains(&"scanpy>=1.9.3".to_string()));
        assert!(reqs.contains(&"scipy".to_string()));
        assert!(reqs.contains(&"bbknn>=1.5.0".to_string()));
        assert!(!reqs.iter().any(|r| r.starts_with("fa2")));
        assert!(!reqs.iter().any(|r| r.starts_with("mnnpy")));
    }

    #[test]
    fn python_requirements_add_cython_cap_for_pomegranate() {
        let parsed = ParsedMeta {
            package_name: "cnvkit".to_string(),
            version: "0.9.12".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/cnvkit-0.9.12.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/cnvkit".to_string(),
            license: "Apache-2.0".to_string(),
            summary: "cnvkit".to_string(),
            source_patches: Vec::new(),
            build_script: Some("$PYTHON -m pip install . --no-deps".to_string()),
            noarch_python: true,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: vec!["python >=3.8".to_string()],
            run_dep_specs_raw: vec!["pomegranate >=0.14.8,<=0.14.9".to_string()],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let reqs = build_python_requirements(&parsed);
        assert!(reqs.iter().any(|r| r.starts_with("pomegranate")));
        assert!(reqs.contains(&"cython<3".to_string()));
        assert!(reqs.contains(&"numpy<2".to_string()));
    }

    #[test]
    fn python_venv_install_disables_build_isolation_for_pomegranate() {
        let block = render_python_venv_setup_block(
            true,
            &["pomegranate>=0.14.8".to_string(), "cython<3".to_string()],
        );
        assert!(block.contains("pip-compile --generate-hashes"));
        assert!(block.contains("--pip-args \"--no-build-isolation\""));
        assert!(block.contains("\"$PIP\" install \"cython<3\" \"numpy<2\" \"scipy<2\""));
        assert!(block.contains("install --no-build-isolation --require-hashes"));
    }

    #[test]
    fn r_dependencies_are_not_converted_to_pip_specs() {
        assert_eq!(conda_dep_to_pip_requirement("r-ggplot2 >=3.5.0"), None);
        assert_eq!(
            conda_dep_to_pip_requirement("bioconductor-genomicranges"),
            None
        );
    }

    #[test]
    fn r_dependencies_map_to_phoreus_r_runtime() {
        assert_eq!(
            map_build_dependency("r-ggplot2"),
            PHOREUS_R_PACKAGE.to_string()
        );
        assert_eq!(
            map_runtime_dependency("bioconductor-limma"),
            "bioconductor-limma".to_string()
        );
        assert_eq!(
            map_runtime_dependency("r-ggplot2"),
            PHOREUS_R_PACKAGE.to_string()
        );
        assert_eq!(
            map_runtime_dependency("r-base"),
            PHOREUS_R_PACKAGE.to_string()
        );
    }

    #[test]
    fn rust_dependencies_map_to_phoreus_rust_runtime() {
        assert_eq!(
            map_build_dependency("rust"),
            PHOREUS_RUST_PACKAGE.to_string()
        );
        assert_eq!(
            map_build_dependency("cargo"),
            PHOREUS_RUST_PACKAGE.to_string()
        );
        assert_eq!(
            map_runtime_dependency("rustc"),
            PHOREUS_RUST_PACKAGE.to_string()
        );
    }

    #[test]
    fn phoreus_r_bootstrap_spec_is_rendered_with_expected_name() {
        let spec = render_phoreus_r_bootstrap_spec();
        assert!(spec.contains("Name:           phoreus-r-4.5.2"));
        assert!(spec.contains("Version:        4.5.2"));
        assert!(spec.contains(
            "Source0:        https://cran.r-project.org/src/base/R-4/R-%{version}.tar.gz"
        ));
        assert!(spec.contains("--with-x=no"));
    }

    #[test]
    fn phoreus_rust_bootstrap_spec_is_rendered_with_expected_name() {
        let spec = render_phoreus_rust_bootstrap_spec();
        assert!(spec.contains("Name:           phoreus-rust-1.92"));
        assert!(spec.contains("Version:        1.92.0"));
        assert!(spec.contains("rustup-init"));
        assert!(spec.contains("default-toolchain 1.92.0"));
    }

    #[test]
    fn phoreus_nim_bootstrap_spec_is_rendered_with_expected_name() {
        let spec = render_phoreus_nim_bootstrap_spec();
        assert!(spec.contains("Name:           phoreus-nim-2.2"));
        assert!(spec.contains("Version:        2.2"));
        assert!(spec.contains("linux_arm64.tar.xz"));
        assert!(spec.contains("linux_x64.tar.xz"));
    }

    #[test]
    fn k8_uses_precompiled_binary_override() {
        let parsed = ParsedMeta {
            package_name: "k8".to_string(),
            version: "1.2".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/source.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://github.com/attractivechaos/k8".to_string(),
            license: "MIT".to_string(),
            summary: "k8".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: Vec::new(),
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let override_cfg =
            precompiled_binary_override("k8", &parsed).expect("k8 precompiled override");
        assert_eq!(
            override_cfg.source_url,
            "https://github.com/attractivechaos/k8/releases/download/v1.2/k8-1.2.tar.bz2"
        );
        assert!(
            override_cfg
                .build_script
                .contains("no upstream precompiled k8 binary")
        );
    }

    #[test]
    fn k8_is_not_treated_as_python_recipe() {
        let mut build_deps = BTreeSet::new();
        build_deps.insert(PHOREUS_PYTHON_PACKAGE.to_string());
        build_deps.insert("gcc-c++".to_string());
        build_deps.insert("make".to_string());

        let parsed = ParsedMeta {
            package_name: "k8".to_string(),
            version: "1.2".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/source.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://github.com/attractivechaos/k8".to_string(),
            license: "MIT".to_string(),
            summary: "k8".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: vec!["sysroot_linux-64 >=2.17".to_string()],
            build_deps,
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        assert!(!is_python_recipe(&parsed));
    }

    #[test]
    fn runtime_python_dependency_alone_does_not_force_python_recipe() {
        let mut run_deps = BTreeSet::new();
        run_deps.insert(PHOREUS_PYTHON_PACKAGE.to_string());
        run_deps.insert("htslib".to_string());

        let parsed = ParsedMeta {
            package_name: "stringtie".to_string(),
            version: "3.0.3".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/stringtie-3.0.3.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/stringtie".to_string(),
            license: "MIT".to_string(),
            summary: "stringtie".to_string(),
            source_patches: Vec::new(),
            build_script: Some(
                "make -j${CPU_COUNT}\ninstall -m 0755 stringtie $PREFIX/bin".to_string(),
            ),
            noarch_python: false,
            build_dep_specs_raw: vec!["automake".to_string()],
            host_dep_specs_raw: vec!["htslib".to_string()],
            run_dep_specs_raw: vec!["python".to_string(), "htslib".to_string()],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps,
        };

        assert!(!is_python_recipe(&parsed));
        let reqs = build_python_requirements(&parsed);
        assert!(!reqs.iter().any(|r| r.contains("automake")));
        assert!(!reqs.iter().any(|r| r.starts_with("python")));
    }

    #[test]
    fn python_requirements_ignore_build_section_tools() {
        let parsed = ParsedMeta {
            package_name: "python-demo".to_string(),
            version: "1.0.0".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/python-demo-1.0.0.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/python-demo".to_string(),
            license: "MIT".to_string(),
            summary: "python-demo".to_string(),
            source_patches: Vec::new(),
            build_script: Some("$PYTHON -m pip install . --no-deps".to_string()),
            noarch_python: true,
            build_dep_specs_raw: vec!["automake".to_string(), "make".to_string()],
            host_dep_specs_raw: vec!["python >=3.11".to_string(), "jinja2 >=3.0.0".to_string()],
            run_dep_specs_raw: vec!["python >=3.11".to_string(), "click >=8.0".to_string()],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let reqs = build_python_requirements(&parsed);
        assert!(reqs.contains(&"jinja2>=3.0.0".to_string()));
        assert!(reqs.contains(&"click>=8.0".to_string()));
        assert!(!reqs.iter().any(|r| r.contains("automake")));
    }

    #[test]
    fn python_requirements_exclude_system_bio_tools() {
        let parsed = ParsedMeta {
            package_name: "ragtag".to_string(),
            version: "2.1.0".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/RagTag-2.1.0.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/ragtag".to_string(),
            license: "MIT".to_string(),
            summary: "ragtag".to_string(),
            source_patches: Vec::new(),
            build_script: Some("$PYTHON -m pip install .".to_string()),
            noarch_python: true,
            build_dep_specs_raw: vec!["pip".to_string(), "python >3".to_string()],
            host_dep_specs_raw: vec!["python >3".to_string(), "numpy".to_string()],
            run_dep_specs_raw: vec![
                "python >3".to_string(),
                "numpy".to_string(),
                "minimap2".to_string(),
                "mummer".to_string(),
            ],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let reqs = build_python_requirements(&parsed);
        assert!(reqs.contains(&"numpy".to_string()));
        assert!(!reqs.iter().any(|r| r == "mummer"));
        assert!(!reqs.iter().any(|r| r == "minimap2"));
    }

    #[test]
    fn precompiled_policy_limits_dependency_planning_to_runtime() {
        let mut build_deps = BTreeSet::new();
        build_deps.insert("gcc-c++".to_string());
        build_deps.insert("make".to_string());
        let mut run_deps = BTreeSet::new();
        run_deps.insert("zlib".to_string());

        let parsed = ParsedMeta {
            package_name: "k8".to_string(),
            version: "1.2".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/source.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://github.com/attractivechaos/k8".to_string(),
            license: "MIT".to_string(),
            summary: "k8".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: Vec::new(),
            build_deps,
            host_deps: BTreeSet::new(),
            run_deps,
        };

        let selected = selected_dependency_set(&parsed, &DependencyPolicy::BuildHostRun, true);
        assert_eq!(selected, BTreeSet::from(["zlib".to_string()]));
    }

    #[test]
    fn python_payload_spec_routes_python_build_deps_to_venv() {
        let mut build_deps = BTreeSet::new();
        build_deps.insert("gcc".to_string());
        let mut host_deps = BTreeSet::new();
        host_deps.insert(PHOREUS_PYTHON_PACKAGE.to_string());
        host_deps.insert("cython".to_string());
        host_deps.insert("setuptools-scm".to_string());
        let mut run_deps = BTreeSet::new();
        run_deps.insert(PHOREUS_PYTHON_PACKAGE.to_string());
        run_deps.insert("dnaio".to_string());
        run_deps.insert("xopen".to_string());

        let parsed = ParsedMeta {
            package_name: "cutadapt".to_string(),
            version: "5.2".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/cutadapt-5.2.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://cutadapt.readthedocs.io/".to_string(),
            license: "MIT".to_string(),
            summary: "cutadapt".to_string(),
            source_patches: Vec::new(),
            build_script: Some(
                "$PYTHON -m pip install . --no-deps --no-build-isolation".to_string(),
            ),
            noarch_python: false,
            build_dep_specs_raw: vec!["c-compiler".to_string()],
            host_dep_specs_raw: vec![
                "python".to_string(),
                "pip".to_string(),
                "cython".to_string(),
                "setuptools-scm".to_string(),
            ],
            run_dep_specs_raw: vec![
                "python".to_string(),
                "xopen >=1.6.0".to_string(),
                "dnaio >=1.2.2".to_string(),
            ],
            build_deps,
            host_deps,
            run_deps,
        };

        let spec = render_payload_spec(
            "cutadapt",
            &parsed,
            "bioconda-cutadapt-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains("BuildRequires:  gcc"));
        assert!(!spec.contains("BuildRequires:  cython"));
        assert!(!spec.contains("BuildRequires:  setuptools-scm"));
        assert!(spec.contains("cython"));
        assert!(spec.contains("setuptools-scm"));
    }

    #[test]
    fn synthesized_build_script_canonicalizes_python_invocation() {
        let script = "-m pip install . --no-deps --no-build-isolation";
        let generated = synthesize_build_sh_from_meta_script(script);
        assert!(generated.contains("set -euxo pipefail"));
        assert!(generated.contains("$PYTHON -m pip install . --no-deps --no-build-isolation"));
    }

    #[test]
    fn python_payload_with_r_dependency_requires_phoreus_r_runtime() {
        let mut run_deps = BTreeSet::new();
        run_deps.insert("r-ggplot2".to_string());
        run_deps.insert(PHOREUS_PYTHON_PACKAGE.to_string());

        let parsed = ParsedMeta {
            package_name: "gatk".to_string(),
            version: "3.8".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/gatk-3.8.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://gatk.broadinstitute.org/".to_string(),
            license: "BSD-3-Clause".to_string(),
            summary: "gatk".to_string(),
            source_patches: Vec::new(),
            build_script: Some("$PYTHON -m pip install . --no-deps".to_string()),
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: vec!["python".to_string()],
            run_dep_specs_raw: vec!["python".to_string(), "r-ggplot2".to_string()],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps,
        };

        let spec = render_payload_spec(
            "gatk",
            &parsed,
            "bioconda-gatk-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains(&format!("BuildRequires:  {}", PHOREUS_R_PACKAGE)));
        assert!(spec.contains(&format!("Requires:  {}", PHOREUS_R_PACKAGE)));
        assert!(spec.contains("export R=\"$PHOREUS_R_PREFIX/bin/R\""));
        assert!(spec.contains("export R_LIBS_SITE=\"$R_LIBS\""));
        assert!(!spec.contains("r-ggplot2"));
    }

    #[test]
    fn rust_payload_requires_phoreus_rust_runtime() {
        let mut build_deps = BTreeSet::new();
        build_deps.insert("rust".to_string());
        build_deps.insert("cargo".to_string());

        let parsed = ParsedMeta {
            package_name: "sdust".to_string(),
            version: "1.0".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/sdust-1.0.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/sdust".to_string(),
            license: "MIT".to_string(),
            summary: "sdust".to_string(),
            source_patches: Vec::new(),
            build_script: Some("cargo build --release".to_string()),
            noarch_python: false,
            build_dep_specs_raw: vec!["rust".to_string(), "cargo".to_string()],
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: Vec::new(),
            build_deps,
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let spec = render_payload_spec(
            "sdust",
            &parsed,
            "bioconda-sdust-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains(&format!("BuildRequires:  {}", PHOREUS_RUST_PACKAGE)));
        assert!(spec.contains("export PHOREUS_RUST_PREFIX=/usr/local/phoreus/rust/1.92"));
        assert!(spec.contains("export CARGO_BUILD_JOBS=1"));
    }

    #[test]
    fn nim_payload_requires_phoreus_nim_runtime() {
        let mut build_deps = BTreeSet::new();
        build_deps.insert("nim".to_string());

        let parsed = ParsedMeta {
            package_name: "mosdepth".to_string(),
            version: "0.3.13".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/mosdepth-0.3.13.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://github.com/brentp/mosdepth".to_string(),
            license: "MIT".to_string(),
            summary: "mosdepth".to_string(),
            source_patches: Vec::new(),
            build_script: Some("nimble build".to_string()),
            noarch_python: false,
            build_dep_specs_raw: vec!["nim".to_string()],
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: Vec::new(),
            build_deps,
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let spec = render_payload_spec(
            "mosdepth",
            &parsed,
            "bioconda-mosdepth-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains(&format!("BuildRequires:  {}", PHOREUS_NIM_PACKAGE)));
        assert!(spec.contains("export PHOREUS_NIM_PREFIX=/usr/local/phoreus/nim/2.2"));
        assert!(spec.contains("export NIMBLE_DIR=\"$PREFIX/.nimble\""));
    }

    #[test]
    fn igv_payload_uses_java21_toolchain() {
        let mut host_deps = BTreeSet::new();
        host_deps.insert("openjdk".to_string());
        host_deps.insert("glib".to_string());
        let mut run_deps = BTreeSet::new();
        run_deps.insert("openjdk".to_string());

        let parsed = ParsedMeta {
            package_name: "igv".to_string(),
            version: "2.19.7".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/igv-2.19.7.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://igv.org".to_string(),
            license: "MIT".to_string(),
            summary: "Integrative Genomics Viewer".to_string(),
            source_patches: Vec::new(),
            build_script: Some("./gradlew createDist".to_string()),
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: vec!["openjdk <22".to_string(), "glib".to_string()],
            run_dep_specs_raw: vec!["openjdk <22".to_string()],
            build_deps: BTreeSet::new(),
            host_deps,
            run_deps,
        };

        let spec = render_payload_spec(
            "igv",
            &parsed,
            "bioconda-igv-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains("BuildRequires:  java-21-openjdk-devel"));
        assert!(!spec.contains("BuildRequires:  java-11-openjdk"));
        assert!(spec.contains("Requires:  java-21-openjdk"));
        assert!(spec.contains("export ORG_GRADLE_JAVA_HOME=\"$JAVA_HOME\""));
    }

    #[test]
    fn canu_payload_keeps_boost_runtime_contract() {
        let mut host_deps = BTreeSet::new();
        host_deps.insert("boost-cpp".to_string());
        let mut run_deps = BTreeSet::new();
        run_deps.insert("boost-cpp".to_string());

        let parsed = ParsedMeta {
            package_name: "canu".to_string(),
            version: "2.3".to_string(),
            build_number: "2".to_string(),
            source_url: "https://example.invalid/canu-2.3.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://github.com/marbl/canu".to_string(),
            license: "GPL-2.0-or-later".to_string(),
            summary: "Canu".to_string(),
            source_patches: Vec::new(),
            build_script: Some("make -j${CPU_COUNT}".to_string()),
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: vec!["boost-cpp".to_string()],
            run_dep_specs_raw: vec!["boost-cpp".to_string()],
            build_deps: BTreeSet::new(),
            host_deps,
            run_deps,
        };

        let spec = render_payload_spec(
            "canu",
            &parsed,
            "bioconda-canu-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains("BuildRequires:  boost-devel"));
        assert!(spec.contains("Requires:  boost"));
    }

    #[test]
    fn perl_payload_does_not_promote_run_deps_to_buildrequires() {
        let mut build_deps = BTreeSet::new();
        build_deps.insert("perl".to_string());
        let mut run_deps = BTreeSet::new();
        run_deps.insert("perl-number-compare".to_string());

        let parsed = ParsedMeta {
            package_name: "perl-file-find-rule".to_string(),
            version: "0.35".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/perl-file-find-rule-0.35.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://metacpan.org".to_string(),
            license: "Artistic-1.0-Perl".to_string(),
            summary: "Perl package".to_string(),
            source_patches: Vec::new(),
            build_script: Some("perl Makefile.PL".to_string()),
            noarch_python: false,
            build_dep_specs_raw: vec!["perl".to_string()],
            host_dep_specs_raw: vec!["perl".to_string()],
            run_dep_specs_raw: vec!["perl-number-compare".to_string()],
            build_deps,
            host_deps: BTreeSet::new(),
            run_deps,
        };

        let spec = render_payload_spec(
            "perl-file-find-rule",
            &parsed,
            "bioconda-perl-file-find-rule-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(!spec.contains("BuildRequires:  perl-Number-Compare"));
        assert!(spec.contains("Requires:  perl-Number-Compare"));
    }

    #[test]
    fn perl_payload_keeps_perl_host_buildrequires() {
        let mut build_deps = BTreeSet::new();
        build_deps.insert("make".to_string());
        let mut host_deps = BTreeSet::new();
        host_deps.insert("perl".to_string());
        host_deps.insert("perl-number-compare".to_string());
        host_deps.insert("perl-text-glob".to_string());
        host_deps.insert("perl-extutils-makemaker".to_string());

        let parsed = ParsedMeta {
            package_name: "perl-file-find-rule".to_string(),
            version: "0.35".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/perl-file-find-rule-0.35.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://metacpan.org".to_string(),
            license: "perl_5".to_string(),
            summary: "Perl package".to_string(),
            source_patches: Vec::new(),
            build_script: Some("perl Makefile.PL".to_string()),
            noarch_python: false,
            build_dep_specs_raw: vec!["make".to_string()],
            host_dep_specs_raw: vec![
                "perl".to_string(),
                "perl-number-compare".to_string(),
                "perl-text-glob".to_string(),
                "perl-extutils-makemaker".to_string(),
            ],
            run_dep_specs_raw: vec![
                "perl".to_string(),
                "perl-number-compare".to_string(),
                "perl-text-glob".to_string(),
            ],
            build_deps,
            host_deps,
            run_deps: BTreeSet::new(),
        };

        let spec = render_payload_spec(
            "perl-file-find-rule",
            &parsed,
            "bioconda-perl-file-find-rule-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(spec.contains("BuildRequires:  perl"));
        assert!(spec.contains("BuildRequires:  perl-ExtUtils-MakeMaker"));
        assert!(spec.contains("BuildRequires:  perl-Number-Compare"));
        assert!(spec.contains("BuildRequires:  perl-text-glob"));
        assert!(spec.contains("lib64/perl5"));
    }

    #[test]
    fn build_script_python_detection_works_for_common_patterns() {
        assert!(script_text_indicates_python(
            "#!/bin/bash\npython -m pip install . --no-deps\n"
        ));
        assert!(script_text_indicates_python(
            "#!/bin/bash\npython setup.py install\n"
        ));
        assert!(!script_text_indicates_python(
            "#!/bin/bash\nmake -j${CPU_COUNT}\n"
        ));
    }

    #[test]
    fn fallback_build_script_supports_metapackage_runtime_only_recipes() {
        let mut run_deps = BTreeSet::new();
        run_deps.insert("snakemake-minimal".to_string());
        let parsed = ParsedMeta {
            package_name: "snakemake".to_string(),
            version: "9.16.3".to_string(),
            build_number: "0".to_string(),
            source_url: String::new(),
            source_folder: String::new(),
            homepage: "https://snakemake.github.io".to_string(),
            license: "MIT".to_string(),
            summary: "meta package".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: vec!["snakemake-minimal".to_string()],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps,
        };
        let generated = synthesize_fallback_build_sh(&parsed).expect("metapackage fallback");
        assert!(generated.contains("metapackage fallback"));
    }

    #[test]
    fn runtime_only_metapackage_does_not_promote_run_deps_to_buildrequires() {
        let mut run_deps = BTreeSet::new();
        run_deps.insert("snakemake-minimal".to_string());
        run_deps.insert("pandas".to_string());
        let parsed = ParsedMeta {
            package_name: "snakemake".to_string(),
            version: "9.16.3".to_string(),
            build_number: "0".to_string(),
            source_url: String::new(),
            source_folder: String::new(),
            homepage: "https://snakemake.github.io".to_string(),
            license: "MIT".to_string(),
            summary: "meta package".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: vec!["snakemake-minimal".to_string(), "pandas".to_string()],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps,
        };
        let spec = render_payload_spec(
            "snakemake",
            &parsed,
            "bioconda-snakemake-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(!spec.contains("BuildRequires:  snakemake-minimal"));
        assert!(!spec.contains("BuildRequires:  pandas"));
        assert!(spec.contains("Requires:  snakemake-minimal"));
        assert!(spec.contains("Requires:  pandas"));
        assert!(!spec.contains("Source0:"));
    }

    #[test]
    fn harden_build_script_rewrites_streamed_wget_tar() {
        let raw = "#!/usr/bin/env bash\nwget -O- https://example.invalid/src.tar.gz | tar -zxf -\n";
        let hardened = harden_build_script_text(raw);
        assert!(hardened.contains("BIOCONDA2RPM_FETCH_0_ARCHIVE"));
        assert!(hardened.contains("wget --no-verbose -O \"${BIOCONDA2RPM_FETCH_0_ARCHIVE}\""));
        assert!(hardened.contains("tar -zxf \"${BIOCONDA2RPM_FETCH_0_ARCHIVE}\""));
        assert!(!hardened.contains("wget -O- https://example.invalid/src.tar.gz | tar -zxf -"));
    }

    #[test]
    fn harden_build_script_neutralizes_cargo_bundle_licenses() {
        let raw = "cargo-bundle-licenses --format yaml --output THIRDPARTY.yml\n";
        let hardened = harden_build_script_text(raw);
        assert!(hardened.contains("Skipping cargo-bundle-licenses"));
        assert!(!hardened.contains("cargo-bundle-licenses --format yaml --output THIRDPARTY.yml"));
    }

    #[test]
    fn harden_build_script_rewrites_glob_copy_to_prefix_bin() {
        let raw = "mkdir -p $PREFIX/bin\ncp *.R $PREFIX/bin\ncp *.sh $PREFIX/bin\n";
        let hardened = harden_build_script_text(raw);
        assert!(hardened.contains("find . -maxdepth 2 -type f -name '*.R' -print0"));
        assert!(hardened.contains("find . -maxdepth 2 -type f -name '*.sh' -print0"));
    }

    #[test]
    fn git_sources_clone_in_prep_and_skip_source0() {
        let parsed = ParsedMeta {
            package_name: "ont_vbz_hdf_plugin".to_string(),
            version: "1.0.12".to_string(),
            build_number: "0".to_string(),
            source_url: "git+https://github.com/nanoporetech/vbz_compression.git#1.0.12"
                .to_string(),
            source_folder: String::new(),
            homepage: "https://github.com/nanoporetech".to_string(),
            license: "MPL-2".to_string(),
            summary: "vbz".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: Vec::new(),
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };
        let spec = render_payload_spec(
            "ont-vbz-hdf-plugin",
            &parsed,
            "bioconda-ont-vbz-hdf-plugin-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(!spec.contains("Source0:"));
        assert!(spec.contains("BuildRequires:  git"));
        assert!(spec.contains("git clone --recursive \"$git_url\" buildsrc"));
    }

    #[test]
    fn tail_lines_omits_transfer_progress_rows() {
        let log = "100K ..........  10% 100M 0s\n\
fatal: meaningful failure\n\
200K ..........  20% 100M 0s\n\
error: build stopped\n";
        let tail = tail_lines(log, 5);
        assert!(!tail.contains(".........."));
        assert!(tail.contains("fatal: meaningful failure"));
        assert!(tail.contains("error: build stopped"));
    }

    #[test]
    fn classify_arch_policy_detects_k8_precompiled_gap_on_aarch64() {
        let log = "no upstream precompiled k8 binary for Linux/aarch64; available entries: k8-x86_64-Linux,k8-arm64-Darwin";
        assert_eq!(classify_arch_policy(log, "aarch64"), Some("amd64_only"));
    }

    #[test]
    fn version_compare_prefers_higher_subdir() {
        let tmp = TempDir::new().expect("create temp dir");
        let recipe = tmp.path().join("blast");
        fs::create_dir_all(recipe.join("2.2.31")).expect("create dir");
        fs::create_dir_all(recipe.join("2.5.0")).expect("create dir");
        fs::write(
            recipe.join("2.2.31/meta.yaml"),
            "package: {name: blast, version: 2.2.31}",
        )
        .expect("write meta");
        fs::write(
            recipe.join("2.5.0/meta.yaml"),
            "package: {name: blast, version: 2.5.0}",
        )
        .expect("write meta");

        let picked = select_recipe_variant_dir(&recipe).expect("select variant");
        assert!(picked.ends_with("2.5.0"));
    }

    #[test]
    fn variant_selection_prefers_newer_root_meta_version() {
        let tmp = TempDir::new().expect("create temp dir");
        let recipe = tmp.path().join("blast");
        fs::create_dir_all(recipe.join("2.5.0")).expect("create dir");
        fs::write(
            recipe.join("meta.yaml"),
            r#"
{% set version = "2.17.0" %}
package:
  name: blast
  version: {{ version }}
"#,
        )
        .expect("write root meta");
        fs::write(
            recipe.join("2.5.0/meta.yaml"),
            "package: {name: blast, version: 2.5.0}",
        )
        .expect("write subdir meta");

        let picked = select_recipe_variant_dir(&recipe).expect("select variant");
        assert_eq!(picked, recipe);
    }

    #[test]
    fn render_meta_handles_common_jinja_helpers() {
        let src = r#"
{% set name = "bwa" %}
{% set version = "0.7.19" %}
package:
  name: {{ name }}
  version: {{ version }}
requirements:
  build:
    - {{ compiler('c') }}
  run:
    - {{ pin_subpackage(name, max_pin="x.x") }}
"#;
        let rendered = render_meta_yaml(src).expect("render jinja");
        assert!(rendered.contains("bwa"));
        assert!(rendered.contains("c-compiler"));
    }

    #[test]
    fn fallback_recipe_selection_prefers_direct_prefix_match() {
        let tmp = TempDir::new().expect("create temp dir");
        let recipes = vec![
            RecipeDir {
                name: "r-seurat-data".to_string(),
                normalized: normalize_name("r-seurat-data"),
                path: tmp.path().join("r-seurat-data"),
            },
            RecipeDir {
                name: "r-seurat-disk".to_string(),
                normalized: normalize_name("r-seurat-disk"),
                path: tmp.path().join("r-seurat-disk"),
            },
            RecipeDir {
                name: "seurat-scripts".to_string(),
                normalized: normalize_name("seurat-scripts"),
                path: tmp.path().join("seurat-scripts"),
            },
        ];

        let selected = select_fallback_recipe("seurat", &recipes).expect("fallback recipe");
        assert_eq!(selected.name, "seurat-scripts");
    }

    #[test]
    fn fallback_recipe_selection_prefers_scripts_over_other_prefix_matches() {
        let tmp = TempDir::new().expect("create temp dir");
        let recipes = vec![
            RecipeDir {
                name: "scanpy-cli".to_string(),
                normalized: normalize_name("scanpy-cli"),
                path: tmp.path().join("scanpy-cli"),
            },
            RecipeDir {
                name: "scanpy-scripts".to_string(),
                normalized: normalize_name("scanpy-scripts"),
                path: tmp.path().join("scanpy-scripts"),
            },
        ];

        let selected = select_fallback_recipe("scanpy", &recipes).expect("fallback recipe");
        assert_eq!(selected.name, "scanpy-scripts");
    }

    #[test]
    fn render_meta_supports_environ_prefix_lookup() {
        let src = r#"
package:
  name: bioconductor-edger
  version: "4.4.0"
about:
  license_file: '{{ environ["PREFIX"] }}/lib/R/share/licenses/GPL-3'
"#;
        let rendered = render_meta_yaml(src).expect("render jinja with environ");
        assert!(rendered.contains("$PREFIX/lib/R/share/licenses/GPL-3"));
    }

    #[test]
    fn render_meta_supports_src_dir_lookup() {
        let src = r#"
build:
  script: "{{ PYTHON }} -m pip install {{ SRC_DIR }}/scanpy-scripts --no-deps"
"#;
        let rendered = render_meta_yaml(src).expect("render jinja with SRC_DIR");
        assert!(rendered.contains("$SRC_DIR/scanpy-scripts"));
    }

    #[test]
    fn render_meta_supports_cran_mirror_variable() {
        let src = r#"
source:
  url: "{{ cran_mirror }}/src/contrib/restfulr_0.0.16.tar.gz"
"#;
        let rendered = render_meta_yaml(src).expect("render jinja with cran_mirror");
        assert!(rendered.contains("https://cran.r-project.org/src/contrib/restfulr_0.0.16.tar.gz"));
    }

    #[test]
    fn spec_escape_flattens_multiline_values() {
        let escaped = spec_escape("Line one\nLine two\t  with   spaces");
        assert_eq!(escaped, "Line one Line two with spaces");
    }

    #[test]
    fn selector_filter_keeps_matching_lines() {
        let ctx = SelectorContext {
            linux: true,
            osx: false,
            win: false,
            aarch64: false,
            arm64: false,
            x86_64: true,
            py_major: 3,
            py_minor: 11,
        };

        let text = "url: http://linux.example # [linux]\nurl: http://osx.example # [osx]\n";
        let filtered = apply_selectors(text, &ctx);
        assert!(filtered.contains("linux.example"));
        assert!(!filtered.contains("osx.example"));
    }

    #[test]
    fn selector_arm64_is_distinct_from_linux_aarch64() {
        let ctx = SelectorContext {
            linux: true,
            osx: false,
            win: false,
            aarch64: true,
            arm64: false,
            x86_64: false,
            py_major: 3,
            py_minor: 11,
        };

        let text = "dep: nim # [not arm64]\n\
dep: linux-aarch64-only # [aarch64]\n\
dep: osx-arm64-only # [arm64]\n";
        let filtered = apply_selectors(text, &ctx);
        assert!(filtered.contains("dep: nim"));
        assert!(filtered.contains("dep: linux-aarch64-only"));
        assert!(!filtered.contains("dep: osx-arm64-only"));
    }
}
