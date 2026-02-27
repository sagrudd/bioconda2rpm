use crate::cli::{BuildArgs, DependencyPolicy, GeneratePrioritySpecsArgs, MissingDependencyPolicy};
use anyhow::{Context, Result};
use chrono::Utc;
use csv::{ReaderBuilder, Writer};
use minijinja::{Environment, context, value::Kwargs};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

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

#[derive(Debug, Clone)]
struct ParsedMeta {
    package_name: String,
    version: String,
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
struct BuildConfig {
    topdir: PathBuf,
    reports_dir: PathBuf,
    container_engine: String,
    container_image: String,
    host_arch: String,
}

const PHOREUS_PYTHON_VERSION: &str = "3.11";
const PHOREUS_PYTHON_PACKAGE: &str = "phoreus-python-3.11";
const REFERENCE_PYTHON_SPECS_DIR: &str = "../software_query/rpm/python/specs";

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

    let Some((root_resolved, root_parsed)) =
        resolve_and_parse_recipe(&args.package, &args.recipe_root, &recipe_dirs, true)?
    else {
        anyhow::bail!(
            "no overlapping recipe found in bioconda metadata for '{}'",
            args.package
        );
    };
    let root_slug = normalize_name(&root_resolved.recipe_name);
    if let PayloadVersionState::UpToDate { existing_version } =
        payload_version_state(&topdir, &root_slug, &root_parsed.version)?
    {
        clear_quarantine_note(&bad_spec_dir, &root_slug);
        let reason = format!(
            "already up-to-date: bioconda version {} already built (latest local payload version {})",
            root_parsed.version, existing_version
        );
        let entry = ReportEntry {
            software: root_resolved.recipe_name.clone(),
            priority: 0,
            status: "up-to-date".to_string(),
            reason,
            overlap_recipe: root_resolved.recipe_name.clone(),
            overlap_reason: "requested-root".to_string(),
            variant_dir: root_resolved.variant_dir.display().to_string(),
            package_name: root_parsed.package_name.clone(),
            version: root_parsed.version.clone(),
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
            build_order: vec![root_resolved.recipe_name],
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
    )?;
    let build_order = plan_order
        .iter()
        .filter_map(|key| plan_nodes.get(key).map(|node| node.name.clone()))
        .collect::<Vec<_>>();

    let mut built = HashSet::new();
    let mut results = Vec::new();
    let mut fail_reason: Option<String> = None;

    for key in &plan_order {
        let Some(node) = plan_nodes.get(key) else {
            continue;
        };

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
        );
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

    if let Some(reason) = fail_reason {
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
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
    nodes: &mut BTreeMap<String, BuildPlanNode>,
    order: &mut Vec<String>,
) -> Result<Option<String>> {
    let resolved_and_parsed =
        match resolve_and_parse_recipe(query, recipe_root, recipe_dirs, is_root) {
            Ok(v) => v,
            Err(err) => {
                if is_root {
                    return Err(err);
                }
                return Ok(None);
            }
        };

    let Some((resolved, parsed)) = resolved_and_parsed else {
        if is_root {
            anyhow::bail!(
                "no overlapping recipe found in bioconda metadata for '{}'",
                query
            );
        }
        return Ok(None);
    };

    let canonical = normalize_name(&resolved.recipe_name);
    if !is_root && !is_buildable_recipe(&resolved, &parsed) {
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
        for dep in selected_dependency_set(&parsed, policy, is_root) {
            if dep == canonical {
                continue;
            }
            if map_perl_core_dependency(&dep).is_some() {
                continue;
            }
            if let Some(dep_key) = visit_build_plan_node(
                &dep,
                false,
                with_deps,
                policy,
                recipe_root,
                recipe_dirs,
                visiting,
                visited,
                nodes,
                order,
            )? {
                bioconda_deps.insert(dep_key);
            }
        }
    }

    visiting.remove(&canonical);
    visited.insert(canonical.clone());
    nodes.insert(
        canonical.clone(),
        BuildPlanNode {
            name: resolved.recipe_name,
            direct_bioconda_deps: bioconda_deps,
        },
    );
    order.push(canonical.clone());
    Ok(Some(canonical))
}

fn is_buildable_recipe(resolved: &ResolvedRecipe, parsed: &ParsedMeta) -> bool {
    (resolved.build_sh_path.is_some() || parsed.build_script.is_some())
        && !parsed.source_url.trim().is_empty()
}

fn selected_dependency_set(
    parsed: &ParsedMeta,
    policy: &DependencyPolicy,
    is_root: bool,
) -> BTreeSet<String> {
    if is_python_recipe(parsed) {
        let mut out = BTreeSet::new();
        out.extend(
            parsed
                .build_deps
                .iter()
                .filter(|dep| should_keep_rpm_dependency_for_python(dep))
                .cloned(),
        );
        out.extend(
            parsed
                .host_deps
                .iter()
                .filter(|dep| should_keep_rpm_dependency_for_python(dep))
                .cloned(),
        );
        out.extend(
            parsed
                .run_deps
                .iter()
                .filter(|dep| should_keep_rpm_dependency_for_python(dep))
                .cloned(),
        );
        return out;
    }

    match policy {
        DependencyPolicy::RunOnly => parsed.run_deps.clone(),
        DependencyPolicy::BuildHostRun => {
            let mut out = BTreeSet::new();
            out.extend(parsed.build_deps.iter().cloned());
            out.extend(parsed.host_deps.iter().cloned());
            out.extend(parsed.run_deps.iter().cloned());
            out
        }
        DependencyPolicy::RuntimeTransitiveRootBuildHost => {
            if is_root {
                let mut out = BTreeSet::new();
                out.extend(parsed.build_deps.iter().cloned());
                out.extend(parsed.host_deps.iter().cloned());
                out.extend(parsed.run_deps.iter().cloned());
                out
            } else {
                parsed.run_deps.clone()
            }
        }
    }
}

fn resolve_and_parse_recipe(
    tool_name: &str,
    recipe_root: &Path,
    recipe_dirs: &[RecipeDir],
    allow_identifier_lookup: bool,
) -> Result<Option<(ResolvedRecipe, ParsedMeta)>> {
    let Some(resolved) =
        resolve_recipe_for_tool_mode(tool_name, recipe_root, recipe_dirs, allow_identifier_lookup)?
    else {
        return Ok(None);
    };
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
    let parsed = parse_rendered_meta(&rendered).with_context(|| {
        format!(
            "failed to parse rendered metadata for {}",
            resolved.meta_path.display()
        )
    })?;
    Ok(Some((resolved, parsed)))
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

    let meta_text = match fs::read_to_string(&resolved.meta_path) {
        Ok(v) => v,
        Err(err) => {
            let reason = format!(
                "failed to read metadata {}: {err}",
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
                package_name: String::new(),
                version: String::new(),
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            };
        }
    };

    let selector_ctx = SelectorContext::for_rpm_build();
    let selected_meta = apply_selectors(&meta_text, &selector_ctx);

    let rendered = match render_meta_yaml(&selected_meta) {
        Ok(v) => v,
        Err(err) => {
            let reason = format!("failed to render Jinja in metadata: {err}");
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

    let parsed = match parse_rendered_meta(&rendered) {
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

    if let Some(build_sh_path) = resolved.build_sh_path.as_ref() {
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

    if allow_identifier_lookup {
        let key = normalize_identifier_key(&lower);
        if let Some(recipe) = find_recipe_by_identifier(recipe_root, &key)? {
            return build_resolved(&recipe, "identifier-match");
        }
    }

    Ok(None)
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

    let template = env
        .template_from_str(meta)
        .context("creating jinja template from meta.yaml")?;

    template
        .render(context! {
            PYTHON => "$PYTHON",
            PIP => "$PIP",
            PREFIX => "$PREFIX",
            RECIPE_DIR => "$RECIPE_DIR",
            R => "R",
        })
        .context("rendering meta.yaml jinja template")
}

#[derive(Debug, Clone, Copy)]
struct SelectorContext {
    linux: bool,
    osx: bool,
    win: bool,
    aarch64: bool,
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
        let x86_64 = arch == "x86_64" || arch == "amd64";
        Self {
            linux,
            osx,
            win,
            aarch64,
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
        "aarch64" | "arm64" => ctx.aarch64,
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

fn is_python_recipe(parsed: &ParsedMeta) -> bool {
    if parsed.noarch_python {
        return true;
    }
    if parsed.build_deps.contains(PHOREUS_PYTHON_PACKAGE)
        || parsed.host_deps.contains(PHOREUS_PYTHON_PACKAGE)
        || parsed.run_deps.contains(PHOREUS_PYTHON_PACKAGE)
    {
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

fn build_python_requirements(parsed: &ParsedMeta) -> Vec<String> {
    let mut out = BTreeSet::new();
    for raw in parsed
        .build_dep_specs_raw
        .iter()
        .chain(parsed.host_dep_specs_raw.iter())
        .chain(parsed.run_dep_specs_raw.iter())
    {
        if let Some(req) = conda_dep_to_pip_requirement(raw) {
            out.insert(req);
        }
    }
    out.into_iter().collect()
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
    let name_token = parts.next()?;
    let normalized = name_token.replace('_', "-").to_lowercase();
    if is_phoreus_python_toolchain_dependency(&normalized) {
        return None;
    }
    if !is_python_ecosystem_dependency_name(&normalized) {
        return None;
    }

    let pip_name = match normalized.as_str() {
        "python-kaleido" => "kaleido".to_string(),
        other => other.to_string(),
    };

    let remainder = cleaned[name_token.len()..].trim();
    if remainder.is_empty() {
        return Some(pip_name);
    }

    let spec_token = remainder
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim();
    if spec_token.is_empty() {
        return Some(pip_name);
    }

    let requirement = if spec_token.starts_with(['>', '<', '=', '!', '~']) {
        format!("{pip_name}{spec_token}")
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

fn should_keep_rpm_dependency_for_python(dep: &str) -> bool {
    let normalized = dep.trim().replace('_', "-").to_lowercase();
    !is_python_ecosystem_dependency_name(&normalized)
}

fn is_python_ecosystem_dependency_name(normalized: &str) -> bool {
    if is_phoreus_python_toolchain_dependency(normalized) {
        return true;
    }

    if matches!(
        normalized,
        "gcc"
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

fn staged_build_script_indicates_python(path: &Path) -> Result<bool> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading staged build script {}", path.display()))?;
    Ok(script_text_indicates_python(&text))
}

fn script_text_indicates_python(script: &str) -> bool {
    let lower = script.to_lowercase();
    lower.contains("pip install")
        || lower.contains("python -m pip")
        || lower.contains("python3 -m pip")
        || lower.contains("python setup.py")
        || lower.contains("setup.py install")
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
        Some(Value::Mapping(map)) => map
            .get(Value::String("url".to_string()))
            .and_then(value_to_string),
        Some(Value::Sequence(seq)) => seq.iter().find_map(|item| {
            item.as_mapping()
                .and_then(|m| m.get(Value::String("url".to_string())))
                .and_then(value_to_string)
        }),
        Some(Value::String(s)) => Some(s.to_string()),
        _ => None,
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
        .map(|t| t.trim_matches(','))
        .unwrap_or_default();

    if token.is_empty() {
        return None;
    }

    let normalized = token.replace('_', "-").to_lowercase();
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
    let source_relsubdir = {
        let folder = parsed.source_folder.trim().trim_matches('/');
        if folder.is_empty() {
            ".".to_string()
        } else {
            folder.to_string()
        }
    };
    // Python policy is applied when either metadata or staged build script indicates
    // Python packaging/install semantics.
    let python_recipe = is_python_recipe(parsed) || python_script_hint;
    let python_requirements = if python_recipe {
        build_python_requirements(parsed)
    } else {
        Vec::new()
    };
    let python_venv_setup = render_python_venv_setup_block(python_recipe, &python_requirements);
    let module_lua_env = render_module_lua_env_block(python_recipe);

    let mut build_requires = BTreeSet::new();
    build_requires.insert("bash".to_string());
    // Enforce canonical builder policy: every payload build uses Phoreus Python,
    // never the system interpreter.
    build_requires.insert(PHOREUS_PYTHON_PACKAGE.to_string());
    build_requires.extend(
        parsed
            .build_deps
            .iter()
            .filter(|dep| !python_recipe || should_keep_rpm_dependency_for_python(dep))
            .map(|d| map_build_dependency(d)),
    );
    build_requires.extend(
        parsed
            .host_deps
            .iter()
            .filter(|dep| !python_recipe || should_keep_rpm_dependency_for_python(dep))
            .map(|d| map_build_dependency(d)),
    );
    if !python_recipe {
        build_requires.extend(parsed.run_deps.iter().map(|d| map_build_dependency(d)));
    }

    let mut runtime_requires = BTreeSet::new();
    runtime_requires.insert("phoreus".to_string());
    if python_recipe {
        runtime_requires.insert(PHOREUS_PYTHON_PACKAGE.to_string());
        runtime_requires.extend(
            parsed
                .run_deps
                .iter()
                .filter(|dep| should_keep_rpm_dependency_for_python(dep))
                .map(|d| map_runtime_dependency(d)),
        );
    } else {
        runtime_requires.extend(parsed.run_deps.iter().map(|d| map_runtime_dependency(d)));
    }

    let build_requires_lines = format_dep_lines("BuildRequires", &build_requires);
    let requires_lines = format_dep_lines("Requires", &runtime_requires);
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
\n\
Name:           phoreus-%{{tool}}-%{{upstream_version}}\n\
Version:        %{{upstream_version}}\n\
Release:        1%{{?dist}}\n\
Provides:       %{{tool}} = %{{version}}-%{{release}}\n\
Summary:        {summary}\n\
License:        {license}\n\
URL:            {homepage}\n\
{build_arch}\
Source0:        {source_url}\n\
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
rm -rf buildsrc\n\
mkdir -p %{{bioconda_source_subdir}}\n\
tar -xf %{{SOURCE0}} -C %{{bioconda_source_subdir}} --strip-components=1\n\
cp %{{SOURCE1}} buildsrc/build.sh\n\
chmod 0755 buildsrc/build.sh\n\
{patch_apply}\
\n\
%build\n\
cd buildsrc\n\
%ifarch aarch64\n\
export BIOCONDA_TARGET_ARCH=aarch64\n\
%else\n\
export BIOCONDA_TARGET_ARCH=x86_64\n\
%endif\n\
export CPU_COUNT=1\n\
export MAKEFLAGS=-j1\n\
\n\
%install\n\
rm -rf %{{buildroot}}\n\
mkdir -p %{{buildroot}}%{{phoreus_prefix}}\n\
cd buildsrc\n\
export PREFIX=%{{buildroot}}%{{phoreus_prefix}}\n\
export SRC_DIR=${{SRC_DIR:-$(pwd)/%{{bioconda_source_relsubdir}}}}\n\
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
export CFLAGS=\"${{CFLAGS:-}}\"\n\
export CXXFLAGS=\"${{CXXFLAGS:-}}\"\n\
export CPPFLAGS=\"${{CPPFLAGS:-}}\"\n\
export LDFLAGS=\"${{LDFLAGS:-}}\"\n\
export AR=\"${{AR:-ar}}\"\n\
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
export PERL_MM_OPT=\"${{PERL_MM_OPT:+$PERL_MM_OPT }}INSTALL_BASE=$PREFIX\"\n\
export PERL_MB_OPT=\"${{PERL_MB_OPT:+$PERL_MB_OPT }}--install_base $PREFIX\"\n\
\n\
# Make locally installed Phoreus Perl dependency trees visible during build.\n\
if [[ -d /usr/local/phoreus ]]; then\n\
while IFS= read -r -d '' perl_lib; do\n\
  case \":${{PERL5LIB:-}}:\" in\n\
    *\":$perl_lib:\"*) ;;\n\
    *) export PERL5LIB=\"$perl_lib${{PERL5LIB:+:$PERL5LIB}}\" ;;\n\
  esac\n\
done < <(find /usr/local/phoreus -maxdepth 6 -type d -path '*/lib/perl5*' -print0 2>/dev/null)\n\
fi\n\
\n\
# Ensure common install subdirectories exist for build.sh scripts that assume them.\n\
mkdir -p \"$PREFIX/lib\" \"$PREFIX/bin\"\n\
\n\
{python_venv_setup}\
\n\
# BLAST recipes in Bioconda assume a conda-style shared prefix where ncbi-vdb\n\
# lives under the same PREFIX. In Phoreus, ncbi-vdb is a separate payload.\n\
# Retarget the generated build.sh argument to the newest installed ncbi-vdb prefix.\n\
if [[ \"%{{tool}}\" == \"blast\" ]]; then\n\
vdb_prefix=$(find /usr/local/phoreus/ncbi-vdb -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort | tail -n 1 || true)\n\
if [[ -n \"$vdb_prefix\" ]]; then\n\
  sed -i 's|--with-vdb=$PREFIX|--with-vdb='\\\"$vdb_prefix\\\"'|g' ./build.sh\n\
fi\n\
# BLAST's Bioconda script hard-codes n_workers=8 on aarch64, which has shown\n\
# unstable flat-make behavior in containerized RPM builds. Enforce single-core\n\
# policy so the orchestrator remains deterministic across all architectures.\n\
export CPU_COUNT=1\n\
sed -i 's|n_workers=8|n_workers=${{CPU_COUNT:-1}}|g' ./build.sh\n\
fi\n\
\n\
# A number of upstream scripts hardcode aggressive THREADS values;\n\
# force single-core policy for deterministic container builds.\n\
sed -i -E 's/THREADS=\"-j[0-9]+\"/THREADS=\"-j1\"/g' ./build.sh || true\n\
\n\
# Capture a pristine buildsrc snapshot so serial retries run from a clean tree,\n\
# not from a partially mutated/failed first attempt.\n\
retry_snapshot=\"$(pwd)/.bioconda2rpm-retry-snapshot.tar\"\n\
rm -f \"$retry_snapshot\"\n\
tar --exclude='.bioconda2rpm-retry-snapshot.tar' -cf \"$retry_snapshot\" .\n\
\n\
# Canonical fallback for flaky parallel builds: retry once serially.\n\
# Enforce fail-fast shell behavior for staged recipe scripts so downstream\n\
# commands do not mask the primary failure reason.\n\
if ! bash -eo pipefail ./build.sh; then\n\
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
        summary = summary,
        license = license,
        homepage = homepage,
        source_url = source_url,
        build_sh = spec_escape(staged_build_sh_name),
        patch_sources = patch_source_lines,
        patch_apply = patch_apply_lines,
        build_requires = build_requires_lines,
        requires = requires_lines,
        build_arch = build_arch_line,
        python_venv_setup = python_venv_setup,
        module_lua_env = module_lua_env,
        changelog_date = changelog_date,
        meta_path = spec_escape(&meta_path.display().to_string()),
        variant_dir = spec_escape(&variant_dir.display().to_string()),
        phoreus_python_version = PHOREUS_PYTHON_VERSION,
    )
}

fn render_python_venv_setup_block(python_recipe: bool, python_requirements: &[String]) -> String {
    if !python_recipe {
        return String::new();
    }

    let requirements_install = if python_requirements.is_empty() {
        String::new()
    } else {
        let requirements_body = python_requirements.join("\n");
        format!(
            "cat > requirements.in <<'REQEOF'\n\
{requirements_body}\n\
REQEOF\n\
\"$PIP\" install pip-tools\n\
pip-compile --generate-hashes requirements.in --output-file requirements.lock\n\
\"$PIP\" install --require-hashes -r requirements.lock\n",
            requirements_body = requirements_body
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

fn render_module_lua_env_block(python_recipe: bool) -> String {
    if python_recipe {
        "setenv(\"VIRTUAL_ENV\", pathJoin(prefix, \"venv\"))\n\
prepend_path(\"PATH\", pathJoin(prefix, \"venv/bin\"))\n\
prepend_path(\"LD_LIBRARY_PATH\", pathJoin(prefix, \"lib\"))\n"
            .to_string()
    } else {
        "prepend_path(\"PATH\", pathJoin(prefix, \"bin\"))\n\
prepend_path(\"LD_LIBRARY_PATH\", pathJoin(prefix, \"lib\"))\n\
prepend_path(\"MANPATH\", pathJoin(prefix, \"share/man\"))\n"
            .to_string()
    }
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
                "(patch -p1 -i %{{SOURCE{}}} || patch -p0 -i %{{SOURCE{}}})\n",
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
    input.replace('%', "%%").trim().to_string()
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

fn normalize_identifier_key(name: &str) -> String {
    normalize_name(name).replace("-plus", "")
}

fn rpm_changelog_date() -> String {
    Utc::now().format("%a %b %d %Y").to_string()
}

fn map_build_dependency(dep: &str) -> String {
    if let Some(mapped) = map_perl_core_dependency(dep) {
        return mapped;
    }
    if is_phoreus_python_toolchain_dependency(dep) {
        return PHOREUS_PYTHON_PACKAGE.to_string();
    }
    match dep {
        "boost-cpp" => "boost-devel".to_string(),
        "go-compiler" => "golang".to_string(),
        other => other.to_string(),
    }
}

fn map_runtime_dependency(dep: &str) -> String {
    if let Some(mapped) = map_perl_core_dependency(dep) {
        return mapped;
    }
    if is_phoreus_python_toolchain_dependency(dep) {
        return PHOREUS_PYTHON_PACKAGE.to_string();
    }
    match dep {
        "boost-cpp" => "boost".to_string(),
        other => other.to_string(),
    }
}

fn is_phoreus_python_toolchain_dependency(dep: &str) -> bool {
    let normalized = dep.trim().replace('_', "-").to_lowercase();
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
        "perl-pathtools" => "perl-PathTools",
        "perl-test-harness" => "perl-Test-Harness",
        "perl-test-simple" => "perl-Test-Simple",
        "perl-module-load" => "perl-Module-Load",
        "perl-params-check" => "perl-Params-Check",
        "perl-test-more" => "perl-Test-Simple",
        "perl-storable" => "perl-Storable",
        "perl-encode" => "perl-Encode",
        "perl-exporter-tiny" => "perl-Exporter-Tiny",
        "perl-test-leaktrace" => "perl-Test-LeakTrace",
        "perl-canary-stability" => "perl-Canary-Stability",
        "perl-types-serialiser" => "perl-Types-Serialiser",
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
spectool_ok=0\n\
for attempt in 1 2 3; do\n\
  if spectool -g -R --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' '{spec}'; then\n\
    spectool_ok=1\n\
    break\n\
  fi\n\
  sleep $((attempt * 2))\n\
done\n\
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
    elif rpm -Uvh --nodeps --force \"$local_rpm\" >\"$dep_log\" 2>&1; then\n\
      if rpm -q --whatprovides \"$dep\" >/dev/null 2>&1; then\n\
        provider=$(rpm -q --whatprovides \"$dep\" | head -n 1 || true)\n\
        emit_depgraph \"$dep\" 'resolved' 'local_rpm' \"$provider\" \"installed_nodeps_from_$(basename \"$local_rpm\")\"\n\
        continue\n\
      fi\n\
    fi\n\
  fi\n\
\n\
  if pm_install \"$dep\" >\"$dep_log\" 2>&1; then\n\
    provider=$(rpm -q --whatprovides \"$dep\" | head -n 1 || true)\n\
    emit_depgraph \"$dep\" 'resolved' 'repo' \"$provider\" 'installed_from_repo'\n\
  else\n\
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

    let run_once = || -> Result<std::process::Output> {
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
            .arg(&script)
            .output()
            .with_context(|| {
                format!(
                    "running container build chain for {} using image {}",
                    spec_name, build_config.container_image
                )
            })
    };

    let mut output = run_once()?;
    let mut combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() && is_source_permission_denied(&combined) {
        fix_host_source_permissions(&build_config.topdir.join("SOURCES"))?;
        output = run_once()?;
        combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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

    let logs_dir = build_config.reports_dir.join("build_logs");
    fs::create_dir_all(&logs_dir)
        .with_context(|| format!("creating build logs dir {}", logs_dir.display()))?;
    let log_path = logs_dir.join(format!("{}.log", sanitize_label(&build_label)));
    fs::write(&log_path, &combined)
        .with_context(|| format!("writing build log {}", log_path.display()))?;

    if !output.status.success() {
        let arch_policy =
            classify_arch_policy(&combined, &build_config.host_arch).unwrap_or("unknown");
        let tail = tail_lines(&combined, 20);
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
            "container build chain failed for {} (exit status: {}) arch_policy={} log={} tail={}{}",
            spec_name,
            output.status,
            arch_policy,
            log_path.display(),
            tail,
            dep_hint
        );
    }

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
    }

    #[test]
    fn dependency_mapping_handles_conda_aliases() {
        assert_eq!(map_build_dependency("boost-cpp"), "boost-devel".to_string());
        assert_eq!(map_runtime_dependency("boost-cpp"), "boost".to_string());
        assert_eq!(
            map_build_dependency("perl-canary-stability"),
            "perl-Canary-Stability".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-types-serialiser"),
            "perl-Types-Serialiser".to_string()
        );
        assert_eq!(
            map_build_dependency("python"),
            PHOREUS_PYTHON_PACKAGE.to_string()
        );
        assert_eq!(
            map_runtime_dependency("python"),
            PHOREUS_PYTHON_PACKAGE.to_string()
        );
        assert_eq!(
            map_build_dependency("setuptools"),
            PHOREUS_PYTHON_PACKAGE.to_string()
        );
        assert_eq!(
            map_runtime_dependency("setuptools"),
            PHOREUS_PYTHON_PACKAGE.to_string()
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
        );
        assert!(spec.contains("Source2:"));
        assert!(spec.contains("patch -p1 -i %{SOURCE2}"));
        assert!(spec.contains("bash -eo pipefail ./build.sh"));
        assert!(spec.contains("retry_snapshot=\"$(pwd)/.bioconda2rpm-retry-snapshot.tar\""));
        assert!(spec.contains("export CPU_COUNT=1"));
        assert!(spec.contains("export MAKEFLAGS=-j1"));
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
    fn python_requirements_are_converted_to_pip_specs() {
        assert_eq!(
            conda_dep_to_pip_requirement("jinja2 >=3.0.0"),
            Some("jinja2>=3.0.0".to_string())
        );
        assert_eq!(
            conda_dep_to_pip_requirement("python-kaleido ==0.2.1"),
            Some("kaleido==0.2.1".to_string())
        );
        assert_eq!(conda_dep_to_pip_requirement("python >=3.8"), None);
        assert_eq!(conda_dep_to_pip_requirement("c-compiler"), None);
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
    fn harden_build_script_rewrites_streamed_wget_tar() {
        let raw = "#!/usr/bin/env bash\nwget -O- https://example.invalid/src.tar.gz | tar -zxf -\n";
        let hardened = harden_build_script_text(raw);
        assert!(hardened.contains("BIOCONDA2RPM_FETCH_0_ARCHIVE"));
        assert!(hardened.contains("wget --no-verbose -O \"${BIOCONDA2RPM_FETCH_0_ARCHIVE}\""));
        assert!(hardened.contains("tar -zxf \"${BIOCONDA2RPM_FETCH_0_ARCHIVE}\""));
        assert!(!hardened.contains("wget -O- https://example.invalid/src.tar.gz | tar -zxf -"));
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
    fn selector_filter_keeps_matching_lines() {
        let ctx = SelectorContext {
            linux: true,
            osx: false,
            win: false,
            aarch64: false,
            x86_64: true,
            py_major: 3,
            py_minor: 11,
        };

        let text = "url: http://linux.example # [linux]\nurl: http://osx.example # [osx]\n";
        let filtered = apply_selectors(text, &ctx);
        assert!(filtered.contains("linux.example"));
        assert!(!filtered.contains("osx.example"));
    }
}
