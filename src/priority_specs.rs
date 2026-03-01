use crate::build_lock;
use crate::cli::{
    BuildArgs, BuildContainerProfile, BuildStage, ContainerMode, DependencyPolicy,
    GeneratePrioritySpecsArgs, MetadataAdapter, MissingDependencyPolicy, NamingProfile,
    OutputSelection, ParallelPolicy, RegressionArgs, RegressionMode, RenderStrategy,
};
use anyhow::{Context, Result};
use chrono::Utc;
use csv::{ReaderBuilder, Writer};
use minijinja::{Environment, context, value::Kwargs};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::thread;
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
    target_id: String,
    target_root: PathBuf,
    reports_dir: PathBuf,
    container_engine: String,
    container_image: String,
    target_arch: String,
    parallel_policy: ParallelPolicy,
    build_jobs: usize,
    force_rebuild: bool,
}

#[derive(Debug, Clone)]
struct PrecompiledBinaryOverride {
    source_url: String,
    build_script: String,
}

#[derive(Debug, Clone, Copy)]
struct PhoreusPythonRuntime {
    major: u64,
    minor: u64,
    minor_str: &'static str,
    full_version: &'static str,
    package: &'static str,
}

const PHOREUS_PYTHON_VERSION: &str = "3.11";
const PHOREUS_PYTHON_FULL_VERSION: &str = "3.11.14";
const PHOREUS_PYTHON_PACKAGE: &str = "phoreus-python-3.11";
const PHOREUS_PYTHON_VERSION_313: &str = "3.13";
const PHOREUS_PYTHON_FULL_VERSION_313: &str = "3.13.2";
const PHOREUS_PYTHON_PACKAGE_313: &str = "phoreus-python-3.13";
const PHOREUS_PYTHON_RUNTIME_311: PhoreusPythonRuntime = PhoreusPythonRuntime {
    major: 3,
    minor: 11,
    minor_str: PHOREUS_PYTHON_VERSION,
    full_version: PHOREUS_PYTHON_FULL_VERSION,
    package: PHOREUS_PYTHON_PACKAGE,
};
const PHOREUS_PYTHON_RUNTIME_313: PhoreusPythonRuntime = PhoreusPythonRuntime {
    major: 3,
    minor: 13,
    minor_str: PHOREUS_PYTHON_VERSION_313,
    full_version: PHOREUS_PYTHON_FULL_VERSION_313,
    package: PHOREUS_PYTHON_PACKAGE_313,
};
const PHOREUS_PERL_VERSION: &str = "5.32";
const PHOREUS_PERL_PACKAGE: &str = "phoreus-perl-5.32";
static PHOREUS_PERL_BOOTSTRAP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
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
static BUILD_STABILITY_CACHE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
type ProgressSink = Arc<dyn Fn(String) + Send + Sync + 'static>;
static PROGRESS_SINK: OnceLock<Mutex<Option<ProgressSink>>> = OnceLock::new();
static CANCELLATION_REQUESTED: AtomicBool = AtomicBool::new(false);
static CANCELLATION_REASON: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static ACTIVE_CONTAINERS: OnceLock<Mutex<HashMap<String, ActiveContainerRun>>> = OnceLock::new();
const CONDA_RENDER_ADAPTER_SCRIPT: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/conda_render_ir.py");

#[derive(Debug, Clone)]
struct ActiveContainerRun {
    engine: String,
    label: String,
    spec: String,
}

struct ActiveContainerGuard {
    name: String,
}

impl ActiveContainerGuard {
    fn new(name: String) -> Self {
        Self { name }
    }
}

impl Drop for ActiveContainerGuard {
    fn drop(&mut self) {
        unregister_active_container(&self.name);
    }
}

fn log_progress(message: impl AsRef<str>) {
    emit_progress_line(format!("progress {}", message.as_ref()));
}

pub fn log_external_progress(message: impl AsRef<str>) {
    log_progress(message);
}

fn emit_progress_line(line: String) {
    let lock = PROGRESS_SINK.get_or_init(|| Mutex::new(None));
    match lock.lock() {
        Ok(guard) => {
            if let Some(sink) = guard.as_ref() {
                sink(line);
            } else {
                println!("{line}");
            }
        }
        Err(_) => {
            println!("{line}");
        }
    }
}

pub fn install_progress_sink(sink: Arc<dyn Fn(String) + Send + Sync + 'static>) {
    let lock = PROGRESS_SINK.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = lock.lock() {
        *guard = Some(sink);
    }
}

pub fn clear_progress_sink() {
    let lock = PROGRESS_SINK.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = lock.lock() {
        *guard = None;
    }
}

pub fn reset_cancellation() {
    CANCELLATION_REQUESTED.store(false, AtomicOrdering::SeqCst);
    let lock = CANCELLATION_REASON.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = lock.lock() {
        *guard = None;
    }
}

pub fn request_cancellation(reason: impl Into<String>) {
    let reason = reason.into();
    CANCELLATION_REQUESTED.store(true, AtomicOrdering::SeqCst);
    let lock = CANCELLATION_REASON.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = lock.lock()
        && guard.is_none()
    {
        *guard = Some(reason.clone());
    }
    log_progress(format!(
        "phase=build status=cancel-requested reason={}",
        compact_reason(&reason, 240)
    ));
    stop_active_containers(&reason);
}

fn register_active_container(name: &str, engine: &str, label: &str, spec: &str) {
    let lock = ACTIVE_CONTAINERS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = lock.lock() {
        guard.insert(
            name.to_string(),
            ActiveContainerRun {
                engine: engine.to_string(),
                label: label.to_string(),
                spec: spec.to_string(),
            },
        );
    }
}

fn unregister_active_container(name: &str) {
    let lock = ACTIVE_CONTAINERS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = lock.lock() {
        guard.remove(name);
    }
}

fn active_container_snapshot() -> Vec<(String, ActiveContainerRun)> {
    let lock = ACTIVE_CONTAINERS.get_or_init(|| Mutex::new(HashMap::new()));
    match lock.lock() {
        Ok(guard) => guard
            .iter()
            .map(|(name, run)| (name.clone(), run.clone()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn lookup_active_container(name: &str) -> Option<ActiveContainerRun> {
    let lock = ACTIVE_CONTAINERS.get_or_init(|| Mutex::new(HashMap::new()));
    match lock.lock() {
        Ok(guard) => guard.get(name).cloned(),
        Err(_) => None,
    }
}

fn force_stop_container(
    name: &str,
    run: &ActiveContainerRun,
    reason: &str,
    clear_registry: bool,
) -> bool {
    let started = Instant::now();
    log_progress(format!(
        "phase=container-build status=stopping label={} spec={} container={} reason={}",
        run.label,
        run.spec,
        name,
        compact_reason(reason, 160)
    ));
    let output = Command::new(&run.engine)
        .arg("rm")
        .arg("-f")
        .arg(name)
        .output();

    let mut stopped = false;
    match output {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let lower = combined.to_lowercase();
            if out.status.success() || lower.contains("no such container") {
                stopped = true;
                log_progress(format!(
                    "phase=container-build status=stopped label={} spec={} container={} elapsed={} detail={}",
                    run.label,
                    run.spec,
                    name,
                    format_elapsed(started.elapsed()),
                    compact_reason(&tail_lines(&combined, 4), 220)
                ));
            } else {
                log_progress(format!(
                    "phase=container-build status=stop-failed label={} spec={} container={} elapsed={} exit={} detail={}",
                    run.label,
                    run.spec,
                    name,
                    format_elapsed(started.elapsed()),
                    out.status,
                    compact_reason(&tail_lines(&combined, 6), 260)
                ));
            }
        }
        Err(err) => {
            log_progress(format!(
                "phase=container-build status=stop-failed label={} spec={} container={} elapsed={} detail={}",
                run.label,
                run.spec,
                name,
                format_elapsed(started.elapsed()),
                compact_reason(&err.to_string(), 220)
            ));
        }
    }
    if clear_registry {
        unregister_active_container(name);
    }
    stopped
}

fn stop_active_container_by_name(name: &str, reason: &str) -> bool {
    let Some(run) = lookup_active_container(name) else {
        return false;
    };
    force_stop_container(name, &run, reason, false)
}

pub fn stop_active_containers(reason: &str) {
    let snapshot = active_container_snapshot();
    if snapshot.is_empty() {
        return;
    }
    log_progress(format!(
        "phase=container-build status=stopping-all count={} reason={}",
        snapshot.len(),
        compact_reason(reason, 160)
    ));
    for (name, run) in snapshot {
        let _ = force_stop_container(&name, &run, reason, true);
    }
}

fn cancellation_requested() -> bool {
    CANCELLATION_REQUESTED.load(AtomicOrdering::SeqCst)
}

fn cancellation_reason() -> String {
    let lock = CANCELLATION_REASON.get_or_init(|| Mutex::new(None));
    match lock.lock() {
        Ok(guard) => guard
            .clone()
            .unwrap_or_else(|| "cancelled by user".to_string()),
        Err(_) => "cancelled by user".to_string(),
    }
}

fn cancellation_error(context: &str) -> anyhow::Error {
    anyhow::anyhow!("{}: {}", context, cancellation_reason())
}

fn is_cancellation_failure(reason: &str) -> bool {
    reason.contains("cancelled by user")
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

fn seed_heartbeat_rng(build_label: &str, spec_name: &str, attempt: usize) -> u64 {
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    for byte in build_label
        .bytes()
        .chain(spec_name.bytes())
        .chain(attempt.to_string().bytes())
    {
        seed = seed.rotate_left(5) ^ u64::from(byte);
        seed = seed.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    }
    let now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    seed ^ now_nanos ^ u64::from(std::process::id())
}

fn next_heartbeat_interval_secs(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    5 + ((*state >> 32) % 6)
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

#[derive(Debug, Serialize, Deserialize, Clone)]
struct BuildStabilityRecord {
    status: String,
    updated_at: String,
    detail: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
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
    pub kpi_scope_entries: usize,
    pub kpi_excluded_arch: usize,
    pub kpi_denominator: usize,
    pub kpi_successes: usize,
    pub kpi_success_rate: f64,
    pub build_order: Vec<String>,
    pub report_json: PathBuf,
    pub report_csv: PathBuf,
    pub report_md: PathBuf,
}

#[derive(Debug, Serialize, Clone)]
struct RegressionReportEntry {
    software: String,
    priority: i64,
    status: String,
    reason: String,
    root_status: String,
    root_reason: String,
    build_report_json: String,
    build_report_md: String,
}

#[derive(Debug)]
pub struct RegressionSummary {
    pub mode: RegressionMode,
    pub requested: usize,
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub excluded: usize,
    pub kpi_denominator: usize,
    pub kpi_successes: usize,
    pub kpi_success_rate: f64,
    pub report_json: PathBuf,
    pub report_csv: PathBuf,
    pub report_md: PathBuf,
}

#[derive(Debug, Clone)]
struct KpiSummary {
    scope_entries: usize,
    excluded_arch: usize,
    denominator: usize,
    successes: usize,
    success_rate: f64,
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
    if cancellation_requested() {
        return Err(cancellation_error("generation cancelled before start"));
    }
    let recipe_root = args.effective_recipe_root();
    let topdir = args.effective_topdir();
    let specs_dir = topdir.join("SPECS");
    let sources_dir = topdir.join("SOURCES");
    let target_arch = args.effective_target_arch();
    let target_id = args.effective_target_id();
    let target_root = args.effective_target_root();
    let rpms_dir = target_root.join("RPMS");
    let srpms_dir = target_root.join("SRPMS");
    let reports_dir = args.effective_reports_dir();
    let bad_spec_dir = args.effective_bad_spec_dir();

    fs::create_dir_all(&specs_dir)
        .with_context(|| format!("creating specs dir {}", specs_dir.display()))?;
    fs::create_dir_all(&sources_dir)
        .with_context(|| format!("creating sources dir {}", sources_dir.display()))?;
    fs::create_dir_all(&rpms_dir)
        .with_context(|| format!("creating rpms dir {}", rpms_dir.display()))?;
    fs::create_dir_all(&srpms_dir)
        .with_context(|| format!("creating srpms dir {}", srpms_dir.display()))?;
    fs::create_dir_all(&reports_dir)
        .with_context(|| format!("creating reports dir {}", reports_dir.display()))?;
    fs::create_dir_all(&bad_spec_dir)
        .with_context(|| format!("creating bad spec dir {}", bad_spec_dir.display()))?;
    ensure_container_engine_available(&args.container_engine)?;
    ensure_container_profile_available(
        &args.container_engine,
        args.container_profile,
        &target_arch,
    )?;
    sync_reference_python_specs(&specs_dir).context("syncing reference Phoreus Python specs")?;

    let mut tools = load_top_tools(&args.tools_csv, args.top_n)?;
    tools.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.line_no.cmp(&b.line_no)));

    let recipe_dirs = discover_recipe_dirs(&recipe_root)?;
    let build_config = BuildConfig {
        topdir: topdir.clone(),
        target_id,
        target_root: target_root.clone(),
        reports_dir: reports_dir.clone(),
        container_engine: args.container_engine.clone(),
        container_image: args.effective_container_image().to_string(),
        target_arch: target_arch.clone(),
        parallel_policy: args.parallel_policy.clone(),
        build_jobs: args.effective_build_jobs(),
        force_rebuild: false,
    };
    ensure_phoreus_python_bootstrap(&build_config, &specs_dir, PHOREUS_PYTHON_RUNTIME_311)
        .context("bootstrapping Phoreus Python runtime")?;
    ensure_phoreus_perl_bootstrap(&build_config, &specs_dir)
        .context("bootstrapping Phoreus Perl runtime")?;

    let indexed_tools: Vec<(usize, PriorityTool)> = tools.into_iter().enumerate().collect();
    let worker_count = args.workers.filter(|w| *w > 0);

    let runner = || {
        indexed_tools
            .par_iter()
            .map(|(idx, tool)| {
                let entry = process_tool(
                    tool,
                    &recipe_root,
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

pub(crate) fn collect_requested_build_packages(args: &BuildArgs) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for pkg in &args.packages {
        let name = pkg.trim();
        if name.is_empty() {
            continue;
        }
        let key = normalize_name(name);
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        out.push(name.to_string());
    }

    if let Some(path) = args.packages_file.as_ref() {
        let from_file = load_software_list(path)?;
        for pkg in from_file {
            let key = normalize_name(&pkg);
            if key.is_empty() || !seen.insert(key) {
                continue;
            }
            out.push(pkg);
        }
    }

    if out.is_empty() {
        anyhow::bail!("no packages requested: pass PACKAGE positional args and/or --packages-file");
    }
    Ok(out)
}

pub fn run_build(args: &BuildArgs) -> Result<BuildSummary> {
    if cancellation_requested() {
        return Err(cancellation_error("build cancelled before start"));
    }
    let build_started = Instant::now();
    let recipe_root = args.effective_recipe_root();
    let requested_packages = collect_requested_build_packages(args)?;
    let topdir = args.effective_topdir();
    let specs_dir = topdir.join("SPECS");
    let sources_dir = topdir.join("SOURCES");
    let target_arch = args.effective_target_arch();
    let target_id = args.effective_target_id();
    let target_root = args.effective_target_root();
    let rpms_dir = target_root.join("RPMS");
    let srpms_dir = target_root.join("SRPMS");
    let reports_dir = args.effective_reports_dir();
    let bad_spec_dir = args.effective_bad_spec_dir();
    let effective_metadata_adapter = args.effective_metadata_adapter();
    log_progress(format!(
        "phase=build-start requested_packages={} deps_enabled={} force_rebuild={} dependency_policy={:?} recipe_root={} topdir={} target_id={} target_root={} target_arch={} deployment_profile={:?} metadata_adapter={:?} parallel_policy={:?} build_jobs={} effective_build_jobs={} queue_workers={} effective_queue_workers={}",
        requested_packages.len(),
        args.with_deps(),
        args.force,
        args.dependency_policy,
        recipe_root.display(),
        topdir.display(),
        target_id,
        target_root.display(),
        target_arch,
        args.deployment_profile,
        effective_metadata_adapter,
        args.parallel_policy,
        args.build_jobs,
        args.effective_build_jobs(),
        args.queue_workers
            .map(|v| v.to_string())
            .unwrap_or_else(|| "auto".to_string()),
        args.effective_queue_workers()
    ));

    fs::create_dir_all(&specs_dir)
        .with_context(|| format!("creating specs dir {}", specs_dir.display()))?;
    fs::create_dir_all(&sources_dir)
        .with_context(|| format!("creating sources dir {}", sources_dir.display()))?;
    fs::create_dir_all(&rpms_dir)
        .with_context(|| format!("creating rpms dir {}", rpms_dir.display()))?;
    fs::create_dir_all(&srpms_dir)
        .with_context(|| format!("creating srpms dir {}", srpms_dir.display()))?;
    fs::create_dir_all(&reports_dir)
        .with_context(|| format!("creating reports dir {}", reports_dir.display()))?;
    fs::create_dir_all(&bad_spec_dir)
        .with_context(|| format!("creating bad spec dir {}", bad_spec_dir.display()))?;

    ensure_container_engine_available(&args.container_engine)?;
    ensure_container_profile_available(
        &args.container_engine,
        args.container_profile,
        &target_arch,
    )?;
    sync_reference_python_specs(&specs_dir).context("syncing reference Phoreus Python specs")?;
    let recipe_dirs = discover_recipe_dirs(&recipe_root)?;
    log_progress(format!(
        "phase=recipe-discovery status=completed recipe_count={} elapsed={}",
        recipe_dirs.len(),
        format_elapsed(build_started.elapsed())
    ));

    let build_config = BuildConfig {
        topdir: topdir.clone(),
        target_id: target_id.clone(),
        target_root: target_root.clone(),
        reports_dir: reports_dir.clone(),
        container_engine: args.container_engine.clone(),
        container_image: args.effective_container_image().to_string(),
        target_arch: target_arch.clone(),
        parallel_policy: args.parallel_policy.clone(),
        build_jobs: args.effective_build_jobs(),
        force_rebuild: args.force,
    };
    ensure_phoreus_python_bootstrap(&build_config, &specs_dir, PHOREUS_PYTHON_RUNTIME_311)
        .context("bootstrapping Phoreus Python runtime")?;
    ensure_phoreus_perl_bootstrap(&build_config, &specs_dir)
        .context("bootstrapping Phoreus Perl runtime")?;

    if requested_packages.len() > 1 {
        return run_build_batch_queue(
            args,
            &requested_packages,
            &recipe_dirs,
            &specs_dir,
            &sources_dir,
            &bad_spec_dir,
            &reports_dir,
            &build_config,
            &effective_metadata_adapter,
            build_started,
        );
    }

    let root_request = requested_packages
        .first()
        .cloned()
        .context("missing requested package after validation")?;

    let Some(root_recipe) = resolve_and_parse_recipe(
        &root_request,
        &recipe_root,
        &recipe_dirs,
        true,
        &effective_metadata_adapter,
        &build_config.target_arch,
    )?
    else {
        anyhow::bail!(
            "no overlapping recipe found in bioconda metadata for '{}'",
            root_request
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
        let report_stem = normalize_name(&root_request);
        let report_json = reports_dir.join(format!("build_{report_stem}.json"));
        let report_csv = reports_dir.join(format!("build_{report_stem}.csv"));
        let report_md = reports_dir.join(format!("build_{report_stem}.md"));
        write_reports(&[entry], &report_json, &report_csv, &report_md)?;
        let kpi = compute_arch_adjusted_kpi(&[]);
        return Ok(BuildSummary {
            requested: 1,
            generated: 0,
            up_to_date: 0,
            skipped: 1,
            quarantined: 0,
            kpi_scope_entries: kpi.scope_entries,
            kpi_excluded_arch: kpi.excluded_arch,
            kpi_denominator: kpi.denominator,
            kpi_successes: kpi.successes,
            kpi_success_rate: kpi.success_rate,
            build_order: vec![root_recipe.resolved.recipe_name.clone()],
            report_json,
            report_csv,
            report_md,
        });
    }

    let root_slug = normalize_name(&root_recipe.resolved.recipe_name);
    if !args.force
        && let PayloadVersionState::UpToDate { existing_version } = payload_version_state(
            &topdir,
            &build_config.target_root,
            &root_slug,
            &root_recipe.parsed.version,
        )?
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

        let report_stem = normalize_name(&root_request);
        let report_json = reports_dir.join(format!("build_{report_stem}.json"));
        let report_csv = reports_dir.join(format!("build_{report_stem}.csv"));
        let report_md = reports_dir.join(format!("build_{report_stem}.md"));
        write_reports(&[entry], &report_json, &report_csv, &report_md)?;
        let kpi = compute_arch_adjusted_kpi(&[]);

        return Ok(BuildSummary {
            requested: 1,
            generated: 0,
            up_to_date: 1,
            skipped: 0,
            quarantined: 0,
            kpi_scope_entries: kpi.scope_entries,
            kpi_excluded_arch: kpi.excluded_arch,
            kpi_denominator: kpi.denominator,
            kpi_successes: kpi.successes,
            kpi_success_rate: kpi.success_rate,
            build_order: vec![root_recipe.resolved.recipe_name],
            report_json,
            report_csv,
            report_md,
        });
    }
    if args.force {
        log_progress(format!(
            "phase=build status=force-rebuild package={} version={} reason=explicit-force-flag",
            root_recipe.resolved.recipe_name, root_recipe.parsed.version
        ));
    }
    run_build_batch_queue(
        args,
        std::slice::from_ref(&root_request),
        &recipe_dirs,
        &specs_dir,
        &sources_dir,
        &bad_spec_dir,
        &reports_dir,
        &build_config,
        &effective_metadata_adapter,
        build_started,
    )
}

#[allow(clippy::too_many_arguments)]
fn process_failed_dependency_queue(
    fail_queue: &mut VecDeque<String>,
    global_nodes: &BTreeMap<String, BuildPlanNode>,
    pending_deps: &mut HashMap<String, usize>,
    dependents: &HashMap<String, Vec<String>>,
    finalized: &mut HashSet<String>,
    failed_by: &mut HashMap<String, BTreeSet<String>>,
    results: &mut Vec<ReportEntry>,
    bad_spec_dir: &Path,
    missing_dependency: &MissingDependencyPolicy,
    fail_reason: &mut Option<String>,
) {
    while let Some(failed_key) = fail_queue.pop_front() {
        if finalized.contains(&failed_key) {
            continue;
        }
        let Some(node) = global_nodes.get(&failed_key) else {
            finalized.insert(failed_key);
            continue;
        };
        let failed_keys = failed_by.get(&failed_key).cloned().unwrap_or_default();
        let failed_names = failed_keys
            .iter()
            .filter_map(|k| global_nodes.get(k).map(|n| n.name.clone()))
            .collect::<Vec<_>>();
        let reason = if failed_names.is_empty() {
            format!("blocked by failed dependencies for {}", node.name)
        } else {
            format!(
                "blocked by failed dependencies: {}",
                failed_names.join(", ")
            )
        };
        let status = match missing_dependency {
            MissingDependencyPolicy::Skip => "skipped",
            _ => "quarantined",
        }
        .to_string();
        if status == "quarantined" {
            quarantine_note(bad_spec_dir, &failed_key, &reason);
        }
        log_progress(format!(
            "phase=batch-queue status={} key={} package={} reason={}",
            status,
            failed_key,
            node.name,
            compact_reason(&reason, 220)
        ));
        results.push(ReportEntry {
            software: node.name.clone(),
            priority: 0,
            status: status.clone(),
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
        finalized.insert(failed_key.clone());
        if *missing_dependency == MissingDependencyPolicy::Fail && fail_reason.is_none() {
            *fail_reason = Some(reason.clone());
        }

        if let Some(children) = dependents.get(&failed_key) {
            for child in children {
                if finalized.contains(child) {
                    continue;
                }
                if let Some(pending) = pending_deps.get_mut(child)
                    && *pending > 0
                {
                    *pending -= 1;
                }
                failed_by
                    .entry(child.clone())
                    .or_default()
                    .insert(failed_key.clone());
                if pending_deps.get(child).copied().unwrap_or(0) == 0 {
                    fail_queue.push_back(child.clone());
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_dynamic_plan_nodes(
    nodes: BTreeMap<String, BuildPlanNode>,
    global_nodes: &mut BTreeMap<String, BuildPlanNode>,
    pending_deps: &mut HashMap<String, usize>,
    dependents: &mut HashMap<String, Vec<String>>,
    failed_by: &mut HashMap<String, BTreeSet<String>>,
    finalized: &HashSet<String>,
    succeeded: &HashSet<String>,
    ready: &mut VecDeque<String>,
    fail_queue: &mut VecDeque<String>,
) -> usize {
    let mut inserted = 0usize;
    for (key, node) in nodes {
        if global_nodes.contains_key(&key) {
            continue;
        }
        inserted += 1;
        let mut pending = 0usize;
        let mut failed_inputs = BTreeSet::new();
        for dep in &node.direct_bioconda_deps {
            dependents.entry(dep.clone()).or_default().push(key.clone());
            if finalized.contains(dep) {
                if !succeeded.contains(dep) {
                    failed_inputs.insert(dep.clone());
                }
            } else {
                pending += 1;
            }
        }
        if !failed_inputs.is_empty() {
            failed_by.insert(key.clone(), failed_inputs);
        }
        pending_deps.insert(key.clone(), pending);
        global_nodes.insert(key.clone(), node);
        if pending == 0 {
            if failed_by
                .get(&key)
                .map(|deps| !deps.is_empty())
                .unwrap_or(false)
            {
                fail_queue.push_back(key.clone());
            } else {
                ready.push_back(key.clone());
            }
        }
    }
    inserted
}

#[allow(clippy::too_many_arguments)]
fn requeue_existing_node_for_rerun(
    key: &str,
    global_nodes: &BTreeMap<String, BuildPlanNode>,
    pending_deps: &mut HashMap<String, usize>,
    failed_by: &mut HashMap<String, BTreeSet<String>>,
    finalized: &mut HashSet<String>,
    succeeded: &mut HashSet<String>,
    ready: &mut VecDeque<String>,
    pending_fail_queue: &mut VecDeque<String>,
    results: &mut Vec<ReportEntry>,
    running_keys: &HashSet<String>,
) -> bool {
    if key.is_empty() || running_keys.contains(key) {
        return false;
    }
    let Some(node) = global_nodes.get(key) else {
        return false;
    };

    // Drop stale completion/quarantine entries for this node so rerun status is authoritative.
    results.retain(|entry| normalize_name(&entry.software) != key);
    ready.retain(|queued| queued != key);
    pending_fail_queue.retain(|queued| queued != key);
    finalized.remove(key);
    succeeded.remove(key);

    let mut pending = 0usize;
    let mut failed_inputs = BTreeSet::new();
    for dep in &node.direct_bioconda_deps {
        if finalized.contains(dep) {
            if !succeeded.contains(dep) {
                failed_inputs.insert(dep.clone());
            }
        } else {
            pending += 1;
        }
    }
    pending_deps.insert(key.to_string(), pending);
    if failed_inputs.is_empty() {
        failed_by.remove(key);
    } else {
        failed_by.insert(key.to_string(), failed_inputs);
    }

    if pending == 0 {
        if failed_by
            .get(key)
            .map(|deps| !deps.is_empty())
            .unwrap_or(false)
        {
            pending_fail_queue.push_back(key.to_string());
        } else {
            ready.push_back(key.to_string());
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn run_build_batch_queue(
    args: &BuildArgs,
    requested_packages: &[String],
    recipe_dirs: &[RecipeDir],
    specs_dir: &Path,
    sources_dir: &Path,
    bad_spec_dir: &Path,
    reports_dir: &Path,
    build_config: &BuildConfig,
    metadata_adapter: &MetadataAdapter,
    build_started: Instant,
) -> Result<BuildSummary> {
    let recipe_root = args.effective_recipe_root();
    let queue_workers = args.effective_queue_workers().max(1);
    log_progress(format!(
        "phase=batch-queue status=initialized roots={} queue_workers={} build_jobs_per_worker={} policy={:?}",
        requested_packages.len(),
        queue_workers,
        build_config.build_jobs,
        build_config.parallel_policy
    ));

    let mut global_nodes: BTreeMap<String, BuildPlanNode> = BTreeMap::new();
    let mut results: Vec<ReportEntry> = Vec::new();
    let mut fail_reason: Option<String> = None;
    let mut requested_roots = requested_packages.to_vec();
    let mut requested_root_keys: HashSet<String> = requested_packages
        .iter()
        .map(|pkg| normalize_name(pkg))
        .filter(|pkg| !pkg.is_empty())
        .collect();

    for root in requested_packages {
        match collect_build_plan(
            root,
            args.with_deps(),
            &args.dependency_policy,
            &recipe_root,
            recipe_dirs,
            metadata_adapter,
            &build_config.target_arch,
        ) {
            Ok((order, nodes)) => {
                let root_order = order
                    .iter()
                    .filter_map(|key| nodes.get(key).map(|node| node.name.clone()))
                    .collect::<Vec<_>>();
                log_progress(format!(
                    "phase=dependency-plan status=completed package={} planned_nodes={} order={}",
                    root,
                    root_order.len(),
                    root_order.join("->")
                ));
                for (key, node) in nodes {
                    global_nodes
                        .entry(key)
                        .and_modify(|existing| {
                            existing
                                .direct_bioconda_deps
                                .extend(node.direct_bioconda_deps.clone());
                        })
                        .or_insert(node);
                }
            }
            Err(err) => {
                let slug = normalize_name(root);
                let reason = format!(
                    "no overlapping recipe found in bioconda metadata for '{}': {}",
                    root,
                    compact_reason(&err.to_string(), 240)
                );
                let status = match args.missing_dependency {
                    MissingDependencyPolicy::Skip => "skipped",
                    _ => "quarantined",
                }
                .to_string();
                if status == "quarantined" {
                    quarantine_note(bad_spec_dir, &slug, &reason);
                }
                results.push(ReportEntry {
                    software: root.clone(),
                    priority: 0,
                    status,
                    reason: reason.clone(),
                    overlap_recipe: root.clone(),
                    overlap_reason: "requested-root".to_string(),
                    variant_dir: String::new(),
                    package_name: String::new(),
                    version: String::new(),
                    payload_spec_path: String::new(),
                    meta_spec_path: String::new(),
                    staged_build_sh: String::new(),
                });
                if args.missing_dependency == MissingDependencyPolicy::Fail && fail_reason.is_none()
                {
                    fail_reason = Some(reason);
                }
            }
        }
    }

    let mut pending_deps: HashMap<String, usize> = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
    for (key, node) in &global_nodes {
        pending_deps.insert(key.clone(), node.direct_bioconda_deps.len());
        for dep in &node.direct_bioconda_deps {
            dependents.entry(dep.clone()).or_default().push(key.clone());
        }
    }

    let mut ready: Vec<String> = pending_deps
        .iter()
        .filter_map(|(key, count)| if *count == 0 { Some(key.clone()) } else { None })
        .collect();
    ready.sort();
    let mut ready = VecDeque::from(ready);

    let recipe_root = Arc::new(recipe_root);
    let recipe_dirs = Arc::new(recipe_dirs.to_vec());
    let specs_dir = Arc::new(specs_dir.to_path_buf());
    let sources_dir = Arc::new(sources_dir.to_path_buf());
    let bad_spec_dir = Arc::new(bad_spec_dir.to_path_buf());
    let build_config = Arc::new(build_config.clone());
    let metadata_adapter = Arc::new(metadata_adapter.clone());

    let (tx, rx) = mpsc::channel::<(String, ReportEntry, Duration)>();
    let mut running = 0usize;
    let mut running_keys: HashSet<String> = HashSet::new();
    let mut finalized: HashSet<String> = HashSet::new();
    let mut succeeded: HashSet<String> = HashSet::new();
    let mut failed_by: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut pending_fail_queue: VecDeque<String> = VecDeque::new();
    let mut build_order = Vec::new();

    while !ready.is_empty() || running > 0 || !pending_fail_queue.is_empty() {
        if !cancellation_requested() {
            match build_lock::drain_forwarded_build_requests(
                build_config.topdir.as_path(),
                &build_config.target_id,
            ) {
                Ok(forwarded_roots) => {
                    let local_host = build_lock::current_host_name();
                    for forwarded in forwarded_roots {
                        let root = forwarded.package;
                        let key = normalize_name(&root);
                        if key.is_empty() {
                            continue;
                        }
                        let is_remote_submitter = !forwarded.submitted_host.is_empty()
                            && forwarded.submitted_host != local_host;
                        if !requested_root_keys.insert(key.clone()) {
                            if is_remote_submitter {
                                let queued = requeue_existing_node_for_rerun(
                                    &key,
                                    &global_nodes,
                                    &mut pending_deps,
                                    &mut failed_by,
                                    &mut finalized,
                                    &mut succeeded,
                                    &mut ready,
                                    &mut pending_fail_queue,
                                    &mut results,
                                    &running_keys,
                                );
                                log_progress(format!(
                                    "phase=workspace-lock status=forwarded-request-rerun package={} key={} submit_host={} submit_pid={} submit_ts={} queued={}",
                                    root,
                                    key,
                                    forwarded.submitted_host,
                                    forwarded.submitted_pid,
                                    forwarded.submitted_at_utc,
                                    queued
                                ));
                            } else {
                                log_progress(format!(
                                    "phase=workspace-lock status=forwarded-request-ignored package={} key={} submit_host={} reason=duplicate-same-host",
                                    root, key, forwarded.submitted_host
                                ));
                            }
                            continue;
                        }
                        requested_roots.push(root.clone());
                        log_progress(format!(
                            "phase=workspace-lock status=forwarded-request-received package={} target_id={} submit_host={} submit_pid={} submit_ts={}",
                            root,
                            build_config.target_id,
                            forwarded.submitted_host,
                            forwarded.submitted_pid,
                            forwarded.submitted_at_utc
                        ));
                        match collect_build_plan(
                            &root,
                            args.with_deps(),
                            &args.dependency_policy,
                            recipe_root.as_path(),
                            recipe_dirs.as_slice(),
                            metadata_adapter.as_ref(),
                            &build_config.target_arch,
                        ) {
                            Ok((order, nodes)) => {
                                let root_order = order
                                    .iter()
                                    .filter_map(|node_key| {
                                        nodes.get(node_key).map(|node| node.name.clone())
                                    })
                                    .collect::<Vec<_>>();
                                let added = merge_dynamic_plan_nodes(
                                    nodes,
                                    &mut global_nodes,
                                    &mut pending_deps,
                                    &mut dependents,
                                    &mut failed_by,
                                    &finalized,
                                    &succeeded,
                                    &mut ready,
                                    &mut pending_fail_queue,
                                );
                                log_progress(format!(
                                    "phase=dependency-plan status=completed package={} planned_nodes={} added_nodes={} order={}",
                                    root,
                                    root_order.len(),
                                    added,
                                    root_order.join("->")
                                ));
                            }
                            Err(err) => {
                                let slug = normalize_name(&root);
                                let reason = format!(
                                    "no overlapping recipe found in bioconda metadata for '{}': {}",
                                    root,
                                    compact_reason(&err.to_string(), 240)
                                );
                                let status = match args.missing_dependency {
                                    MissingDependencyPolicy::Skip => "skipped",
                                    _ => "quarantined",
                                }
                                .to_string();
                                if status == "quarantined" {
                                    quarantine_note(bad_spec_dir.as_path(), &slug, &reason);
                                }
                                results.push(ReportEntry {
                                    software: root.clone(),
                                    priority: 0,
                                    status,
                                    reason: reason.clone(),
                                    overlap_recipe: root.clone(),
                                    overlap_reason: "requested-root".to_string(),
                                    variant_dir: String::new(),
                                    package_name: String::new(),
                                    version: String::new(),
                                    payload_spec_path: String::new(),
                                    meta_spec_path: String::new(),
                                    staged_build_sh: String::new(),
                                });
                                if args.missing_dependency == MissingDependencyPolicy::Fail
                                    && fail_reason.is_none()
                                {
                                    fail_reason = Some(reason);
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    log_progress(format!(
                        "phase=workspace-lock status=forwarded-request-drain-error target_id={} detail={}",
                        build_config.target_id,
                        compact_reason(&err.to_string(), 220)
                    ));
                }
            }
        }

        process_failed_dependency_queue(
            &mut pending_fail_queue,
            &global_nodes,
            &mut pending_deps,
            &dependents,
            &mut finalized,
            &mut failed_by,
            &mut results,
            bad_spec_dir.as_path(),
            &args.missing_dependency,
            &mut fail_reason,
        );

        let cancelled = cancellation_requested();
        while !cancelled && running < queue_workers && !ready.is_empty() {
            let key = ready.pop_front().unwrap_or_default();
            if key.is_empty() || finalized.contains(&key) {
                continue;
            }
            if fail_reason.is_some() && args.missing_dependency == MissingDependencyPolicy::Fail {
                break;
            }
            let Some(node) = global_nodes.get(&key) else {
                finalized.insert(key);
                continue;
            };
            build_order.push(node.name.clone());
            let tool = PriorityTool {
                line_no: 0,
                software: node.name.clone(),
                priority: 0,
            };
            let key_for_thread = key.clone();
            let txc = tx.clone();
            let recipe_root_c = Arc::clone(&recipe_root);
            let recipe_dirs_c = Arc::clone(&recipe_dirs);
            let specs_dir_c = Arc::clone(&specs_dir);
            let sources_dir_c = Arc::clone(&sources_dir);
            let bad_spec_dir_c = Arc::clone(&bad_spec_dir);
            let build_config_c = Arc::clone(&build_config);
            let metadata_adapter_c = Arc::clone(&metadata_adapter);
            running += 1;
            running_keys.insert(key_for_thread.clone());
            log_progress(format!(
                "phase=batch-queue status=dispatch key={} package={} running={} queued={}",
                key_for_thread,
                tool.software,
                running,
                ready.len()
            ));
            thread::spawn(move || {
                let package_started = Instant::now();
                let entry = process_tool(
                    &tool,
                    recipe_root_c.as_path(),
                    recipe_dirs_c.as_slice(),
                    specs_dir_c.as_path(),
                    sources_dir_c.as_path(),
                    bad_spec_dir_c.as_path(),
                    &build_config_c,
                    &metadata_adapter_c,
                );
                let _ = txc.send((key_for_thread, entry, package_started.elapsed()));
            });
        }

        if cancelled && !ready.is_empty() {
            let dropped = ready.len();
            log_progress(format!(
                "phase=batch-queue status=cancelled action=drop-queued dropped={} running={}",
                dropped, running
            ));
            ready.clear();
        }

        if running == 0 {
            break;
        }

        let (done_key, entry, elapsed) = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(msg) => msg,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("batch queue worker channel closed unexpectedly");
            }
        };
        running = running.saturating_sub(1);
        running_keys.remove(&done_key);
        if finalized.contains(&done_key) {
            continue;
        }
        finalized.insert(done_key.clone());
        log_progress(format!(
            "phase=batch-queue status=completed key={} package={} result={} elapsed={}",
            done_key,
            entry.software,
            entry.status,
            format_elapsed(elapsed)
        ));
        let success = entry.status == "generated"
            || entry.status == "up-to-date"
            || entry.status == "skipped";
        if success {
            succeeded.insert(done_key.clone());
        }
        if !success
            && args.missing_dependency == MissingDependencyPolicy::Fail
            && fail_reason.is_none()
        {
            fail_reason = Some(entry.reason.clone());
        }
        results.push(entry.clone());

        let mut fail_queue: VecDeque<String> = VecDeque::new();
        if !success {
            fail_queue.push_back(done_key.clone());
        }

        if let Some(children) = dependents.get(&done_key) {
            for child in children {
                if finalized.contains(child) {
                    continue;
                }
                if let Some(pending) = pending_deps.get_mut(child)
                    && *pending > 0
                {
                    *pending -= 1;
                }
                if !success {
                    failed_by
                        .entry(child.clone())
                        .or_default()
                        .insert(done_key.clone());
                }
                if pending_deps.get(child).copied().unwrap_or(0) == 0 {
                    if failed_by.get(child).map(|v| !v.is_empty()).unwrap_or(false) {
                        fail_queue.push_back(child.clone());
                    } else {
                        ready.push_back(child.clone());
                    }
                }
            }
        }

        process_failed_dependency_queue(
            &mut fail_queue,
            &global_nodes,
            &mut pending_deps,
            &dependents,
            &mut finalized,
            &mut failed_by,
            &mut results,
            bad_spec_dir.as_path(),
            &args.missing_dependency,
            &mut fail_reason,
        );
    }

    if finalized.len() < global_nodes.len() {
        for (key, node) in &global_nodes {
            if finalized.contains(key) {
                continue;
            }
            let reason = if cancellation_requested() {
                "cancelled by user before scheduling".to_string()
            } else {
                "scheduler ended before node became buildable".to_string()
            };
            let status = if cancellation_requested() {
                "skipped".to_string()
            } else {
                quarantine_note(bad_spec_dir.as_path(), key, &reason);
                "quarantined".to_string()
            };
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
            if !cancellation_requested()
                && args.missing_dependency == MissingDependencyPolicy::Fail
                && fail_reason.is_none()
            {
                fail_reason = Some(reason);
            }
        }
    }

    let report_stem = if requested_roots.len() == 1 {
        normalize_name(&requested_roots[0])
    } else {
        format!(
            "batch_{}_{}",
            requested_roots.len(),
            Utc::now().format("%Y%m%d%H%M%S")
        )
    };
    let report_json = reports_dir.join(format!("build_{report_stem}.json"));
    let report_csv = reports_dir.join(format!("build_{report_stem}.csv"));
    let report_md = reports_dir.join(format!("build_{report_stem}.md"));
    write_reports(&results, &report_json, &report_csv, &report_md)?;

    if cancellation_requested() {
        anyhow::bail!(
            "build cancelled by user (report_md={})",
            report_md.display()
        );
    }

    if let Some(reason) = fail_reason {
        anyhow::bail!(
            "batch build failed under missing-dependency policy fail: {} (report_md={})",
            reason,
            report_md.display()
        );
    }

    let kpi = compute_arch_adjusted_kpi(&results);
    if args.effective_kpi_gate() && kpi.success_rate + f64::EPSILON < args.kpi_min_success_rate {
        anyhow::bail!(
            "kpi gate failed: arch-adjusted success rate {:.2}% is below threshold {:.2}% (denominator={}, successes={}, excluded_arch={}, report_md={})",
            kpi.success_rate,
            args.kpi_min_success_rate,
            kpi.denominator,
            kpi.successes,
            kpi.excluded_arch,
            report_md.display()
        );
    }

    log_progress(format!(
        "phase=batch-queue status=completed requested_roots={} node_results={} elapsed={}",
        requested_roots.len(),
        results.len(),
        format_elapsed(build_started.elapsed())
    ));

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
        kpi_scope_entries: kpi.scope_entries,
        kpi_excluded_arch: kpi.excluded_arch,
        kpi_denominator: kpi.denominator,
        kpi_successes: kpi.successes,
        kpi_success_rate: kpi.success_rate,
        build_order,
        report_json,
        report_csv,
        report_md,
    })
}

pub fn run_regression(args: &RegressionArgs) -> Result<RegressionSummary> {
    let campaign_started = Instant::now();
    let recipe_root = args.effective_recipe_root();
    let topdir = args.effective_topdir();
    let target_arch = args.effective_target_arch();
    let target_id = args.effective_target_id();
    let target_root = args.effective_target_root();
    let reports_dir = args.effective_reports_dir();
    let bad_spec_dir = args.effective_bad_spec_dir();
    log_progress(format!(
        "phase=regression-start mode={:?} recipe_root={} tools_csv={} topdir={} target_id={} target_root={} target_arch={} container_profile={:?} container_image={} deployment_profile={:?} metadata_adapter={:?} parallel_policy={:?} build_jobs={} effective_build_jobs={}",
        args.mode,
        recipe_root.display(),
        args.tools_csv.display(),
        topdir.display(),
        target_id,
        target_root.display(),
        target_arch,
        args.container_profile,
        args.effective_container_image(),
        args.deployment_profile,
        args.effective_metadata_adapter(),
        args.parallel_policy,
        args.build_jobs,
        args.effective_build_jobs()
    ));

    fs::create_dir_all(&topdir).with_context(|| format!("creating topdir {}", topdir.display()))?;
    fs::create_dir_all(&reports_dir)
        .with_context(|| format!("creating reports dir {}", reports_dir.display()))?;
    fs::create_dir_all(&bad_spec_dir)
        .with_context(|| format!("creating bad spec dir {}", bad_spec_dir.display()))?;
    ensure_container_engine_available(&args.container_engine)?;
    ensure_container_profile_available(
        &args.container_engine,
        args.container_profile,
        &target_arch,
    )?;

    let all_tools = load_tools_csv_rows(&args.tools_csv)?;
    let selected_tools = if let Some(software_list_path) = args.software_list.as_ref() {
        let names = load_software_list(software_list_path)?;
        let mut priority_by_name: HashMap<String, i64> = HashMap::new();
        for tool in &all_tools {
            priority_by_name.insert(normalize_name(&tool.software), tool.priority);
        }
        let selected = names
            .into_iter()
            .enumerate()
            .map(|(idx, name)| {
                let key = normalize_name(&name);
                PriorityTool {
                    line_no: idx + 1,
                    software: name,
                    priority: priority_by_name.get(&key).copied().unwrap_or(0),
                }
            })
            .collect::<Vec<_>>();
        if selected.len() != 100 {
            log_progress(format!(
                "phase=regression-corpus status=notice source=software-list count={} expected=100 note=non-fatal",
                selected.len()
            ));
        }
        selected
    } else {
        match args.mode {
            RegressionMode::Pr => all_tools.into_iter().take(args.top_n).collect::<Vec<_>>(),
            RegressionMode::Nightly => all_tools,
        }
    };
    log_progress(format!(
        "phase=regression-corpus status=selected mode={:?} requested={} source={} elapsed={}",
        args.mode,
        selected_tools.len(),
        if args.software_list.is_some() {
            "software-list"
        } else {
            "tools-csv"
        },
        format_elapsed(campaign_started.elapsed())
    ));

    let mut rows = Vec::new();
    let mut attempted = 0usize;
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut excluded = 0usize;

    for (idx, tool) in selected_tools.iter().enumerate() {
        attempted += 1;
        log_progress(format!(
            "phase=regression-tool status=started index={}/{} tool={}",
            idx + 1,
            selected_tools.len(),
            tool.software
        ));
        let build_args = BuildArgs {
            recipe_root: Some(recipe_root.clone()),
            sync_recipes: false,
            recipe_ref: None,
            topdir: Some(topdir.clone()),
            bad_spec_dir: Some(bad_spec_dir.clone()),
            reports_dir: Some(reports_dir.clone()),
            stage: BuildStage::Rpm,
            dependency_policy: args.dependency_policy.clone(),
            no_deps: args.no_deps,
            force: false,
            container_mode: ContainerMode::Ephemeral,
            container_profile: args.container_profile,
            container_engine: args.container_engine.clone(),
            parallel_policy: args.parallel_policy.clone(),
            build_jobs: args.build_jobs.clone(),
            missing_dependency: args.missing_dependency.clone(),
            arch: args.arch.clone(),
            naming_profile: NamingProfile::Phoreus,
            render_strategy: RenderStrategy::JinjaFull,
            metadata_adapter: args.metadata_adapter.clone(),
            deployment_profile: args.deployment_profile.clone(),
            kpi_gate: false,
            kpi_min_success_rate: args.kpi_min_success_rate,
            outputs: OutputSelection::All,
            packages_file: None,
            packages: vec![tool.software.clone()],
            ui: crate::cli::UiMode::Plain,
            queue_workers: None,
            phoreus_local_repo: Vec::new(),
            phoreus_core_repo: Vec::new(),
        };

        match run_build(&build_args) {
            Ok(summary) => {
                let root =
                    detect_root_outcome(&tool.software, &summary).unwrap_or_else(|| RootOutcome {
                        status: "unknown".to_string(),
                        reason: "unable to infer root status from build report".to_string(),
                        excluded: false,
                        success: false,
                    });
                if root.excluded {
                    excluded += 1;
                } else if root.success {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
                rows.push(RegressionReportEntry {
                    software: tool.software.clone(),
                    priority: tool.priority,
                    status: if root.excluded {
                        "excluded".to_string()
                    } else if root.success {
                        "success".to_string()
                    } else {
                        "failed".to_string()
                    },
                    reason: root.reason.clone(),
                    root_status: root.status,
                    root_reason: root.reason,
                    build_report_json: summary.report_json.display().to_string(),
                    build_report_md: summary.report_md.display().to_string(),
                });
            }
            Err(err) => {
                let reason = err.to_string();
                let arch_excluded = reason_is_arch_incompatible(&reason);
                if arch_excluded {
                    excluded += 1;
                } else {
                    failed += 1;
                }
                rows.push(RegressionReportEntry {
                    software: tool.software.clone(),
                    priority: tool.priority,
                    status: if arch_excluded {
                        "excluded".to_string()
                    } else {
                        "failed".to_string()
                    },
                    reason: reason.clone(),
                    root_status: "build_error".to_string(),
                    root_reason: reason,
                    build_report_json: String::new(),
                    build_report_md: String::new(),
                });
            }
        }
    }

    let kpi_denominator = attempted.saturating_sub(excluded);
    let kpi_successes = succeeded;
    let kpi_success_rate = if kpi_denominator == 0 {
        100.0
    } else {
        (kpi_successes as f64 * 100.0) / (kpi_denominator as f64)
    };

    let mode_slug = match args.mode {
        RegressionMode::Pr => "pr",
        RegressionMode::Nightly => "nightly",
    };
    let report_json = reports_dir.join(format!("regression_{mode_slug}.json"));
    let report_csv = reports_dir.join(format!("regression_{mode_slug}.csv"));
    let report_md = reports_dir.join(format!("regression_{mode_slug}.md"));
    write_regression_reports(
        &rows,
        &report_json,
        &report_csv,
        &report_md,
        args,
        kpi_denominator,
        kpi_successes,
        kpi_success_rate,
    )?;

    if args.effective_kpi_gate() && kpi_success_rate + f64::EPSILON < args.kpi_min_success_rate {
        anyhow::bail!(
            "regression KPI gate failed: success rate {:.2}% < threshold {:.2}% (mode={:?}, denominator={}, successes={}, excluded={}, report_md={})",
            kpi_success_rate,
            args.kpi_min_success_rate,
            args.mode,
            kpi_denominator,
            kpi_successes,
            excluded,
            report_md.display()
        );
    }

    log_progress(format!(
        "phase=regression status=completed mode={:?} requested={} attempted={} succeeded={} failed={} excluded={} kpi_denominator={} kpi_successes={} kpi_success_rate={:.2} elapsed={}",
        args.mode,
        selected_tools.len(),
        attempted,
        succeeded,
        failed,
        excluded,
        kpi_denominator,
        kpi_successes,
        kpi_success_rate,
        format_elapsed(campaign_started.elapsed())
    ));

    Ok(RegressionSummary {
        mode: args.mode.clone(),
        requested: selected_tools.len(),
        attempted,
        succeeded,
        failed,
        excluded,
        kpi_denominator,
        kpi_successes,
        kpi_success_rate,
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
    target_arch: &str,
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
        target_arch,
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
    target_arch: &str,
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
        target_arch,
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
                target_arch,
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
    target_arch: &str,
) -> Result<Option<ResolvedParsedRecipe>> {
    let Some(resolved) =
        resolve_recipe_for_tool_mode(tool_name, recipe_root, recipe_dirs, allow_identifier_lookup)?
    else {
        return Ok(None);
    };
    let parsed_result = parse_meta_for_resolved(&resolved, metadata_adapter, target_arch)
        .with_context(|| {
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
    target_arch: &str,
) -> Result<ParsedRecipeResult> {
    match metadata_adapter {
        MetadataAdapter::Native => parse_meta_for_resolved_native(resolved, target_arch),
        MetadataAdapter::Conda => parse_meta_for_resolved_conda(resolved, target_arch),
        MetadataAdapter::Auto => match parse_meta_for_resolved_conda(resolved, target_arch) {
            Ok(parsed) => Ok(parsed),
            Err(err) => {
                log_progress(format!(
                    "phase=metadata-adapter status=using-native recipe={} from=conda to=native note={}",
                    resolved.recipe_name,
                    compact_reason(&err.to_string(), 240)
                ));
                parse_meta_for_resolved_native(resolved, target_arch)
            }
        },
    }
}

fn parse_meta_for_resolved_native(
    resolved: &ResolvedRecipe,
    target_arch: &str,
) -> Result<ParsedRecipeResult> {
    let meta_text = fs::read_to_string(&resolved.meta_path)
        .with_context(|| format!("failed to read metadata {}", resolved.meta_path.display()))?;
    let selector_ctx = SelectorContext::for_rpm_build(target_arch);
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

fn parse_meta_for_resolved_conda(
    resolved: &ResolvedRecipe,
    target_arch: &str,
) -> Result<ParsedRecipeResult> {
    let output = Command::new("python3")
        .env("CONDA_SUBDIR", conda_subdir_for_target_arch(target_arch))
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
        let detail = summarize_conda_adapter_issue(output.status, &stdout, &stderr);
        anyhow::bail!(
            "conda render adapter not active: {}",
            compact_reason(&detail, 400)
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

fn summarize_conda_adapter_issue(
    status: std::process::ExitStatus,
    stdout: &str,
    stderr: &str,
) -> String {
    let stderr_compact = compact_reason(stderr, 300);
    if stderr.contains("No module named 'conda_build'") {
        return format!("status={} detail=conda_build_module_not_installed", status);
    }
    if stderr.contains("command not found") {
        return format!("status={} detail=conda_tooling_not_installed", status);
    }
    if stderr.trim().is_empty() && stdout.trim().is_empty() {
        return format!("status={} detail=no_adapter_output", status);
    }
    format!(
        "status={} detail={} stdout={}",
        status,
        stderr_compact,
        compact_reason(stdout, 200)
    )
}

fn normalize_dep_specs_to_set(raw_specs: &[String]) -> BTreeSet<String> {
    raw_specs
        .iter()
        .filter_map(|raw| normalize_dependency_name(raw))
        .collect()
}

fn conda_subdir_for_target_arch(target_arch: &str) -> &'static str {
    match target_arch {
        "aarch64" | "arm64" => "linux-aarch64",
        _ => "linux-64",
    }
}

fn load_top_tools(tools_csv: &Path, top_n: usize) -> Result<Vec<PriorityTool>> {
    let mut rows = load_tools_csv_rows(tools_csv)?;
    rows.truncate(top_n);
    Ok(rows)
}

fn load_tools_csv_rows(tools_csv: &Path) -> Result<Vec<PriorityTool>> {
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
    Ok(rows)
}

fn load_software_list(software_list: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(software_list)
        .with_context(|| format!("reading software list {}", software_list.display()))?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (idx, line) in text.lines().enumerate() {
        let cleaned = line
            .split('#')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if cleaned.is_empty() {
            continue;
        }
        let key = normalize_name(&cleaned);
        if key.is_empty() {
            continue;
        }
        if seen.insert(key) {
            out.push(cleaned);
        }
        if out.len() > 10_000 {
            anyhow::bail!(
                "software list {} appears too large or malformed (line {})",
                software_list.display(),
                idx + 1
            );
        }
    }
    if out.is_empty() {
        anyhow::bail!("software list {} is empty", software_list.display());
    }
    Ok(out)
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

    let parsed_result =
        match parse_meta_for_resolved(&resolved, metadata_adapter, &build_config.target_arch) {
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

    let version_state = match payload_version_state(
        &build_config.topdir,
        &build_config.target_root,
        &software_slug,
        &parsed.version,
    ) {
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
    if !build_config.force_rebuild
        && let PayloadVersionState::UpToDate { existing_version } = &version_state
    {
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
    if build_config.force_rebuild {
        log_progress(format!(
            "phase=package status=force-rebuild package={} version={} reason=explicit-force-flag",
            tool.software, parsed.version
        ));
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
    let python_recipe = is_python_recipe(&parsed) || python_script_hint;
    let python_runtime = select_phoreus_python_runtime(&parsed, python_recipe);
    if let Err(err) = ensure_phoreus_python_bootstrap(build_config, specs_dir, python_runtime) {
        let reason = format!("bootstrapping Phoreus Python runtime failed: {err}");
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
    let meta_version = match next_meta_package_version(
        &build_config.topdir,
        &build_config.target_root,
        &software_slug,
    ) {
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
        if is_cancellation_failure(&reason) {
            clear_quarantine_note(bad_spec_dir, &software_slug);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "skipped".to_string(),
                reason: "cancelled by user".to_string(),
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
        if is_cancellation_failure(&reason) {
            clear_quarantine_note(bad_spec_dir, &software_slug);
            return ReportEntry {
                software: tool.software.clone(),
                priority: tool.priority,
                status: "skipped".to_string(),
                reason: "cancelled by user".to_string(),
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
    let selector_ctx = SelectorContext::for_rpm_build(std::env::consts::ARCH);
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
    env.add_function("cdt", |name: String| name);
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
    fn for_rpm_build(target_arch: &str) -> Self {
        let arch = target_arch;
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
        // HEURISTIC-TEMP(issue=HEUR-0001): Prefer upstream precompiled k8 binary.
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
                out.insert(canonical_r_package_name(pkg));
            }
        }
    }
    out.into_iter().collect()
}

fn canonical_r_package_name(name: &str) -> String {
    let normalized = name.trim().to_lowercase().replace('-', ".");
    match normalized.as_str() {
        "rcurl" => "RCurl".to_string(),
        "xml" => "XML".to_string(),
        other => other.to_string(),
    }
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

#[allow(dead_code)]
fn build_python_requirements(parsed: &ParsedMeta) -> Vec<String> {
    build_python_requirements_for_runtime(
        parsed,
        PHOREUS_PYTHON_RUNTIME_311.major,
        PHOREUS_PYTHON_RUNTIME_311.minor,
    )
}

fn build_python_requirements_for_runtime(
    parsed: &ParsedMeta,
    runtime_major: u64,
    runtime_minor: u64,
) -> Vec<String> {
    let runtime_incompatible =
        recipe_python_runtime_incompatible_with(parsed, runtime_major, runtime_minor);
    let mut out = BTreeSet::new();
    for raw in &parsed.host_dep_specs_raw {
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

#[allow(dead_code)]
fn recipe_python_runtime_incompatible(parsed: &ParsedMeta) -> bool {
    recipe_python_runtime_incompatible_with(
        parsed,
        PHOREUS_PYTHON_RUNTIME_311.major,
        PHOREUS_PYTHON_RUNTIME_311.minor,
    )
}

fn recipe_python_runtime_incompatible_with(
    parsed: &ParsedMeta,
    runtime_major: u64,
    runtime_minor: u64,
) -> bool {
    parsed
        .build_dep_specs_raw
        .iter()
        .chain(parsed.host_dep_specs_raw.iter())
        .chain(parsed.run_dep_specs_raw.iter())
        .any(|raw| python_dep_spec_conflicts_with_runtime(raw, runtime_major, runtime_minor))
}

fn select_phoreus_python_runtime(parsed: &ParsedMeta, python_recipe: bool) -> PhoreusPythonRuntime {
    if !python_recipe {
        return PHOREUS_PYTHON_RUNTIME_311;
    }
    if parsed
        .build_deps
        .iter()
        .chain(parsed.host_deps.iter())
        .chain(parsed.run_deps.iter())
        .map(|dep| normalize_dependency_token(dep))
        .any(|dep| dep == PHOREUS_PYTHON_PACKAGE_313)
    {
        return PHOREUS_PYTHON_RUNTIME_313;
    }
    let conflicts_311 = recipe_python_runtime_incompatible_with(
        parsed,
        PHOREUS_PYTHON_RUNTIME_311.major,
        PHOREUS_PYTHON_RUNTIME_311.minor,
    );
    let conflicts_313 = recipe_python_runtime_incompatible_with(
        parsed,
        PHOREUS_PYTHON_RUNTIME_313.major,
        PHOREUS_PYTHON_RUNTIME_313.minor,
    );
    if conflicts_311 && !conflicts_313 {
        PHOREUS_PYTHON_RUNTIME_313
    } else {
        PHOREUS_PYTHON_RUNTIME_311
    }
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
        } else if let Some(version) = token.strip_prefix(">=")
            && let Some((major, minor)) = parse_major_minor(version)
            && (runtime_major, runtime_minor) < (major, minor)
        {
            return true;
        } else if let Some(version) = token.strip_prefix('>')
            && let Some((major, minor)) = parse_major_minor(version)
            && (runtime_major, runtime_minor) <= (major, minor)
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

fn should_keep_rpm_dependency_for_r(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    if !is_r_ecosystem_dependency_name(&normalized) {
        return true;
    }
    if is_r_base_dependency_name(&normalized) {
        return true;
    }
    // Keep Bioconductor RPM edges explicit so local dependency artifacts are
    // hydrated before R install scripts run. This avoids fallback network
    // installs for core BioC libs (for example, zlibbioc -> Rhtslib).
    if normalized.starts_with("bioconductor-") {
        return true;
    }
    // For CRAN-style r-* package names we still prefer Phoreus R runtime
    // restore logic over hard rpmbuild edges against distro repos.
    false
}

fn should_keep_rpm_dependency_for_perl(dep: &str) -> bool {
    let normalized = normalize_dependency_token(dep);
    // Perl test-only modules frequently appear in Bioconda host/test deps but
    // should not hard-block RPM payload builds when upstream tests are not run.
    if normalized == "perl-test" || normalized.starts_with("perl-test-") {
        return false;
    }
    if normalized.starts_with("perl(test") {
        return false;
    }
    true
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
    let python_runtime = select_phoreus_python_runtime(parsed, python_recipe);
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
        build_python_requirements_for_runtime(parsed, python_runtime.major, python_runtime.minor)
    } else {
        Vec::new()
    };
    let needs_isal = recipe_dep_mentions(parsed, "isa-l");
    let needs_libdeflate = recipe_dep_mentions(parsed, "libdeflate")
        || recipe_dep_mentions(parsed, "libdeflate-devel");
    let needs_cereal = recipe_dep_mentions(parsed, "cereal");
    let needs_jemalloc = recipe_dep_mentions(parsed, "jemalloc");
    let needs_libhwy = recipe_dep_mentions(parsed, "libhwy");
    let needs_jsoncpp =
        recipe_dep_mentions(parsed, "jsoncpp") || recipe_dep_mentions(parsed, "jsoncpp-devel");
    let python_venv_setup = render_python_venv_setup_block(python_recipe, &python_requirements);
    let r_runtime_setup =
        render_r_runtime_setup_block(r_runtime_required, r_project_recipe, &r_cran_requirements);
    let rust_runtime_setup = render_rust_runtime_setup_block(rust_runtime_required);
    let nim_runtime_setup = render_nim_runtime_setup_block(nim_runtime_required);
    let core_c_dep_bootstrap = render_core_c_dep_bootstrap_block(
        needs_isal,
        needs_libdeflate,
        needs_cereal,
        needs_jemalloc,
        needs_libhwy,
        needs_jsoncpp,
    );
    let module_lua_env = render_module_lua_env_block(
        python_recipe,
        r_runtime_required,
        rust_runtime_required,
        nim_runtime_required,
    );
    let phoreus_prefix_macro = if perl_recipe {
        format!("/usr/local/phoreus/perl/{PHOREUS_PERL_VERSION}")
    } else {
        "/usr/local/phoreus/%{tool}/%{version}".to_string()
    };
    let module_prefix_path = if perl_recipe {
        format!("/usr/local/phoreus/perl/{PHOREUS_PERL_VERSION}")
    } else {
        format!(
            "/usr/local/phoreus/{software_slug}/{}",
            spec_escape(&parsed.version)
        )
    };
    let perl_runtime_setup = if perl_recipe {
        format!(
            "export PHOREUS_PERL_PREFIX=/usr/local/phoreus/perl/{version}\n\
if [[ -d \"$PHOREUS_PERL_PREFIX/lib/perl5\" ]]; then\n\
  export PATH=\"$PHOREUS_PERL_PREFIX/bin:$PATH\"\n\
  export BIOCONDA2RPM_PERL_RUNTIME=phoreus\n\
else\n\
  # Keep builds functional when Phoreus Perl is not preinstalled in container.\n\
  # Payload still installs into Phoreus prefix via PREFIX.\n\
  export BIOCONDA2RPM_PERL_RUNTIME=system\n\
  echo \"Phoreus Perl runtime not present; falling back to system perl for build-time execution\" >&2\n\
fi\n\
export PERL5LIB=\"$PREFIX/lib/perl5:$PREFIX/lib64/perl5${{PERL5LIB:+:$PERL5LIB}}\"\n\
export PERL_LOCAL_LIB_ROOT=\"$PREFIX\"\n\
export PERL_MM_OPT=\"${{PERL_MM_OPT:+$PERL_MM_OPT }}INSTALL_BASE=$PREFIX\"\n\
export PERL_MB_OPT=\"${{PERL_MB_OPT:+$PERL_MB_OPT }}--install_base $PREFIX\"\n",
            version = PHOREUS_PERL_VERSION
        )
    } else {
        "export PERL_MM_OPT=\"${PERL_MM_OPT:+$PERL_MM_OPT }INSTALL_BASE=$PREFIX\"\n\
export PERL_MB_OPT=\"${PERL_MB_OPT:+$PERL_MB_OPT }--install_base $PREFIX\"\n"
            .to_string()
    };

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
    // HEURISTIC-TEMP(issue=HEUR-0002): userApps archive layout requires extra strip depth.
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
    build_requires.insert(python_runtime.package.to_string());
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
        build_requires.insert("libcurl-devel".to_string());
        build_requires.insert("libxml2-devel".to_string());
        build_requires.insert("openssl-devel".to_string());
        build_requires.insert("libpng-devel".to_string());
        build_requires.insert("libtiff-devel".to_string());
        build_requires.insert("libwebp-devel".to_string());
        build_requires.insert("zlib-devel".to_string());
        build_requires.insert("hdf5-devel".to_string());
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
    if perl_recipe {
        // Use system Perl toolchain for build-time resolution and reserve
        // Phoreus Perl as runtime requirement in generated payload specs.
        build_requires.insert("perl".to_string());
    }
    // HEURISTIC-TEMP(issue=HEUR-0003): monocle3 geospatial native stack mapping.
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
            .filter(|dep| !r_runtime_required || should_keep_rpm_dependency_for_r(dep))
            .map(|d| map_build_dependency(d))
            .filter(|dep| !perl_recipe || should_keep_rpm_dependency_for_perl(dep)),
    );
    build_requires.extend(
        parsed
            .host_deps
            .iter()
            .filter(|dep| !is_conda_only_dependency(dep))
            .filter(|dep| !python_recipe || should_keep_rpm_dependency_for_python(dep))
            .filter(|dep| !r_runtime_required || should_keep_rpm_dependency_for_r(dep))
            .map(|d| map_build_dependency(d))
            .filter(|dep| !perl_recipe || should_keep_rpm_dependency_for_perl(dep)),
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
                .filter(|dep| !r_runtime_required || should_keep_rpm_dependency_for_r(dep))
                .map(|d| map_build_dependency(d)),
        );
    }
    // HEURISTIC-TEMP(issue=HEUR-0004): IGV currently requires Java 21 toolchain at build time.
    if software_slug == "igv" {
        // IGV's Gradle build enforces Java toolchain languageVersion=21.
        build_requires.remove("java-11-openjdk");
        build_requires.insert("java-21-openjdk-devel".to_string());
    }
    // HEURISTIC-TEMP(issue=HEUR-0005): SPAdes ExternalProject performs git clone at configure time.
    if software_slug == "spades" {
        // SPAdes pulls ncbi_vdb_ext via ExternalProject git clone at configure time.
        build_requires.insert("git".to_string());
    }
    build_requires.remove(PHOREUS_PYTHON_PACKAGE);
    build_requires.remove(PHOREUS_PYTHON_PACKAGE_313);
    build_requires.insert(python_runtime.package.to_string());

    let mut runtime_requires = BTreeSet::new();
    runtime_requires.insert("phoreus".to_string());
    if python_recipe {
        runtime_requires.insert(python_runtime.package.to_string());
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
        if perl_recipe {
            runtime_requires.insert(PHOREUS_PERL_PACKAGE.to_string());
        }
        if r_runtime_required {
            runtime_requires.insert(PHOREUS_R_PACKAGE.to_string());
        }
        runtime_requires.extend(
            parsed
                .run_deps
                .iter()
                .filter(|dep| !is_conda_only_dependency(dep))
                .filter(|dep| !r_runtime_required || should_keep_rpm_dependency_for_r(dep))
                .map(|d| map_runtime_dependency(d))
                .filter(|dep| !perl_recipe || should_keep_rpm_dependency_for_perl(dep)),
        );
    }
    // HEURISTIC-TEMP(issue=HEUR-0006): IGV runtime also requires Java 21.
    if software_slug == "igv" {
        runtime_requires.remove("java-11-openjdk");
        runtime_requires.insert("java-21-openjdk".to_string());
    }
    if python_recipe {
        runtime_requires.remove(PHOREUS_PYTHON_PACKAGE);
        runtime_requires.remove(PHOREUS_PYTHON_PACKAGE_313);
        runtime_requires.insert(python_runtime.package.to_string());
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
    let perl_module_provides = if perl_recipe {
        perl_module_name_from_conda(&parsed.package_name)
            .map(|module| format!("Provides:       perl({module}) = %{{version}}-%{{release}}\n"))
            .unwrap_or_default()
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
    {perl_module_provides}\
    Summary:        {summary}\n\
    License:        {license}\n\
    URL:            {homepage}\n\
    {build_arch}\
    {source0_line}\
    Source1:        {build_sh}\n\
    {patch_sources}\n\
    {build_requires}\n\
    {requires}\n\
    %global phoreus_prefix {phoreus_prefix}\n\
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
    export CPU_COUNT=\"${{BIOCONDA2RPM_CPU_COUNT:-1}}\"\n\
    if [[ -z \"$CPU_COUNT\" || \"$CPU_COUNT\" == \"0\" ]]; then\n\
    export CPU_COUNT=1\n\
    fi\n\
    export MAKEFLAGS=\"-j${{CPU_COUNT}}\"\n\
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
    export CPU_COUNT=\"${{BIOCONDA2RPM_CPU_COUNT:-1}}\"\n\
    if [[ -z \"$CPU_COUNT\" || \"$CPU_COUNT\" == \"0\" ]]; then\n\
    export CPU_COUNT=1\n\
    fi\n\
    export MAKEFLAGS=\"-j${{CPU_COUNT}}\"\n\
    export CMAKE_BUILD_PARALLEL_LEVEL=\"$CPU_COUNT\"\n\
    export NINJAFLAGS=\"-j${{CPU_COUNT}}\"\n\
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
    {perl_runtime_setup}\
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
while IFS= read -r -d '' dep_bin; do\n\
  case \":${{PATH:-}}:\" in\n\
    *\":$dep_bin:\"*) ;;\n\
    *) export PATH=\"$dep_bin:$PATH\" ;;\n\
  esac\n\
done < <(find /usr/local/phoreus -mindepth 3 -maxdepth 3 -type d -name bin -print0 2>/dev/null)\n\
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
# Some recipes call `yacc` explicitly while only `bison` is present.\n\
# Provide a deterministic shim instead of per-package overrides.\n\
if ! command -v yacc >/dev/null 2>&1 && command -v bison >/dev/null 2>&1; then\n\
  shim_dir=\"$(pwd)/.bioconda2rpm-shims\"\n\
  mkdir -p \"$shim_dir\"\n\
  cat > \"$shim_dir/yacc\" <<'SHIMEOF'\n\
#!/usr/bin/env bash\n\
exec bison -y \"$@\"\n\
SHIMEOF\n\
  chmod 0755 \"$shim_dir/yacc\"\n\
  export PATH=\"$shim_dir:$PATH\"\n\
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
{core_c_dep_bootstrap}\
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
    # BLAST's make graph uses internal lock orchestration that can stall under\n\
    # parallel worker fan-out in this containerized flow. Force serial payload\n\
    # make execution for deterministic progress.\n\
    sed -i 's|n_workers=8|n_workers=1|g' ./build.sh\n\
    sed -i 's|n_workers=${{CPU_COUNT:-1}}|n_workers=1|g' ./build.sh || true\n\
    # Bioconda's BLAST script removes a temporary linker path with plain `rm`,\n\
    # but in our staged prefix this path may be materialized as a directory.\n\
    # Ensure cleanup succeeds for both symlink and directory forms.\n\
    sed -i 's|^rm \"\\$LIB_INSTALL_DIR\"$|rm -rf \"\\$LIB_INSTALL_DIR\"|g' ./build.sh\n\
    # Newer BLAST source trees can ship subdirectories (for example `lib/outside`).\n\
    # Bioconda's flat `cp $RESULT_PATH/lib/*` fails when a directory is present.\n\
    # Canonicalize all cp-glob spellings (indented, quoted, unquoted) to file-only copy.\n\
    sed -i -E 's|^[[:space:]]*cp[[:space:]]+\"?\\$RESULT_PATH/lib/?\"?\\*[[:space:]]+\"?\\$LIB_INSTALL_DIR/?\"?[[:space:]]*$|find \"\\$RESULT_PATH/lib\" -maxdepth 1 -type f -exec cp -f {{}} \"\\$LIB_INSTALL_DIR\"/ \\\\;|g' ./build.sh || true\n\
    # BLAST passes --with-sqlite3=$PREFIX in conda builds where sqlite lives in\n\
    # that shared prefix. In RPM/container builds sqlite comes from system repos.\n\
    sed -i 's|--with-sqlite3=\\$PREFIX|--with-sqlite3=/usr|g' ./build.sh || true\n\
    sed -i 's|--with-sqlite3=${{PREFIX}}|--with-sqlite3=/usr|g' ./build.sh || true\n\
    sed -i 's|--with-sqlite3=\"\\$PREFIX\"|--with-sqlite3=/usr|g' ./build.sh || true\n\
    sed -i 's|--with-sqlite3=\"${{PREFIX}}\"|--with-sqlite3=/usr|g' ./build.sh || true\n\
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
    fi\n\
    \n\
    # Minimap2 build.sh may pass a quoted empty ARCH_OPTS token to make,\n\
    # which GNU make treats as an invalid empty filename on EL platforms.\n\
    # Normalize to shell expansion that vanishes when ARCH_OPTS is unset.\n\
    if [[ \"%{{tool}}\" == \"minimap2\" ]]; then\n\
    sed -i 's|\"\\$ARCH_OPTS\"|${{ARCH_OPTS:+$ARCH_OPTS}}|g' ./build.sh || true\n\
    sed -i 's|\"${{ARCH_OPTS}}\"|${{ARCH_OPTS:+$ARCH_OPTS}}|g' ./build.sh || true\n\
    sed -i \"s|'\\\\$ARCH_OPTS'|${{ARCH_OPTS:+$ARCH_OPTS}}|g\" ./build.sh || true\n\
    sed -i \"s|'${{ARCH_OPTS}}'|${{ARCH_OPTS:+$ARCH_OPTS}}|g\" ./build.sh || true\n\
    sed -i 's|[[:space:]]\"\"[[:space:]]| |g' ./build.sh || true\n\
    sed -i \"s|[[:space:]]''[[:space:]]| |g\" ./build.sh || true\n\
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
    # Bowtie2 v2.5.x emits ENABLE_x86_64_v3 code paths that use\n\
    # __builtin_cpu_supports(\"x86-64-v3\"), which GCC 11 (EL9 baseline)\n\
    # rejects. Strip the problematic probe/define while keeping upstream\n\
    # target tuning flags intact (required for AVX2 object files).\n\
    if [[ \"%{{tool}}\" == \"bowtie2\" ]]; then\n\
    sed -i 's/-DENABLE_x86_64_v3//g' ./build.sh || true\n\
    sed -i 's/-DENABLE_x86_64_v3//g' Makefile || true\n\
    sed -E -i 's/__builtin_cpu_supports[[:space:]]*\\([[:space:]]*\"x86-64-v3\"[[:space:]]*\\)/0/g' bowtie_main.cpp || true\n\
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
    # Diamond's Bioconda script hard-pins static zstd under PREFIX, but RPM\n\
    # container builds should link system shared zstd.\n\
    if [[ \"%{{tool}}\" == \"diamond\" ]]; then\n\
    echo \"bioconda2rpm: applying diamond zstd shared-library rewrites\" >&2\n\
    # Handle exact and relaxed forms observed in recipe variants.\n\
    sed -i 's|-DZSTD_LIBRARY=\"${{PREFIX}}/lib/libzstd.a\"|-DZSTD_LIBRARY=\"/usr/lib64/libzstd.so\"|g' ./build.sh || true\n\
    sed -i 's|-DZSTD_LIBRARY=\"$PREFIX/lib/libzstd.a\"|-DZSTD_LIBRARY=\"/usr/lib64/libzstd.so\"|g' ./build.sh || true\n\
    sed -E -i 's|-DZSTD_LIBRARY=\"[^\"]*libzstd\\.a\"|-DZSTD_LIBRARY=\"/usr/lib64/libzstd.so\"|g' ./build.sh || true\n\
    sed -i 's|-DZSTD_INCLUDE_DIR=\"${{PREFIX}}/include\"|-DZSTD_INCLUDE_DIR=\"/usr/include\"|g' ./build.sh || true\n\
    sed -i 's|-DZSTD_INCLUDE_DIR=\"$PREFIX/include\"|-DZSTD_INCLUDE_DIR=\"/usr/include\"|g' ./build.sh || true\n\
    if [[ ! -e /usr/lib64/libzstd.so && -e /usr/lib/libzstd.so ]]; then\n\
      sed -i 's|/usr/lib64/libzstd.so|/usr/lib/libzstd.so|g' ./build.sh || true\n\
    fi\n\
    # Emit the resulting cmake args so failures are diagnosable from logs.\n\
    grep -nE 'ZSTD_LIBRARY|ZSTD_INCLUDE_DIR|WITH_ZSTD' ./build.sh || true\n\
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
    # BLAST configure --with-bin-release expects static libstdc++ in-toolchain.\n\
    # EL containers are dynamic-first, so prefer non-bin-release mode.\n\
    if [[ \"%{{tool}}\" == \"blast\" ]]; then\n\
    sed -i 's/--with-bin-release/--without-bin-release/g' ./build.sh || true\n\
    fi\n\
    \n\
    # Qt tooling (qmake) may be packaged under versioned bin roots on EL.\n\
    # Surface common locations so upstream build.sh can invoke qmake directly.\n\
    for qt_bin in /usr/lib64/qt5/bin /usr/lib/qt5/bin /usr/lib64/qt6/bin /usr/lib/qt6/bin; do\n\
    if [[ -d \"$qt_bin\" ]]; then\n\
      export PATH=\"$qt_bin:$PATH\"\n\
    fi\n\
    done\n\
    if ! command -v qmake >/dev/null 2>&1 && command -v qmake-qt5 >/dev/null 2>&1; then\n\
    ln -sf \"$(command -v qmake-qt5)\" /usr/local/bin/qmake || true\n\
    fi\n\
    \n\
    # A number of upstream scripts hardcode aggressive THREADS values.\n\
    # Normalize to canonical CPU_COUNT policy rather than fixed thread counts.\n\
    sed -i -E 's/THREADS=\"-j[0-9]+\"/THREADS=\"-j${{CPU_COUNT:-1}}\"/g' ./build.sh || true\n\
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
    if [[ \"${{BIOCONDA2RPM_ADAPTIVE_RETRY:-0}}\" != \"1\" ]]; then\n\
    exit \"$rc\"\n\
    fi\n\
    if [[ \"${{BIOCONDA2RPM_RETRIED_SERIAL:-0}}\" == \"1\" ]]; then\n\
    exit 1\n\
    fi\n\
    echo \"BIOCONDA2RPM_SERIAL_RETRY_TRIGGERED=1\"\n\
    export BIOCONDA2RPM_RETRIED_SERIAL=1\n\
    export CPU_COUNT=1\n\
    export MAKEFLAGS=-j1\n\
    export CMAKE_BUILD_PARALLEL_LEVEL=1\n\
    export NINJAFLAGS=-j1\n\
    find . -mindepth 1 -maxdepth 1 ! -name \"$(basename \"$retry_snapshot\")\" -exec rm -rf {{}} +\n\
    tar -xf \"$retry_snapshot\"\n\
    bash -eo pipefail ./build.sh\n\
    fi\n\
    rm -f \"$retry_snapshot\"\n\
    \n\
    # Some Bioconda build scripts emit absolute symlinks (and occasionally\n\
    # self-referential broken links) into %{{buildroot}}. Normalize those links\n\
    # so RPM payload validation passes and install prefixes stay relocatable.\n\
    while IFS= read -r -d '' link_path; do\n\
    link_target=$(readlink \"$link_path\" || true)\n\
    [[ -n \"$link_target\" ]] || continue\n\
    link_base=$(basename \"$link_path\")\n\
    if [[ \"$link_target\" == \"$link_base\" ]]; then\n\
      rm -f \"$link_path\"\n\
      continue\n\
    fi\n\
    fixed_target=\"\"\n\
    case \"$link_target\" in\n\
    %{{buildroot}}/*)\n\
      fixed_target=\"${{link_target#%{{buildroot}}}}\"\n\
      ;;\n\
    /*)\n\
      if command -v realpath >/dev/null 2>&1; then\n\
        fixed_target=$(realpath -m --relative-to \"$(dirname \"$link_path\")\" \"$link_target\" 2>/dev/null || true)\n\
      fi\n\
      ;;\n\
    esac\n\
    if [[ -n \"$fixed_target\" ]]; then\n\
      ln -snf \"$fixed_target\" \"$link_path\"\n\
    fi\n\
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
    local prefix = \"{module_prefix_path}\"\n\
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
        phoreus_prefix = phoreus_prefix_macro,
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
        perl_module_provides = perl_module_provides,
        python_venv_setup = python_venv_setup,
        module_lua_env = module_lua_env,
        changelog_date = changelog_date,
        meta_path = spec_escape(&meta_path.display().to_string()),
        variant_dir = spec_escape(&variant_dir.display().to_string()),
        phoreus_python_version = python_runtime.minor_str,
        conda_pkg_name = spec_escape(&parsed.package_name),
        conda_pkg_version = spec_escape(&parsed.version),
        conda_pkg_build_number = spec_escape(&parsed.build_number),
        perl_runtime_setup = perl_runtime_setup,
        r_runtime_setup = r_runtime_setup,
        rust_runtime_setup = rust_runtime_setup,
        nim_runtime_setup = nim_runtime_setup,
        core_c_dep_bootstrap = core_c_dep_bootstrap,
        module_prefix_path = module_prefix_path,
    )
}

fn recipe_dep_mentions(parsed: &ParsedMeta, dep_name: &str) -> bool {
    parsed
        .build_deps
        .iter()
        .chain(parsed.host_deps.iter())
        .chain(parsed.run_deps.iter())
        .map(|dep| normalize_dependency_token(dep))
        .any(|dep| dep == dep_name)
}

fn render_core_c_dep_bootstrap_block(
    needs_isal: bool,
    needs_libdeflate: bool,
    needs_cereal: bool,
    needs_jemalloc: bool,
    needs_libhwy: bool,
    needs_jsoncpp: bool,
) -> String {
    if !needs_isal
        && !needs_libdeflate
        && !needs_cereal
        && !needs_jemalloc
        && !needs_libhwy
        && !needs_jsoncpp
    {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(
        "# Bootstrap selected low-level C libraries when distro repos do not\n\
# provide matching RPMs and recipe build scripts require PREFIX linkage.\n\
third_party_root=\"$(pwd)/.bioconda2rpm-thirdparty\"\n\
mkdir -p \"$third_party_root\"\n\
",
    );

    if needs_isal {
        out.push_str(
            "if [[ ! -e \"$PREFIX/lib/libisal.so\" && ! -e \"$PREFIX/lib/libisal.a\" && ! -e \"$PREFIX/lib64/libisal.so\" ]]; then\n\
  echo \"bioconda2rpm: bootstrapping isa-l into $PREFIX\" >&2\n\
  if ! command -v nasm >/dev/null 2>&1; then\n\
    if command -v dnf >/dev/null 2>&1; then dnf -y install nasm >/dev/null 2>&1 || true; fi\n\
    if command -v microdnf >/dev/null 2>&1; then microdnf -y install nasm >/dev/null 2>&1 || true; fi\n\
  fi\n\
  if ! command -v autoreconf >/dev/null 2>&1; then\n\
    if command -v dnf >/dev/null 2>&1; then dnf -y install autoconf automake libtool >/dev/null 2>&1 || true; fi\n\
    if command -v microdnf >/dev/null 2>&1; then microdnf -y install autoconf automake libtool >/dev/null 2>&1 || true; fi\n\
  fi\n\
  pushd \"$third_party_root\" >/dev/null\n\
  rm -rf isa-l-2.31.1\n\
  if command -v curl >/dev/null 2>&1; then\n\
    curl -L --fail --output isa-l-2.31.1.tar.gz https://github.com/intel/isa-l/archive/refs/tags/v2.31.1.tar.gz\n\
  elif command -v wget >/dev/null 2>&1; then\n\
    wget -O isa-l-2.31.1.tar.gz https://github.com/intel/isa-l/archive/refs/tags/v2.31.1.tar.gz\n\
  else\n\
    echo \"missing curl/wget for isa-l bootstrap\" >&2\n\
    exit 44\n\
  fi\n\
  tar -xf isa-l-2.31.1.tar.gz\n\
  cd isa-l-2.31.1\n\
  ./autogen.sh\n\
  ./configure --prefix=\"$PREFIX\"\n\
  make -j\"${CPU_COUNT:-1}\"\n\
  make install\n\
  popd >/dev/null\n\
fi\n\
",
        );
    }

    if needs_libdeflate {
        out.push_str(
            "if [[ ! -e \"$PREFIX/lib/libdeflate.so\" && ! -e \"$PREFIX/lib/libdeflate.a\" && ! -e \"$PREFIX/lib64/libdeflate.so\" ]]; then\n\
  echo \"bioconda2rpm: bootstrapping libdeflate into $PREFIX\" >&2\n\
  if ! command -v cmake >/dev/null 2>&1; then\n\
    if command -v dnf >/dev/null 2>&1; then dnf -y install cmake >/dev/null 2>&1 || true; fi\n\
    if command -v microdnf >/dev/null 2>&1; then microdnf -y install cmake >/dev/null 2>&1 || true; fi\n\
  fi\n\
  pushd \"$third_party_root\" >/dev/null\n\
  rm -rf libdeflate-1.23\n\
  if command -v curl >/dev/null 2>&1; then\n\
    curl -L --fail --output libdeflate-1.23.tar.gz https://github.com/ebiggers/libdeflate/archive/refs/tags/v1.23.tar.gz\n\
  elif command -v wget >/dev/null 2>&1; then\n\
    wget -O libdeflate-1.23.tar.gz https://github.com/ebiggers/libdeflate/archive/refs/tags/v1.23.tar.gz\n\
  else\n\
    echo \"missing curl/wget for libdeflate bootstrap\" >&2\n\
    exit 44\n\
  fi\n\
  tar -xf libdeflate-1.23.tar.gz\n\
  cd libdeflate-1.23\n\
  cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=\"$PREFIX\" -DCMAKE_INSTALL_LIBDIR=lib\n\
  cmake --build build -j\"${CPU_COUNT:-1}\"\n\
  cmake --install build\n\
  popd >/dev/null\n\
fi\n\
",
        );
    }

    if needs_cereal {
        out.push_str(
            "if [[ ! -e \"$PREFIX/include/cereal/cereal.hpp\" ]]; then\n\
  echo \"bioconda2rpm: bootstrapping cereal into $PREFIX\" >&2\n\
  if command -v dnf >/dev/null 2>&1; then dnf -y install cereal-devel >/dev/null 2>&1 || dnf -y install cereal >/dev/null 2>&1 || true; fi\n\
  if command -v microdnf >/dev/null 2>&1; then microdnf -y install cereal-devel >/dev/null 2>&1 || microdnf -y install cereal >/dev/null 2>&1 || true; fi\n\
  if [[ ! -e \"$PREFIX/include/cereal/cereal.hpp\" ]]; then\n\
    pushd \"$third_party_root\" >/dev/null\n\
    rm -rf cereal-1.3.2\n\
    if command -v curl >/dev/null 2>&1; then\n\
      curl -L --fail --output cereal-1.3.2.tar.gz https://github.com/USCiLab/cereal/archive/refs/tags/v1.3.2.tar.gz\n\
    elif command -v wget >/dev/null 2>&1; then\n\
      wget -O cereal-1.3.2.tar.gz https://github.com/USCiLab/cereal/archive/refs/tags/v1.3.2.tar.gz\n\
    else\n\
      echo \"missing curl/wget for cereal bootstrap\" >&2\n\
      exit 44\n\
    fi\n\
    tar -xf cereal-1.3.2.tar.gz\n\
    mkdir -p \"$PREFIX/include\"\n\
    cp -a cereal-1.3.2/include/cereal \"$PREFIX/include/\"\n\
    popd >/dev/null\n\
  fi\n\
fi\n\
",
        );
    }

    if needs_jemalloc {
        out.push_str(
            "if [[ ( ! -e \"$PREFIX/lib/libjemalloc.so\" && ! -e \"$PREFIX/lib/libjemalloc.a\" && ! -e \"$PREFIX/lib64/libjemalloc.so\" ) || ! -e \"$PREFIX/include/jemalloc/jemalloc.h\" ]]; then\n\
  echo \"bioconda2rpm: bootstrapping jemalloc into $PREFIX\" >&2\n\
  if command -v dnf >/dev/null 2>&1; then dnf -y install jemalloc-devel jemalloc >/dev/null 2>&1 || dnf -y install jemalloc >/dev/null 2>&1 || true; fi\n\
  if command -v microdnf >/dev/null 2>&1; then microdnf -y install jemalloc-devel jemalloc >/dev/null 2>&1 || microdnf -y install jemalloc >/dev/null 2>&1 || true; fi\n\
  if [[ ( ! -e \"$PREFIX/lib/libjemalloc.so\" && ! -e \"$PREFIX/lib/libjemalloc.a\" && ! -e \"$PREFIX/lib64/libjemalloc.so\" ) || ! -e \"$PREFIX/include/jemalloc/jemalloc.h\" ]]; then\n\
    pushd \"$third_party_root\" >/dev/null\n\
    rm -rf jemalloc-5.3.0\n\
    if command -v curl >/dev/null 2>&1; then\n\
      curl -L --fail --output jemalloc-5.3.0.tar.bz2 https://github.com/jemalloc/jemalloc/releases/download/5.3.0/jemalloc-5.3.0.tar.bz2\n\
    elif command -v wget >/dev/null 2>&1; then\n\
      wget -O jemalloc-5.3.0.tar.bz2 https://github.com/jemalloc/jemalloc/releases/download/5.3.0/jemalloc-5.3.0.tar.bz2\n\
    else\n\
      echo \"missing curl/wget for jemalloc bootstrap\" >&2\n\
      exit 44\n\
    fi\n\
    tar -xf jemalloc-5.3.0.tar.bz2\n\
    cd jemalloc-5.3.0\n\
    ./configure --prefix=\"$PREFIX\" --libdir=\"$PREFIX/lib\"\n\
    make -j\"${CPU_COUNT:-1}\"\n\
    make install\n\
    popd >/dev/null\n\
  fi\n\
fi\n\
",
        );
    }

    if needs_libhwy {
        out.push_str(
            "if [[ ! -e \"$PREFIX/lib/libhwy.so\" && ! -e \"$PREFIX/lib/libhwy.a\" && ! -e \"$PREFIX/lib64/libhwy.so\" ]]; then\n\
  echo \"bioconda2rpm: bootstrapping libhwy into $PREFIX\" >&2\n\
  if ! command -v cmake >/dev/null 2>&1; then\n\
    if command -v dnf >/dev/null 2>&1; then dnf -y install cmake >/dev/null 2>&1 || true; fi\n\
    if command -v microdnf >/dev/null 2>&1; then microdnf -y install cmake >/dev/null 2>&1 || true; fi\n\
  fi\n\
  pushd \"$third_party_root\" >/dev/null\n\
  rm -rf highway-1.2.0\n\
  if command -v curl >/dev/null 2>&1; then\n\
    curl -L --fail --output highway-1.2.0.tar.gz https://github.com/google/highway/archive/refs/tags/1.2.0.tar.gz\n\
  elif command -v wget >/dev/null 2>&1; then\n\
    wget -O highway-1.2.0.tar.gz https://github.com/google/highway/archive/refs/tags/1.2.0.tar.gz\n\
  else\n\
    echo \"missing curl/wget for libhwy bootstrap\" >&2\n\
    exit 44\n\
  fi\n\
  tar -xf highway-1.2.0.tar.gz\n\
  rm -rf highway-build\n\
  mkdir -p highway-build\n\
  cmake -S highway-1.2.0 -B highway-build -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=\"$PREFIX\" -DCMAKE_INSTALL_LIBDIR=lib -DBUILD_SHARED_LIBS=ON -DHWY_ENABLE_TESTS=OFF\n\
  cmake --build highway-build -j\"${CPU_COUNT:-1}\"\n\
  cmake --install highway-build\n\
  popd >/dev/null\n\
fi\n\
",
        );
    }

    if needs_jsoncpp {
        out.push_str(
            "if [[ ! -e \"$PREFIX/include/json/json.h\" || ( ! -e \"$PREFIX/lib/libjsoncpp.so\" && ! -e \"$PREFIX/lib/libjsoncpp.a\" && ! -e \"$PREFIX/lib64/libjsoncpp.so\" ) ]]; then\n\
  echo \"bioconda2rpm: bootstrapping jsoncpp into $PREFIX\" >&2\n\
  if ! command -v cmake >/dev/null 2>&1; then\n\
    if command -v dnf >/dev/null 2>&1; then dnf -y install cmake >/dev/null 2>&1 || true; fi\n\
    if command -v microdnf >/dev/null 2>&1; then microdnf -y install cmake >/dev/null 2>&1 || true; fi\n\
  fi\n\
  pushd \"$third_party_root\" >/dev/null\n\
  rm -rf jsoncpp-1.9.6\n\
  if command -v curl >/dev/null 2>&1; then\n\
    curl -L --fail --output jsoncpp-1.9.6.tar.gz https://github.com/open-source-parsers/jsoncpp/archive/refs/tags/1.9.6.tar.gz\n\
  elif command -v wget >/dev/null 2>&1; then\n\
    wget -O jsoncpp-1.9.6.tar.gz https://github.com/open-source-parsers/jsoncpp/archive/refs/tags/1.9.6.tar.gz\n\
  else\n\
    echo \"missing curl/wget for jsoncpp bootstrap\" >&2\n\
    exit 44\n\
  fi\n\
  tar -xf jsoncpp-1.9.6.tar.gz\n\
  rm -rf jsoncpp-build\n\
  mkdir -p jsoncpp-build\n\
  cmake -S jsoncpp-1.9.6 -B jsoncpp-build -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=\"$PREFIX\" -DCMAKE_INSTALL_LIBDIR=lib -DBUILD_SHARED_LIBS=ON -DJSONCPP_WITH_TESTS=OFF -DJSONCPP_WITH_POST_BUILD_UNITTEST=OFF -DJSONCPP_WITH_PKGCONFIG_SUPPORT=ON\n\
  cmake --build jsoncpp-build -j\"${CPU_COUNT:-1}\"\n\
  cmake --install jsoncpp-build\n\
  popd >/dev/null\n\
fi\n\
",
        );
    }

    out.push_str(
        "if [[ -d \"$PREFIX/lib64\" ]]; then\n\
  export LIBRARY_PATH=\"$PREFIX/lib64${LIBRARY_PATH:+:$LIBRARY_PATH}\"\n\
  export LD_LIBRARY_PATH=\"$PREFIX/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}\"\n\
  export LDFLAGS=\"-L$PREFIX/lib64 ${LDFLAGS:-}\"\n\
fi\n\
if [[ -d \"$PREFIX/lib\" ]]; then\n\
  export LIBRARY_PATH=\"$PREFIX/lib${LIBRARY_PATH:+:$LIBRARY_PATH}\"\n\
  export LD_LIBRARY_PATH=\"$PREFIX/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}\"\n\
  export LDFLAGS=\"-L$PREFIX/lib ${LDFLAGS:-}\"\n\
fi\n\
if [[ -d \"$PREFIX/include\" ]]; then\n\
  export C_INCLUDE_PATH=\"$PREFIX/include${C_INCLUDE_PATH:+:$C_INCLUDE_PATH}\"\n\
  export CPLUS_INCLUDE_PATH=\"$PREFIX/include${CPLUS_INCLUDE_PATH:+:$CPLUS_INCLUDE_PATH}\"\n\
  export CPPFLAGS=\"-I$PREFIX/include ${CPPFLAGS:-}\"\n\
fi\n\
",
    );

    out
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
normalize_pkg_key <- function(pkg) {{\n\
  tolower(gsub(\"[-_]\", \".\", pkg))\n\
}}\n\
resolve_case <- function(pkg) {{\n\
  if (!length(avail)) return(pkg)\n\
  key <- normalize_pkg_key(pkg)\n\
  if (pkg %in% avail) return(pkg)\n\
  hit <- avail[normalize_pkg_key(avail) == key]\n\
  if (length(hit)) return(hit[[1]])\n\
  pkg\n\
}}\n\
resolved <- unique(vapply(req, resolve_case, character(1)))\n\
canonicalize <- function(pkg) {{\n\
  pkg <- gsub(\"[-_]\", \".\", pkg)\n\
  key <- tolower(pkg)\n\
  if (identical(key, \"rcurl\")) return(\"RCurl\")\n\
  if (identical(key, \"xml\")) return(\"XML\")\n\
  pkg\n\
}}\n\
resolved <- unique(vapply(resolved, canonicalize, character(1)))\n\
dependency_diff <- function(expected, installed) {{\n\
  if (!length(expected)) return(character())\n\
  if (!length(installed)) return(expected)\n\
  installed_keys <- normalize_pkg_key(installed)\n\
  keep <- vapply(expected, function(pkg) {{\n\
    !(normalize_pkg_key(pkg) %in% installed_keys)\n\
  }}, logical(1))\n\
  expected[keep]\n\
}}\n\
installed <- rownames(installed.packages(lib.loc = unique(c(.libPaths(), lib))))\n\
missing <- dependency_diff(resolved, installed)\n\
if (length(missing)) {{\n\
  BiocManager::install(missing, ask = FALSE, update = FALSE, lib = lib, Ncpus = 1)\n\
}}\n\
installed_after <- rownames(installed.packages(lib.loc = unique(c(.libPaths(), lib))))\n\
still_missing <- dependency_diff(resolved, installed_after)\n\
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
    install.packages(paste0(archive_url, tarball), repos = c(CRAN = \"https://cloud.r-project.org\"), dependencies = TRUE, type = \"source\", lib = lib)\n\
    TRUE\n\
  }}, error = function(e) FALSE)\n\
  ok\n\
}}\n\
install_from_local_phoreus_rpm <- function(pkg) {{\n\
  key <- tolower(gsub(\"[._]\", \"-\", pkg))\n\
  patterns <- c(\n\
    sprintf(\"/work/targets/*/RPMS/*/phoreus-bioconductor-%s-*.rpm\", key),\n\
    sprintf(\"/work/targets/*/RPMS/*/phoreus-r-%s-*.rpm\", key),\n\
    sprintf(\"/work/targets/*/RPMS/*/phoreus-%s-*.rpm\", key)\n\
  )\n\
  files <- unique(unlist(lapply(patterns, Sys.glob), use.names = FALSE))\n\
  if (!length(files)) return(FALSE)\n\
  for (rpmf in files) {{\n\
    status <- tryCatch(\n\
      suppressWarnings(system2(\"rpm\", c(\"-Uvh\", \"--nodeps\", \"--force\", rpmf), stdout = FALSE, stderr = FALSE)),\n\
      error = function(e) 1L\n\
    )\n\
    if (is.integer(status) && status == 0L) return(TRUE)\n\
  }}\n\
  FALSE\n\
}}\n\
if (length(still_missing)) {{\n\
  for (pkg in still_missing) {{\n\
    try(install_from_local_phoreus_rpm(pkg), silent = TRUE)\n\
  }}\n\
  installed_after <- rownames(installed.packages(lib.loc = unique(c(.libPaths(), lib))))\n\
  still_missing <- dependency_diff(resolved, installed_after)\n\
}}\n\
if (length(still_missing)) {{\n\
  for (pkg in still_missing) {{\n\
    try(install.packages(pkg, repos = \"https://cloud.r-project.org\", lib = lib), silent = TRUE)\n\
  }}\n\
  installed_after <- rownames(installed.packages(lib.loc = unique(c(.libPaths(), lib))))\n\
  still_missing <- dependency_diff(resolved, installed_after)\n\
}}\n\
if (length(still_missing)) {{\n\
  for (pkg in still_missing) {{\n\
    try(install_from_cran_archive(pkg, lib), silent = TRUE)\n\
  }}\n\
  installed_after <- rownames(installed.packages(lib.loc = unique(c(.libPaths(), lib))))\n\
  still_missing <- dependency_diff(resolved, installed_after)\n\
}}\n\
if (length(still_missing)) {{\n\
  message(\"bioconda2rpm unresolved R deps after restore: \", paste(still_missing, collapse = \",\"))\n\
  quit(save = \"no\", status = 43)\n\
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
    if let Some(mapped) = map_perl_provider_dependency(dep) {
        return mapped;
    }
    if let Some(mapped) = map_perl_core_dependency(dep) {
        return mapped;
    }
    if let Some(mapped) = map_perl_module_dependency(dep) {
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
            return normalized;
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
        // Keep ISA-L as a Bioconda/Phoreus dependency so libraries are staged
        // into the Phoreus prefix expected by fastp-style build scripts.
        "isa-l" => "isa-l".to_string(),
        "jansson" => "jansson-devel".to_string(),
        "jsoncpp" => "jsoncpp".to_string(),
        "jsoncpp-devel" => "jsoncpp".to_string(),
        "libcurl" => "libcurl-devel".to_string(),
        "libgd" => "gd-devel".to_string(),
        "libblas" => "openblas-devel".to_string(),
        // Keep libdeflate as a Bioconda/Phoreus dependency for prefix hydration.
        "libdeflate" => "libdeflate".to_string(),
        "libdeflate-devel" => "libdeflate".to_string(),
        "liblzma-devel" => "xz-devel".to_string(),
        "liblapack" => "lapack-devel".to_string(),
        "libhwy" => "highway-devel".to_string(),
        "libiconv" => "glibc-devel".to_string(),
        "libxau" => "libXau-devel".to_string(),
        "libxdamage" => "libXdamage-devel".to_string(),
        "libxext" => "libXext-devel".to_string(),
        "libxfixes" => "libXfixes-devel".to_string(),
        "libxxf86vm" => "libXxf86vm-devel".to_string(),
        "mesa-libgl-devel" => "mesa-libGL-devel".to_string(),
        "libpng" => "libpng-devel".to_string(),
        "libuuid" => "libuuid-devel".to_string(),
        "libopenssl-static" => "openssl-devel".to_string(),
        "lz4-c" => "lz4-devel".to_string(),
        "lzo" | "lzo2" | "liblzo2" | "liblzo2-dev" | "liblzo2-devel" => "lzo-devel".to_string(),
        "mysql-connector-c" => "mariadb-connector-c-devel".to_string(),
        "ncurses" => "ncurses-devel".to_string(),
        "ninja" => "ninja-build".to_string(),
        "openssl" => "openssl-devel".to_string(),
        "openmpi" => "openmpi-devel".to_string(),
        "sqlite" => "sqlite-devel".to_string(),
        "qt" => "qt5-qtbase-devel qt5-qtsvg-devel".to_string(),
        "llvmdev" => "llvm-devel".to_string(),
        "xorg-libxext" => "libXext-devel".to_string(),
        "xorg-libxfixes" => "libXfixes-devel".to_string(),
        "xz" => "xz-devel".to_string(),
        "zlib" => "zlib-devel".to_string(),
        "zstd" => "libzstd-devel".to_string(),
        "zstd-static" => "libzstd-devel".to_string(),
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
    if let Some(mapped) = map_perl_provider_dependency(dep) {
        return mapped;
    }
    if let Some(mapped) = map_perl_core_dependency(dep) {
        return mapped;
    }
    if let Some(mapped) = map_perl_module_dependency(dep) {
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
            return normalized;
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
        "jsoncpp" => "jsoncpp".to_string(),
        "libblas" => "openblas".to_string(),
        "libhwy" => "highway".to_string(),
        "libiconv" => "glibc".to_string(),
        "libxau" => "libXau".to_string(),
        "libxdamage" => "libXdamage".to_string(),
        "libxext" => "libXext".to_string(),
        "libxfixes" => "libXfixes".to_string(),
        "libxxf86vm" => "libXxf86vm".to_string(),
        "libgd" => "gd".to_string(),
        "libdeflate-devel" => "libdeflate".to_string(),
        "liblzma-devel" => "xz".to_string(),
        "liblapack" => "lapack".to_string(),
        "mesa-libgl-devel" => "mesa-libGL".to_string(),
        "mysql-connector-c" => "mariadb-connector-c".to_string(),
        "lzo" | "lzo2" | "liblzo2" | "liblzo2-dev" | "liblzo2-devel" => "lzo".to_string(),
        "qt" => "qt5-qtbase qt5-qtsvg".to_string(),
        "llvmdev" => "llvm".to_string(),
        "ninja" => "ninja-build".to_string(),
        "zstd-static" => "zstd".to_string(),
        "xorg-libxext" => "libXext".to_string(),
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
            | PHOREUS_PYTHON_PACKAGE_313
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
    for runtime in [PHOREUS_PYTHON_RUNTIME_311, PHOREUS_PYTHON_RUNTIME_313] {
        let spec_name = format!("{}.spec", runtime.package);
        let destination = specs_dir.join(spec_name);
        let spec_body = render_phoreus_python_bootstrap_spec(runtime);
        fs::write(&destination, spec_body).with_context(|| {
            format!(
                "writing bundled python bootstrap spec {}",
                destination.display()
            )
        })?;
        #[cfg(unix)]
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o644))
            .with_context(|| format!("setting permissions on {}", destination.display()))?;
    }
    Ok(())
}

fn ensure_phoreus_python_bootstrap(
    build_config: &BuildConfig,
    specs_dir: &Path,
    runtime: PhoreusPythonRuntime,
) -> Result<()> {
    if topdir_has_package_artifact(
        &build_config.topdir,
        &build_config.target_root,
        runtime.package,
    )? {
        return Ok(());
    }

    let spec_name = format!("{}.spec", runtime.package);
    let spec_path = specs_dir.join(&spec_name);
    if !spec_path.exists() {
        anyhow::bail!(
            "required bundled bootstrap spec missing: {}",
            spec_path.display()
        );
    }
    build_spec_chain_in_container(build_config, &spec_path, runtime.package)
        .with_context(|| format!("building bootstrap package {}", runtime.package))?;
    Ok(())
}

fn ensure_phoreus_perl_bootstrap(build_config: &BuildConfig, specs_dir: &Path) -> Result<()> {
    let lock = PHOREUS_PERL_BOOTSTRAP_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("phoreus Perl bootstrap lock poisoned"))?;

    if topdir_has_package_artifact(
        &build_config.topdir,
        &build_config.target_root,
        PHOREUS_PERL_PACKAGE,
    )? {
        return Ok(());
    }

    let spec_name = format!("{PHOREUS_PERL_PACKAGE}.spec");
    let spec_path = specs_dir.join(&spec_name);
    let spec_body = render_phoreus_perl_bootstrap_spec();
    fs::write(&spec_path, spec_body)
        .with_context(|| format!("writing Perl bootstrap spec {}", spec_path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(&spec_path, fs::Permissions::from_mode(0o644))
        .with_context(|| format!("setting permissions on {}", spec_path.display()))?;

    build_spec_chain_in_container(build_config, &spec_path, PHOREUS_PERL_PACKAGE)
        .with_context(|| format!("building bootstrap package {}", PHOREUS_PERL_PACKAGE))?;
    Ok(())
}

fn ensure_phoreus_r_bootstrap(build_config: &BuildConfig, specs_dir: &Path) -> Result<()> {
    let lock = PHOREUS_R_BOOTSTRAP_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("phoreus R bootstrap lock poisoned"))?;

    if topdir_has_package_artifact(
        &build_config.topdir,
        &build_config.target_root,
        PHOREUS_R_PACKAGE,
    )? {
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

    if topdir_has_package_artifact(
        &build_config.topdir,
        &build_config.target_root,
        PHOREUS_RUST_PACKAGE,
    )? {
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

    if topdir_has_package_artifact(
        &build_config.topdir,
        &build_config.target_root,
        PHOREUS_NIM_PACKAGE,
    )? {
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

fn render_phoreus_python_bootstrap_spec(runtime: PhoreusPythonRuntime) -> String {
    format!(
        "%global py_minor {py_minor}\n\
%global debug_package %{{nil}}\n\
%global __brp_mangle_shebangs %{{nil}}\n\
\n\
Name:           {package}\n\
Version:        {version}\n\
Release:        1%{{?dist}}\n\
Summary:        Phoreus Python %{{py_minor}} runtime built from CPython source\n\
License:        Python-2.0\n\
URL:            https://www.python.org/\n\
Source0:        https://www.python.org/ftp/python/%{{version}}/Python-%{{version}}.tar.xz\n\
\n\
Requires:       phoreus\n\
\n\
%global phoreus_tool python\n\
%global phoreus_prefix /usr/local/phoreus/%{{phoreus_tool}}/%{{py_minor}}\n\
%global phoreus_moddir /usr/local/phoreus/modules/%{{phoreus_tool}}\n\
\n\
BuildRequires:  gcc\n\
BuildRequires:  make\n\
BuildRequires:  openssl-devel\n\
BuildRequires:  bzip2-devel\n\
BuildRequires:  libffi-devel\n\
BuildRequires:  zlib-devel\n\
BuildRequires:  sqlite-devel\n\
BuildRequires:  xz-devel\n\
BuildRequires:  ncurses-devel\n\
\n\
%description\n\
Phoreus CPython %{{version}} runtime package for Python %{{py_minor}}.\n\
Builds CPython from upstream source into a dedicated Phoreus prefix.\n\
\n\
%prep\n\
%autosetup -n Python-%{{version}}\n\
\n\
%build\n\
./configure \\\n\
  --prefix=%{{phoreus_prefix}} \\\n\
  --enable-shared \\\n\
  --with-system-ffi \\\n\
  --with-ensurepip=install\n\
make %{{?_smp_mflags}}\n\
\n\
%install\n\
rm -rf %{{buildroot}}\n\
make install DESTDIR=%{{buildroot}}\n\
ln -sfn python%{{py_minor}} %{{buildroot}}%{{phoreus_prefix}}/bin/python\n\
ln -sfn pip%{{py_minor}} %{{buildroot}}%{{phoreus_prefix}}/bin/pip\n\
# Ensure library/test payload files are not executable; avoids shebang mangling failures.\n\
find %{{buildroot}}%{{phoreus_prefix}}/lib/python%{{py_minor}} -type f -perm /111 -exec chmod a-x {{}} +\n\
\n\
mkdir -p %{{buildroot}}%{{phoreus_moddir}}\n\
cat > %{{buildroot}}%{{phoreus_moddir}}/%{{py_minor}}.lua <<'LUAEOF'\n\
help([[ Phoreus Python {py_minor} runtime module ]])\n\
whatis(\"Name: python\")\n\
whatis(\"Version: {py_minor}\")\n\
local prefix = \"/usr/local/phoreus/python/{py_minor}\"\n\
setenv(\"PHOREUS_PYTHON_VERSION\", \"{py_minor}\")\n\
prepend_path(\"PATH\", pathJoin(prefix, \"bin\"))\n\
prepend_path(\"LD_LIBRARY_PATH\", pathJoin(prefix, \"lib\"))\n\
LUAEOF\n\
chmod 0644 %{{buildroot}}%{{phoreus_moddir}}/%{{py_minor}}.lua\n\
\n\
%files\n\
%{{phoreus_prefix}}/\n\
%{{phoreus_moddir}}/%{{py_minor}}.lua\n\
\n\
%changelog\n\
* Thu Feb 26 2026 Phoreus Builder <packaging@phoreus.local> - {version}-1\n\
- Build CPython {version} from upstream source under Phoreus prefix\n",
        py_minor = runtime.minor_str,
        package = runtime.package,
        version = runtime.full_version,
    )
}

fn render_phoreus_perl_bootstrap_spec() -> String {
    format!(
        "%global debug_package %{{nil}}\n\
\n\
Name:           {package}\n\
Version:        {version}\n\
Release:        1%{{?dist}}\n\
Summary:        Phoreus Perl shared runtime prefix\n\
License:        GPL-1.0-or-later OR Artistic-1.0-Perl\n\
URL:            https://www.perl.org/\n\
\n\
BuildArch:      noarch\n\
Requires:       phoreus\n\
Requires:       perl\n\
\n\
%global phoreus_tool perl\n\
%global phoreus_prefix /usr/local/phoreus/%{{phoreus_tool}}/{version}\n\
%global phoreus_moddir /usr/local/phoreus/modules/%{{phoreus_tool}}\n\
\n\
%description\n\
Shared Perl runtime prefix for Phoreus Perl module payloads.\n\
\n\
%prep\n\
\n\
%build\n\
\n\
%install\n\
rm -rf %{{buildroot}}\n\
install -d %{{buildroot}}%{{phoreus_prefix}}/lib/perl5\n\
install -d %{{buildroot}}%{{phoreus_prefix}}/lib64/perl5\n\
install -d %{{buildroot}}%{{phoreus_moddir}}\n\
cat > %{{buildroot}}%{{phoreus_moddir}}/{version}.lua <<'LUAEOF'\n\
help([[ Phoreus Perl {version} runtime module ]])\n\
whatis(\"Name: perl\")\n\
whatis(\"Version: {version}\")\n\
local prefix = \"/usr/local/phoreus/perl/{version}\"\n\
prepend_path(\"PERL5LIB\", pathJoin(prefix, \"lib/perl5\"))\n\
prepend_path(\"PERL5LIB\", pathJoin(prefix, \"lib64/perl5\"))\n\
setenv(\"PERL_LOCAL_LIB_ROOT\", prefix)\n\
setenv(\"PERL_MB_OPT\", \"--install_base \" .. prefix)\n\
setenv(\"PERL_MM_OPT\", \"INSTALL_BASE=\" .. prefix)\n\
LUAEOF\n\
chmod 0644 %{{buildroot}}%{{phoreus_moddir}}/{version}.lua\n\
\n\
%files\n\
%{{phoreus_prefix}}/\n\
%{{phoreus_moddir}}/{version}.lua\n\
\n\
%changelog\n\
* Thu Feb 26 2026 Phoreus Builder <packaging@phoreus.local> - {version}-1\n\
- Initialize shared Perl runtime prefix for Phoreus module payloads\n",
        package = PHOREUS_PERL_PACKAGE,
        version = PHOREUS_PERL_VERSION,
    )
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
%global __strip /bin/true\n\
%global __objdump /bin/true\n\
%global __os_install_post %{{nil}}\n\
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

fn topdir_has_package_artifact(
    topdir: &Path,
    target_root: &Path,
    package_name: &str,
) -> Result<bool> {
    for file_name in artifact_filenames(topdir, target_root)? {
        if file_name.starts_with(&format!("{package_name}-")) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn map_perl_core_dependency(dep: &str) -> Option<String> {
    let normalized = normalize_dependency_token(dep);
    let mapped = match normalized.as_str() {
        "perl-extutils-makemaker" => "perl-ExtUtils-MakeMaker",
        "perl-common-sense" => "perl-common-sense",
        "perl-compress-raw-bzip2" => "perl-Compress-Raw-Bzip2",
        "perl-compress-raw-zlib" => "perl-Compress-Raw-Zlib",
        "perl-scalar-list-utils" => "perl-Scalar-List-Utils",
        "perl-carp" => "perl-Carp",
        "perl-exporter" => "perl-Exporter",
        "perl-file-path" => "perl-File-Path",
        "perl-file-temp" => "perl-File-Temp",
        "perl-autoloader" => "perl-AutoLoader",
        "perl-base" => "perl",
        "perl-pathtools" => "perl-PathTools",
        "perl-lib" => "perl",
        "perl-module-load" => "perl-Module-Load",
        "perl-params-check" => "perl-Params-Check",
        "perl-storable" => "perl-Storable",
        "perl-version" => "perl-version",
        "perl-encode" => "perl-Encode",
        "perl-data-dumper" => "perl-Data-Dumper",
        "perl-xml-parser" => "perl-XML-Parser",
        _ => return None,
    };
    Some(mapped.to_string())
}

fn map_perl_provider_dependency(dep: &str) -> Option<String> {
    let normalized = normalize_dependency_token(dep);
    let module = normalized.strip_prefix("perl(")?.strip_suffix(')')?.trim();
    if module.is_empty() {
        return None;
    }
    if module == "common::sense" {
        return Some("perl-common-sense".to_string());
    }
    let canonical = canonicalize_perl_module_name(module);
    Some(format!("perl({canonical})"))
}

fn map_perl_module_dependency(dep: &str) -> Option<String> {
    let module = perl_module_name_from_conda(dep)?;
    Some(format!("perl({module})"))
}

fn canonicalize_perl_module_name(module: &str) -> String {
    module
        .split("::")
        .filter(|part| !part.is_empty())
        .map(canonicalize_perl_module_segment)
        .collect::<Vec<_>>()
        .join("::")
}

fn canonicalize_perl_module_segment(segment: &str) -> String {
    match segment {
        "api" => "API".to_string(),
        "cgi" => "CGI".to_string(),
        "cpan" => "CPAN".to_string(),
        "dbd" => "DBD".to_string(),
        "dbi" => "DBI".to_string(),
        "http" => "HTTP".to_string(),
        "idn" => "IDN".to_string(),
        "io" => "IO".to_string(),
        "ipc" => "IPC".to_string(),
        "json" => "JSON".to_string(),
        "lwp" => "LWP".to_string(),
        "mime" => "MIME".to_string(),
        "moreutils" => "MoreUtils".to_string(),
        "ssl" => "SSL".to_string(),
        "uri" => "URI".to_string(),
        "utf8" => "UTF8".to_string(),
        "www" => "WWW".to_string(),
        "xml" => "XML".to_string(),
        "xs" => "XS".to_string(),
        other => {
            let mut chars = other.chars();
            if let Some(first) = chars.next() {
                let mut out = String::new();
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
                out
            } else {
                String::new()
            }
        }
    }
}

fn perl_module_name_from_conda(dep: &str) -> Option<String> {
    let normalized = normalize_dependency_token(dep);
    let module = normalized.strip_prefix("perl-")?;
    if module.is_empty() {
        return None;
    }
    let overridden = match module {
        "test-leaktrace" => Some("Test::LeakTrace".to_string()),
        "json-xs" => Some("JSON::XS".to_string()),
        "list-moreutils" => Some("List::MoreUtils".to_string()),
        "list-moreutils-xs" => Some("List::MoreUtils::XS".to_string()),
        _ => None,
    };
    if let Some(name) = overridden {
        return Some(name);
    }

    let parts = module
        .split('-')
        .filter(|p| !p.is_empty())
        .map(|part| match part {
            "api" => "API".to_string(),
            "cgi" => "CGI".to_string(),
            "cpan" => "CPAN".to_string(),
            "dbi" => "DBI".to_string(),
            "dbd" => "DBD".to_string(),
            "http" => "HTTP".to_string(),
            "io" => "IO".to_string(),
            "ipc" => "IPC".to_string(),
            "json" => "JSON".to_string(),
            "lwp" => "LWP".to_string(),
            "mime" => "MIME".to_string(),
            "ssl" => "SSL".to_string(),
            "uri" => "URI".to_string(),
            "utf8" => "UTF8".to_string(),
            "www" => "WWW".to_string(),
            "xml" => "XML".to_string(),
            "xs" => "XS".to_string(),
            "yaml" => "YAML".to_string(),
            other => {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) => {
                        let mut out = String::new();
                        out.push(first.to_ascii_uppercase());
                        out.push_str(chars.as_str());
                        out
                    }
                    None => String::new(),
                }
            }
        })
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("::"))
    }
}

fn payload_version_state(
    topdir: &Path,
    target_root: &Path,
    software_slug: &str,
    target_version: &str,
) -> Result<PayloadVersionState> {
    let Some(existing) = latest_existing_payload_version(topdir, target_root, software_slug)?
    else {
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

fn latest_existing_payload_version(
    topdir: &Path,
    target_root: &Path,
    software_slug: &str,
) -> Result<Option<String>> {
    let mut versions = BTreeSet::new();
    for name in artifact_filenames(topdir, target_root)? {
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

fn next_meta_package_version(
    topdir: &Path,
    target_root: &Path,
    software_slug: &str,
) -> Result<u64> {
    let mut max_meta = 0u64;
    for name in artifact_filenames(topdir, target_root)? {
        if let Some(v) = extract_meta_package_version_from_name(&name, software_slug)
            && v > max_meta
        {
            max_meta = v;
        }
    }
    Ok(max_meta.saturating_add(1).max(1))
}

fn artifact_filenames(topdir: &Path, target_root: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut visited = HashSet::new();
    let candidates = [
        target_root.join("RPMS"),
        target_root.join("SRPMS"),
        // Backward-compatible read support for legacy flat layout.
        topdir.join("RPMS"),
        topdir.join("SRPMS"),
    ];

    for root in candidates {
        if !visited.insert(root.clone()) {
            continue;
        }
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

fn container_image_exists(engine: &str, image: &str) -> Result<bool> {
    let status = Command::new(engine)
        .arg("image")
        .arg("inspect")
        .arg(image)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("checking container image '{image}' via {engine}"))?;
    Ok(status.success())
}

fn normalize_container_arch(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

fn expected_container_arch_for_target(target_arch: &str) -> &'static str {
    match target_arch {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        _ => "amd64",
    }
}

fn inspect_container_image_arch(engine: &str, image: &str) -> Result<Option<String>> {
    let output = Command::new(engine)
        .arg("image")
        .arg("inspect")
        .arg("--format")
        .arg("{{.Architecture}}")
        .arg(image)
        .output()
        .with_context(|| format!("inspecting container image architecture for '{image}'"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let arch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if arch.is_empty() {
        Ok(None)
    } else {
        Ok(Some(arch))
    }
}

fn container_platform_for_arch(target_arch: &str) -> &'static str {
    match target_arch {
        "aarch64" => "linux/arm64",
        "x86_64" => "linux/amd64",
        _ => "linux/amd64",
    }
}

fn ensure_container_profile_available(
    engine: &str,
    profile: BuildContainerProfile,
    target_arch: &str,
) -> Result<()> {
    let image = profile.image();
    let platform = container_platform_for_arch(target_arch);
    let expected_arch = expected_container_arch_for_target(target_arch);
    if container_image_exists(engine, image)? {
        match inspect_container_image_arch(engine, image)? {
            Some(actual_arch) => {
                let normalized = normalize_container_arch(&actual_arch);
                if normalized == expected_arch {
                    log_progress(format!(
                        "phase=container-profile status=ready profile={:?} image={} source=local arch={} platform={}",
                        profile, image, actual_arch, platform
                    ));
                    return Ok(());
                }
                log_progress(format!(
                    "phase=container-profile status=rebuild profile={:?} image={} reason=platform-mismatch image_arch={} expected_arch={} platform={}",
                    profile, image, actual_arch, expected_arch, platform
                ));
            }
            None => {
                log_progress(format!(
                    "phase=container-profile status=rebuild profile={:?} image={} reason=arch-inspect-unavailable expected_arch={} platform={}",
                    profile, image, expected_arch, platform
                ));
            }
        }
    }

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dockerfile = repo_root.join(profile.dockerfile_path());
    if !dockerfile.exists() {
        anyhow::bail!(
            "container profile {:?} is configured but Dockerfile is missing: {}",
            profile,
            dockerfile.display()
        );
    }

    let started = Instant::now();
    log_progress(format!(
        "phase=container-profile status=building profile={:?} image={} platform={} dockerfile={}",
        profile,
        image,
        platform,
        dockerfile.display()
    ));
    let output = Command::new(engine)
        .arg("build")
        .arg("--platform")
        .arg(platform)
        .arg("-t")
        .arg(image)
        .arg("-f")
        .arg(&dockerfile)
        .arg(&repo_root)
        .output()
        .with_context(|| {
            format!(
                "building container image {} from {} via {}",
                image,
                dockerfile.display(),
                engine
            )
        })?;
    if !output.status.success() {
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let detail = compact_reason(&tail_lines(&combined, 20), 320);
        log_progress(format!(
            "phase=container-profile status=failed profile={:?} image={} elapsed={} detail={}",
            profile,
            image,
            format_elapsed(started.elapsed()),
            detail
        ));
        anyhow::bail!(
            "failed to build container image {} for profile {:?} (engine={} dockerfile={} platform={} exit={}) detail={}",
            image,
            profile,
            engine,
            dockerfile.display(),
            platform,
            output.status,
            detail
        );
    }

    log_progress(format!(
        "phase=container-profile status=built profile={:?} image={} elapsed={} platform={}",
        profile,
        image,
        format_elapsed(started.elapsed()),
        platform
    ));
    Ok(())
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
    let target_rpms_in_container = format!("/work/targets/{}/RPMS", build_config.target_id);
    let target_srpms_in_container = format!("/work/targets/{}/SRPMS", build_config.target_id);
    let legacy_rpms_in_container = "/work/RPMS";
    let work_mount = format!("{}:/work", build_config.topdir.display());
    let container_platform = container_platform_for_arch(&build_config.target_arch);
    let build_label = label.replace('\'', "_");
    let stage_started = Instant::now();
    log_progress(format!(
        "phase=container-build status=queued label={} spec={} image={} target_id={}",
        build_label, spec_name, build_config.container_image, build_config.target_id
    ));
    let logs_dir = build_config.reports_dir.join("build_logs");
    fs::create_dir_all(&logs_dir)
        .with_context(|| format!("creating build logs dir {}", logs_dir.display()))?;
    let final_log_path = logs_dir.join(format!("{}.log", sanitize_label(&build_label)));
    let stability_key = spec_name.replace(".spec", "");
    let requested_jobs = build_config.build_jobs.max(1);
    let cached_parallel_unstable = matches!(build_config.parallel_policy, ParallelPolicy::Adaptive)
        && requested_jobs > 1
        && is_parallel_unstable_cached(&build_config.reports_dir, &stability_key);
    let initial_jobs = match build_config.parallel_policy {
        ParallelPolicy::Serial => 1,
        ParallelPolicy::Adaptive => {
            if cached_parallel_unstable {
                1
            } else {
                requested_jobs
            }
        }
    };
    let adaptive_retry_enabled =
        matches!(build_config.parallel_policy, ParallelPolicy::Adaptive) && initial_jobs > 1;
    log_progress(format!(
        "phase=container-build status=config label={} spec={} parallel_policy={:?} requested_jobs={} initial_jobs={} adaptive_retry={} cache_parallel_unstable={}",
        build_label,
        spec_name,
        build_config.parallel_policy,
        requested_jobs,
        initial_jobs,
        adaptive_retry_enabled,
        cached_parallel_unstable
    ));

    let script = format!(
        "set -euo pipefail\n\
sanitize_field() {{\n\
  printf '%s' \"$1\" | tr '\\n' ' ' | tr '|' '/'\n\
}}\n\
normalize_arch() {{\n\
  case \"$1\" in\n\
    aarch64|arm64) printf 'aarch64' ;;\n\
    x86_64|amd64) printf 'x86_64' ;;\n\
    *) printf '%s' \"$1\" ;;\n\
  esac\n\
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
mkdir -p '{target_rpms_dir}' '{target_srpms_dir}' /work/SOURCES /work/SPECS\n\
expected_arch=$(normalize_arch '{target_arch}')\n\
rpm_arch=$(normalize_arch \"$(rpm --eval '%{{_arch}}' 2>/dev/null || true)\")\n\
uname_arch=$(normalize_arch \"$(uname -m 2>/dev/null || true)\")\n\
actual_arch=\"$rpm_arch\"\n\
if [[ -z \"$actual_arch\" ]]; then\n\
  actual_arch=\"$uname_arch\"\n\
fi\n\
if [[ -z \"$actual_arch\" ]]; then\n\
  echo \"unable to detect container architecture\" >&2\n\
  exit 96\n\
fi\n\
if [[ \"$actual_arch\" != \"$expected_arch\" ]]; then\n\
  echo \"bioconda2rpm architecture mismatch: target=$expected_arch container=$actual_arch (rpm_arch=$rpm_arch uname_arch=$uname_arch)\" >&2\n\
  exit 97\n\
fi\n\
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
export BIOCONDA2RPM_CPU_COUNT={initial_jobs}\n\
if [[ -z \"${{BIOCONDA2RPM_CPU_COUNT}}\" || \"${{BIOCONDA2RPM_CPU_COUNT}}\" == \"0\" ]]; then\n\
  export BIOCONDA2RPM_CPU_COUNT=1\n\
fi\n\
export BIOCONDA2RPM_ADAPTIVE_RETRY={adaptive_retry}\n\
rpm_smp_flags=(--define \"_smp_mflags -j${{BIOCONDA2RPM_CPU_COUNT}}\" --define \"_smp_build_ncpus ${{BIOCONDA2RPM_CPU_COUNT}}\")\n\
source0_url=$(rpmspec -q --srpm --qf '%{{SOURCE0}}\\n' --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' '{spec}' 2>/dev/null | head -n 1 | tr -d '\\r' || true)\n\
if [[ -z \"$source0_url\" || \"$source0_url\" == '(none)' ]]; then\n\
  source0_url=$(rpmspec -P --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' '{spec}' 2>/dev/null | awk '/^Source0:[[:space:]]+/ {{print $2; exit}}' || true)\n\
fi\n\
if [[ -z \"$source0_url\" ]]; then\n\
  source0_url=$(awk '/^Source0:[[:space:]]+/ {{print $2; exit}}' '{spec}' || true)\n\
fi\n\
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
# ClustalW upstream current URL can rot; use deterministic versioned and EBI mirror fallbacks.\n\
if [[ \"$source0_url\" =~ ^https?://(www\\.)?clustal\\.org/download/current/(clustalw-([0-9][0-9A-Za-z\\._-]*))\\.tar\\.gz$ ]]; then\n\
  clustalw_file=\"${{BASH_REMATCH[2]}}.tar.gz\"\n\
  clustalw_version=\"${{BASH_REMATCH[3]}}\"\n\
  source_candidates+=(\"https://www.clustal.org/download/${{clustalw_version}}/${{clustalw_file}}\")\n\
  source_candidates+=(\"http://www.clustal.org/download/${{clustalw_version}}/${{clustalw_file}}\")\n\
  source_candidates+=(\"https://ftp.ebi.ac.uk/pub/software/clustalw2/${{clustalw_version}}/${{clustalw_file}}\")\n\
  source_candidates+=(\"ftp://ftp.ebi.ac.uk/pub/software/clustalw2/${{clustalw_version}}/${{clustalw_file}}\")\n\
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
rpmbuild -bs --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' \"${{rpm_smp_flags[@]}}\" '{spec}'\n\
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
declare -a pm_repo_args\n\
pm_repo_args=()\n\
mapfile -t pm_all_repos < <(\"$pm\" -q repolist all 2>/dev/null | awk 'NR > 1 {{print $1}}' | sed '/^$/d')\n\
for repo in \\\n\
  crb \\\n\
  codeready-builder-for-rhel-9-$(arch)-rpms \\\n\
  codeready-builder-for-rhel-10-$(arch)-rpms; do\n\
  for known_repo in \"${{pm_all_repos[@]:-}}\"; do\n\
    if [[ \"$known_repo\" == \"$repo\" ]]; then\n\
      pm_repo_args+=(\"--enablerepo=$repo\")\n\
      break\n\
    fi\n\
  done\n\
done\n\
pm_install() {{\n\
  \"$pm\" -y --setopt='*.skip_if_unavailable=true' --disablerepo=dropworm \"${{pm_repo_args[@]}}\" install \"$@\"\n\
}}\n\
\n\
declare -A local_candidates\n\
declare -A local_candidate_score\n\
declare -A local_candidates_norm\n\
declare -A local_candidates_norm_score\n\
\n\
normalize_lookup_key() {{\n\
  local key=\"$1\"\n\
  key=$(printf '%s' \"$key\" | tr '[:upper:]' '[:lower:]')\n\
  key=$(printf '%s' \"$key\" | sed -E 's/[[:space:]]+//g; s/[()\\[\\]]//g; s/:://g; s/[-_.]//g')\n\
  printf '%s' \"$key\"\n\
}}\n\
\n\
record_local_candidate() {{\n\
  local candidate_key=\"$1\"\n\
  local rpmf=\"$2\"\n\
  local candidate_score=\"${{3:-1}}\"\n\
  if [[ -z \"$candidate_key\" ]]; then\n\
    return 0\n\
  fi\n\
  local existing_score\n\
  existing_score=\"${{local_candidate_score[$candidate_key]:--1}}\"\n\
  if [[ -n \"${{local_candidates[$candidate_key]:-}}\" && \"$existing_score\" =~ ^[0-9]+$ && \"$candidate_score\" =~ ^[0-9]+$ && \"$existing_score\" -ge \"$candidate_score\" ]]; then\n\
    return 0\n\
  fi\n\
  local_candidates[\"$candidate_key\"]=\"$rpmf\"\n\
  local_candidate_score[\"$candidate_key\"]=\"$candidate_score\"\n\
  local norm_key\n\
  norm_key=$(normalize_lookup_key \"$candidate_key\")\n\
  if [[ -n \"$norm_key\" ]]; then\n\
    local norm_existing_score\n\
    norm_existing_score=\"${{local_candidates_norm_score[$norm_key]:--1}}\"\n\
    if [[ -z \"${{local_candidates_norm[$norm_key]:-}}\" || ! \"$norm_existing_score\" =~ ^[0-9]+$ || ! \"$candidate_score\" =~ ^[0-9]+$ || \"$candidate_score\" -gt \"$norm_existing_score\" ]]; then\n\
      local_candidates_norm[\"$norm_key\"]=\"$rpmf\"\n\
      local_candidates_norm_score[\"$norm_key\"]=\"$candidate_score\"\n\
    fi\n\
  fi\n\
}}\n\
\n\
for rpm_dir in '{target_rpms_dir}' '{legacy_rpms_dir}'; do\n\
  if [[ ! -d \"$rpm_dir\" ]]; then\n\
    continue\n\
  fi\n\
  while IFS= read -r -d '' rpmf; do\n\
    name=$(rpm -qp --qf '%{{NAME}}\\n' \"$rpmf\" 2>/dev/null || true)\n\
    mapfile -t rpm_provides < <(rpm -qp --provides \"$rpmf\" 2>/dev/null || true)\n\
    provides_score=${{#rpm_provides[@]}}\n\
    if [[ -z \"$provides_score\" || \"$provides_score\" == \"0\" ]]; then\n\
      provides_score=1\n\
    fi\n\
    record_local_candidate \"$name\" \"$rpmf\" \"$provides_score\"\n\
    lower_name=$(printf '%s' \"$name\" | tr '[:upper:]' '[:lower:]')\n\
    record_local_candidate \"$lower_name\" \"$rpmf\" \"$provides_score\"\n\
    for provide in \"${{rpm_provides[@]:-}}\"; do\n\
      key=$(printf '%s' \"$provide\" | awk '{{print $1}}')\n\
      record_local_candidate \"$key\" \"$rpmf\" \"$provides_score\"\n\
      lower_key=$(printf '%s' \"$key\" | tr '[:upper:]' '[:lower:]')\n\
      record_local_candidate \"$lower_key\" \"$rpmf\" \"$provides_score\"\n\
    done\n\
  done < <(find \"$rpm_dir\" -type f -name '*.rpm' -print0 2>/dev/null)\n\
done\n\
\n\
lookup_local_candidate() {{\n\
  local req_key=\"$1\"\n\
  local found=\"${{local_candidates[$req_key]:-}}\"\n\
  if [[ -n \"$found\" ]]; then\n\
    printf '%s' \"$found\"\n\
    return 0\n\
  fi\n\
  local req_lower\n\
  req_lower=$(printf '%s' \"$req_key\" | tr '[:upper:]' '[:lower:]')\n\
  found=\"${{local_candidates[$req_lower]:-}}\"\n\
  if [[ -n \"$found\" ]]; then\n\
    printf '%s' \"$found\"\n\
    return 0\n\
  fi\n\
  local req_norm\n\
  req_norm=$(normalize_lookup_key \"$req_key\")\n\
  found=\"${{local_candidates_norm[$req_norm]:-}}\"\n\
  if [[ -n \"$found\" ]]; then\n\
    printf '%s' \"$found\"\n\
    return 0\n\
  fi\n\
  return 1\n\
}}\n\
\n\
declare -A local_installed\n\
install_local_with_hydration() {{\n\
  local req_key=\"$1\"\n\
  local local_rpm\n\
  local_rpm=$(lookup_local_candidate \"$req_key\" || true)\n\
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
      nested_local_rpm=$(lookup_local_candidate \"$req\" || true)\n\
      if [[ -z \"$nested_local_rpm\" ]]; then\n\
        nested_local_rpm=$(lookup_local_candidate \"$candidate\" || true)\n\
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
mapfile -t build_requires < <(rpmspec -q --buildrequires --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' --define \"_smp_build_ncpus ${{BIOCONDA2RPM_CPU_COUNT}}\" '{spec}' | awk '{{print $1}}' | sed '/^$/d' | sort -u)\n\
dep_log=\"/tmp/bioconda2rpm-dep-{label}.log\"\n\
for dep in \"${{build_requires[@]}}\"; do\n\
  if rpm -q --whatprovides \"$dep\" >/dev/null 2>&1; then\n\
    provider=$(rpm -q --whatprovides \"$dep\" | head -n 1 || true)\n\
    emit_depgraph \"$dep\" 'resolved' 'installed' \"$provider\" 'already_installed'\n\
    continue\n\
  fi\n\
\n\
  local_rpm=$(lookup_local_candidate \"$dep\" || true)\n\
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
rpmbuild --rebuild --nodeps --define \"_topdir $build_root\" --define '_sourcedir /work/SOURCES' \"${{rpm_smp_flags[@]}}\" \"${{srpm_path}}\"\n\
find \"$build_root/SRPMS\" -type f -name '*.src.rpm' -exec cp -f {{}} '{target_srpms_dir}'/ \\;\n\
while IFS= read -r rpmf; do\n\
  rel=\"${{rpmf#$build_root/RPMS/}}\"\n\
  rpm_subarch=$(printf '%s' \"$rel\" | cut -d'/' -f1)\n\
  rpm_subarch=$(normalize_arch \"$rpm_subarch\")\n\
  if [[ \"$rpm_subarch\" != \"noarch\" && \"$rpm_subarch\" != \"$expected_arch\" ]]; then\n\
    echo \"bioconda2rpm rpm arch path mismatch: rpm=$rpmf subarch=$rpm_subarch expected=$expected_arch\" >&2\n\
    exit 98\n\
  fi\n\
  dst=\"{target_rpms_dir}/$(dirname \"$rel\")\"\n\
  mkdir -p \"$dst\"\n\
  cp -f \"$rpmf\" \"$dst/\"\n\
done < <(find \"$build_root/RPMS\" -type f -name '*.rpm')\n",
        label = build_label,
        spec = sh_single_quote(&spec_in_container),
        target_rpms_dir = target_rpms_in_container,
        target_srpms_dir = target_srpms_in_container,
        legacy_rpms_dir = legacy_rpms_in_container,
        target_arch = build_config.target_arch,
        initial_jobs = initial_jobs,
        adaptive_retry = if adaptive_retry_enabled { 1 } else { 0 },
    );

    let run_once = |attempt: usize| -> Result<(std::process::ExitStatus, String)> {
        if cancellation_requested() {
            return Err(cancellation_error("container build cancelled before start"));
        }
        let step_started = Instant::now();
        let container_name = build_container_name(&build_label, spec_name, attempt);
        log_progress(format!(
            "phase=container-build status=started label={} spec={} attempt={} image={} platform={} container={}",
            build_label,
            spec_name,
            attempt,
            build_config.container_image,
            container_platform,
            container_name
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
            .arg("--name")
            .arg(&container_name)
            .arg("--platform")
            .arg(container_platform)
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
        register_active_container(
            &container_name,
            &build_config.container_engine,
            &build_label,
            spec_name,
        );
        let _container_guard = ActiveContainerGuard::new(container_name.clone());

        let mut heartbeat_rng = seed_heartbeat_rng(&build_label, spec_name, attempt);
        let mut next_heartbeat_at =
            Instant::now() + Duration::from_secs(next_heartbeat_interval_secs(&mut heartbeat_rng));
        loop {
            if child
                .try_wait()
                .with_context(|| format!("polling container build chain for {}", spec_name))?
                .is_some()
            {
                break;
            }
            if cancellation_requested() {
                let _ = stop_active_container_by_name(&container_name, "cancelled by user");
                let _ = child.kill();
                let _ = child.wait();
                return Err(cancellation_error("container build cancelled by user"));
            }
            std::thread::sleep(Duration::from_secs(1));
            if Instant::now() >= next_heartbeat_at {
                let elapsed = step_started.elapsed();
                log_progress(format!(
                    "phase=container-build status=running label={} spec={} attempt={} elapsed={}",
                    build_label,
                    spec_name,
                    attempt,
                    format_elapsed(elapsed)
                ));
                next_heartbeat_at = Instant::now()
                    + Duration::from_secs(next_heartbeat_interval_secs(&mut heartbeat_rng));
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
    let serial_retry_triggered = combined.contains("BIOCONDA2RPM_SERIAL_RETRY_TRIGGERED=1");
    if status.success() && serial_retry_triggered && adaptive_retry_enabled {
        let detail = compact_reason(&tail_lines(&combined, 12), 320);
        match mark_parallel_unstable_cache(
            &build_config.reports_dir,
            &stability_key,
            &detail,
            initial_jobs,
        ) {
            Ok(()) => {
                log_progress(format!(
                    "phase=container-build status=learned-parallel-unstable spec={} target_id={} initial_jobs={} cache={}",
                    spec_name,
                    build_config.target_id,
                    initial_jobs,
                    build_stability_cache_path(&build_config.reports_dir).display()
                ));
            }
            Err(err) => {
                log_progress(format!(
                    "phase=container-build status=cache-write-warning spec={} reason={}",
                    spec_name,
                    compact_reason(&err.to_string(), 240)
                ));
            }
        }
    }

    if !status.success() {
        let arch_policy =
            classify_arch_policy(&combined, &build_config.target_arch).unwrap_or("unknown");
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

fn build_container_name(label: &str, spec_name: &str, attempt: usize) -> String {
    let sanitized_label = sanitize_label(label);
    let sanitized_spec = sanitize_label(spec_name.trim_end_matches(".spec"));
    let clipped_label: String = sanitized_label.chars().take(24).collect();
    let clipped_spec: String = sanitized_spec.chars().take(24).collect();
    let now_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!(
        "bioconda2rpm-{}-{}-a{}-p{}-{}",
        clipped_label,
        clipped_spec,
        attempt,
        std::process::id(),
        now_millis
    )
}

fn build_stability_cache_path(reports_dir: &Path) -> PathBuf {
    reports_dir.join("build_stability.json")
}

fn read_build_stability_cache(path: &Path) -> BTreeMap<String, BuildStabilityRecord> {
    let Ok(raw) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<BTreeMap<String, BuildStabilityRecord>>(&raw).unwrap_or_default()
}

fn is_parallel_unstable_cached(reports_dir: &Path, key: &str) -> bool {
    let lock = BUILD_STABILITY_CACHE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = match lock.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let path = build_stability_cache_path(reports_dir);
    read_build_stability_cache(&path)
        .get(key)
        .map(|entry| entry.status == "parallel_unstable")
        .unwrap_or(false)
}

fn mark_parallel_unstable_cache(
    reports_dir: &Path,
    key: &str,
    detail: &str,
    initial_jobs: usize,
) -> Result<()> {
    let lock = BUILD_STABILITY_CACHE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| anyhow::anyhow!("build stability cache lock poisoned"))?;
    fs::create_dir_all(reports_dir)
        .with_context(|| format!("creating reports dir {}", reports_dir.display()))?;
    let path = build_stability_cache_path(reports_dir);
    let mut cache = read_build_stability_cache(&path);
    cache.insert(
        key.to_string(),
        BuildStabilityRecord {
            status: "parallel_unstable".to_string(),
            updated_at: Utc::now().to_rfc3339(),
            detail: format!("initial_jobs={} detail={}", initial_jobs, detail),
        },
    );
    let payload = serde_json::to_string_pretty(&cache)
        .context("serializing build stability cache json payload")?;
    fs::write(&path, payload)
        .with_context(|| format!("writing build stability cache {}", path.display()))?;
    Ok(())
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
    let kpi = compute_arch_adjusted_kpi(entries);

    let mut md = String::new();
    md.push_str("# Priority SPEC Generation Summary\n\n");
    md.push_str(&format!("- Requested: {}\n", entries.len()));
    md.push_str(&format!("- Generated: {}\n", generated));
    md.push_str(&format!("- Quarantined: {}\n\n", quarantined));
    md.push_str("## Reliability KPI (Arch-Adjusted)\n\n");
    md.push_str("- Rule: architecture-incompatible packages are excluded from denominator.\n");
    md.push_str(&format!("- KPI scope entries: {}\n", kpi.scope_entries));
    md.push_str(&format!(
        "- Excluded (arch-incompatible): {}\n",
        kpi.excluded_arch
    ));
    md.push_str(&format!("- KPI denominator: {}\n", kpi.denominator));
    md.push_str(&format!("- KPI successes: {}\n", kpi.successes));
    md.push_str(&format!("- KPI success rate: {:.2}%\n\n", kpi.success_rate));
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

fn report_entry_is_arch_incompatible(entry: &ReportEntry) -> bool {
    let reason = entry.reason.to_ascii_lowercase();
    reason.contains("arch_policy=amd64_only")
        || reason.contains("arch_policy=aarch64_only")
        || reason.contains("arch_policy=arm64_only")
}

#[derive(Debug, Clone)]
struct RootOutcome {
    status: String,
    reason: String,
    excluded: bool,
    success: bool,
}

fn detect_root_outcome(requested_tool: &str, summary: &BuildSummary) -> Option<RootOutcome> {
    let payload = fs::read_to_string(&summary.report_json).ok()?;
    let entries: Vec<ReportEntry> = serde_json::from_str(&payload).ok()?;
    if entries.is_empty() {
        return None;
    }
    let requested_norm = normalize_name(requested_tool);
    let root_norm = summary
        .build_order
        .last()
        .map(|s| normalize_name(s))
        .unwrap_or_else(|| requested_norm.clone());

    let selected = entries
        .iter()
        .rev()
        .find(|e| normalize_name(&e.software) == root_norm)
        .or_else(|| {
            entries
                .iter()
                .rev()
                .find(|e| normalize_name(&e.software) == requested_norm)
        })
        .or_else(|| entries.last())?;

    let success = selected.status == "generated" || selected.status == "up-to-date";
    let excluded = selected.status == "skipped" || report_entry_is_arch_incompatible(selected);
    Some(RootOutcome {
        status: selected.status.clone(),
        reason: selected.reason.clone(),
        excluded,
        success,
    })
}

fn reason_is_arch_incompatible(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("arch_policy=amd64_only")
        || lower.contains("arch_policy=aarch64_only")
        || lower.contains("arch_policy=arm64_only")
}

fn compute_arch_adjusted_kpi(entries: &[ReportEntry]) -> KpiSummary {
    let scope_entries: Vec<&ReportEntry> = entries
        .iter()
        .filter(|e| e.status != "up-to-date" && e.status != "skipped")
        .collect();
    let excluded_arch = scope_entries
        .iter()
        .filter(|e| report_entry_is_arch_incompatible(e))
        .count();
    let denominator = scope_entries.len().saturating_sub(excluded_arch);
    let successes = scope_entries
        .iter()
        .filter(|e| e.status == "generated" && !report_entry_is_arch_incompatible(e))
        .count();
    let success_rate = if denominator == 0 {
        100.0
    } else {
        (successes as f64 * 100.0) / (denominator as f64)
    };
    KpiSummary {
        scope_entries: scope_entries.len(),
        excluded_arch,
        denominator,
        successes,
        success_rate,
    }
}

fn write_regression_reports(
    entries: &[RegressionReportEntry],
    json_path: &Path,
    csv_path: &Path,
    md_path: &Path,
    args: &RegressionArgs,
    kpi_denominator: usize,
    kpi_successes: usize,
    kpi_success_rate: f64,
) -> Result<()> {
    let json = serde_json::to_string_pretty(entries).context("serializing regression json")?;
    fs::write(json_path, json)
        .with_context(|| format!("writing regression json {}", json_path.display()))?;

    let mut writer = Writer::from_path(csv_path)
        .with_context(|| format!("opening regression csv {}", csv_path.display()))?;
    for entry in entries {
        writer
            .serialize(entry)
            .context("writing regression csv row")?;
    }
    writer.flush().context("flushing regression csv writer")?;

    let attempted = entries.len();
    let succeeded = entries.iter().filter(|e| e.status == "success").count();
    let failed = entries.iter().filter(|e| e.status == "failed").count();
    let excluded = entries.iter().filter(|e| e.status == "excluded").count();

    let mut md = String::new();
    md.push_str("# Regression Campaign Summary\n\n");
    md.push_str(&format!("- Mode: {:?}\n", args.mode));
    md.push_str(&format!("- Requested: {}\n", attempted));
    md.push_str(&format!("- Succeeded: {}\n", succeeded));
    md.push_str(&format!("- Failed: {}\n", failed));
    md.push_str(&format!("- Excluded: {}\n", excluded));
    md.push_str(&format!(
        "- KPI Gate Active: {}\n",
        if args.effective_kpi_gate() {
            "yes"
        } else {
            "no"
        }
    ));
    md.push_str(&format!(
        "- KPI Threshold: {:.2}%\n",
        args.kpi_min_success_rate
    ));
    md.push_str(&format!("- KPI Denominator: {}\n", kpi_denominator));
    md.push_str(&format!("- KPI Successes: {}\n", kpi_successes));
    md.push_str(&format!("- KPI Success Rate: {:.2}%\n\n", kpi_success_rate));
    md.push_str("| Software | Priority | Status | Root Status | Reason |\n");
    md.push_str("|---|---:|---|---|---|\n");
    for e in entries {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            e.software,
            e.priority,
            e.status,
            e.root_status,
            e.reason.replace('|', "\\|")
        ));
    }
    fs::write(md_path, md)
        .with_context(|| format!("writing regression markdown {}", md_path.display()))?;
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
        assert_eq!(map_build_dependency("libdeflate"), "libdeflate".to_string());
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
        assert_eq!(map_build_dependency("isa-l"), "isa-l".to_string());
        assert_eq!(map_build_dependency("xz"), "xz-devel".to_string());
        assert_eq!(map_build_dependency("libcurl"), "libcurl-devel".to_string());
        assert_eq!(map_build_dependency("libpng"), "libpng-devel".to_string());
        assert_eq!(map_build_dependency("liblzo2"), "lzo-devel".to_string());
        assert_eq!(map_build_dependency("liblzo2-dev"), "lzo-devel".to_string());
        assert_eq!(map_runtime_dependency("liblzo2"), "lzo".to_string());
        assert_eq!(
            map_build_dependency("zstd-static"),
            "libzstd-devel".to_string()
        );
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
        assert_eq!(map_build_dependency("sqlite"), "sqlite-devel".to_string());
        assert_eq!(map_build_dependency("cereal"), "cereal-devel".to_string());
        assert_eq!(map_build_dependency("gnuconfig"), "automake".to_string());
        assert_eq!(map_build_dependency("glib"), "glib2-devel".to_string());
        assert_eq!(map_build_dependency("libiconv"), "glibc-devel".to_string());
        assert_eq!(map_build_dependency("libxext"), "libXext-devel".to_string());
        assert_eq!(
            map_build_dependency("libxfixes"),
            "libXfixes-devel".to_string()
        );
        assert_eq!(
            map_build_dependency("mesa-libgl-devel"),
            "mesa-libGL-devel".to_string()
        );
        assert_eq!(
            map_build_dependency("qt"),
            "qt5-qtbase-devel qt5-qtsvg-devel".to_string()
        );
        assert_eq!(map_build_dependency("jsoncpp"), "jsoncpp".to_string());
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
        assert_eq!(map_runtime_dependency("libxext"), "libXext".to_string());
        assert_eq!(map_runtime_dependency("libxfixes"), "libXfixes".to_string());
        assert_eq!(
            map_runtime_dependency("qt"),
            "qt5-qtbase qt5-qtsvg".to_string()
        );
        assert_eq!(map_runtime_dependency("jsoncpp"), "jsoncpp".to_string());
        assert_eq!(map_runtime_dependency("glib"), "glib2".to_string());
        assert_eq!(map_runtime_dependency("liblapack"), "lapack".to_string());
        assert_eq!(map_runtime_dependency("liblzma-devel"), "xz".to_string());
        assert_eq!(map_runtime_dependency("zstd-static"), "zstd".to_string());
        assert_eq!(
            map_runtime_dependency("xorg-libxfixes"),
            "libXfixes".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-canary-stability"),
            "perl(Canary::Stability)".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-types-serialiser"),
            "perl(Types::Serialiser)".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-autoloader"),
            "perl-AutoLoader".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-common-sense"),
            "perl-common-sense".to_string()
        );
        assert_eq!(map_build_dependency("perl-base"), "perl".to_string());
        assert_eq!(map_build_dependency("perl-lib"), "perl".to_string());
        assert_eq!(
            map_build_dependency("perl-version"),
            "perl-version".to_string()
        );
        assert_eq!(map_build_dependency("perl-test"), "perl(Test)".to_string());
        assert_eq!(
            map_build_dependency("perl-test-nowarnings"),
            "perl(Test::Nowarnings)".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-test-leaktrace"),
            "perl(Test::LeakTrace)".to_string()
        );
        assert_eq!(
            map_build_dependency("perl-list-moreutils-xs"),
            "perl(List::MoreUtils::XS)".to_string()
        );
        assert_eq!(
            map_build_dependency("perl(list::moreutils::xs)"),
            "perl(List::MoreUtils::XS)".to_string()
        );
        assert_eq!(
            map_build_dependency("perl(common::sense)"),
            "perl-common-sense".to_string()
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
    fn core_c_bootstrap_empty_when_no_deps_requested() {
        let script = render_core_c_dep_bootstrap_block(false, false, false, false, false, false);
        assert!(script.is_empty());
    }

    #[test]
    fn core_c_bootstrap_includes_cereal_and_jemalloc() {
        let script = render_core_c_dep_bootstrap_block(false, false, true, true, false, false);
        assert!(script.contains("bootstrapping cereal into $PREFIX"));
        assert!(script.contains("USCiLab/cereal"));
        assert!(script.contains("bootstrapping jemalloc into $PREFIX"));
        assert!(script.contains("jemalloc/releases/download/5.3.0"));
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
        assert!(spec.contains("export CPU_COUNT=\"${BIOCONDA2RPM_CPU_COUNT:-1}\""));
        assert!(spec.contains("export MAKEFLAGS=\"-j${CPU_COUNT}\""));
        assert!(spec.contains("if [[ \"${BIOCONDA2RPM_ADAPTIVE_RETRY:-0}\" != \"1\" ]]; then"));
        assert!(spec.contains("BIOCONDA2RPM_SERIAL_RETRY_TRIGGERED=1"));
        assert!(spec.contains("/opt/rh/autoconf271/bin/autoconf"));
        assert!(
            spec.contains("find /usr/local/phoreus -mindepth 3 -maxdepth 3 -type d -name include")
        );
        assert!(spec.contains("export CPATH=\"$dep_include${CPATH:+:$CPATH}\""));
        assert!(spec.contains("find /usr/local/phoreus -mindepth 3 -maxdepth 3 -type d -name bin"));
        assert!(spec.contains("export PATH=\"$dep_bin:$PATH\""));
        assert!(spec.contains("disabled by bioconda2rpm for EL9 compatibility"));
        assert!(spec.contains("if [[ \"${CONFIG_SITE:-}\" == \"NONE\" ]]; then"));
        assert!(spec.contains("export PKG_NAME=\"${PKG_NAME:-blast}\""));
        assert!(spec.contains("export PKG_VERSION=\"${PKG_VERSION:-2.5.0}\""));
        assert!(spec.contains("export PKG_BUILDNUM=\"${PKG_BUILDNUM:-0}\""));
        assert!(spec.contains("export ncbi_cv_lib_boost_test=no"));
        assert!(spec.contains("sed -i -E 's|^[[:space:]]*cp[[:space:]]+"));
        assert!(spec.contains("\\$RESULT_PATH/lib/?"));
        assert!(spec.contains(
            "find \"\\$RESULT_PATH/lib\" -maxdepth 1 -type f -exec cp -f {} \"\\$LIB_INSTALL_DIR\"/ \\\\;"
        ));
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
    - scanpy =1.9.3
    - scipy <1.9.0
    - bbknn >=1.5.0,<1.6.0
    - fa2
    - mnnpy >=0.1.9.5
  run:
    - python >=3
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
    fn python_requirements_add_cython_cap_for_host_pomegranate() {
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
            host_dep_specs_raw: vec![
                "python >=3.8".to_string(),
                "pomegranate >=0.14.8,<=0.14.9".to_string(),
            ],
            run_dep_specs_raw: vec!["python >=3.8".to_string()],
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
    fn r_dependencies_map_to_explicit_r_packages() {
        assert_eq!(map_build_dependency("r-ggplot2"), "r-ggplot2".to_string());
        assert_eq!(
            map_runtime_dependency("bioconductor-limma"),
            "bioconductor-limma".to_string()
        );
        assert_eq!(map_runtime_dependency("r-ggplot2"), "r-ggplot2".to_string());
        assert_eq!(
            map_runtime_dependency("r-base"),
            PHOREUS_R_PACKAGE.to_string()
        );
    }

    #[test]
    fn r_dependency_names_are_canonicalized_for_restore() {
        assert_eq!(canonical_r_package_name("rcurl"), "RCurl".to_string());
        assert_eq!(canonical_r_package_name("xml"), "XML".to_string());
        assert_eq!(canonical_r_package_name("httr"), "httr".to_string());
        assert_eq!(
            canonical_r_package_name("futile-logger"),
            "futile.logger".to_string()
        );
    }

    #[test]
    fn r_project_payload_uses_phoreus_r_runtime_without_hard_cran_rpm_edges() {
        let parsed = ParsedMeta {
            package_name: "r-restfulr".to_string(),
            version: "0.0.16".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/restfulr_0.0.16.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/restfulr".to_string(),
            license: "MIT".to_string(),
            summary: "restfulr".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: vec!["r-base".to_string()],
            host_dep_specs_raw: vec!["r-rcurl".to_string(), "r-yaml".to_string()],
            run_dep_specs_raw: vec![
                "r-rcurl".to_string(),
                "r-rjson".to_string(),
                "r-xml".to_string(),
                "r-yaml".to_string(),
            ],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::from(["r-rcurl".to_string(), "r-yaml".to_string()]),
            run_deps: BTreeSet::from([
                "r-rcurl".to_string(),
                "r-rjson".to_string(),
                "r-xml".to_string(),
                "r-yaml".to_string(),
            ]),
        };

        let spec = render_payload_spec(
            "r-restfulr",
            &parsed,
            "bioconda-r-restfulr-build.sh",
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
        assert!(!spec.contains("BuildRequires:  r-rcurl"));
        assert!(!spec.contains("BuildRequires:  r-yaml"));
        assert!(!spec.contains("Requires:  r-rcurl"));
        assert!(!spec.contains("Requires:  r-rjson"));
        assert!(!spec.contains("Requires:  r-xml"));
        assert!(!spec.contains("Requires:  r-yaml"));
    }

    #[test]
    fn r_project_payload_keeps_bioconductor_rpm_edges_for_local_hydration() {
        let parsed = ParsedMeta {
            package_name: "bioconductor-rhtslib".to_string(),
            version: "3.2.0".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/rhtslib_3.2.0.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/rhtslib".to_string(),
            license: "Artistic-2.0".to_string(),
            summary: "Rhtslib".to_string(),
            source_patches: Vec::new(),
            build_script: None,
            noarch_python: false,
            build_dep_specs_raw: vec!["r-base".to_string()],
            host_dep_specs_raw: vec!["bioconductor-zlibbioc".to_string()],
            run_dep_specs_raw: vec!["bioconductor-zlibbioc".to_string()],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::from(["bioconductor-zlibbioc".to_string()]),
            run_deps: BTreeSet::from(["bioconductor-zlibbioc".to_string()]),
        };

        let spec = render_payload_spec(
            "bioconductor-rhtslib",
            &parsed,
            "bioconda-bioconductor-rhtslib-build.sh",
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
        assert!(spec.contains("BuildRequires:  bioconductor-zlibbioc"));
        assert!(spec.contains("Requires:  bioconductor-zlibbioc"));
        assert!(spec.contains("install_from_local_phoreus_rpm <- function(pkg)"));
        assert!(spec.contains("/work/targets/*/RPMS/*/phoreus-bioconductor-%s-*.rpm"));
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
    fn phoreus_python_bootstrap_spec_is_rendered_with_expected_name() {
        let spec = render_phoreus_python_bootstrap_spec(PHOREUS_PYTHON_RUNTIME_311);
        assert!(spec.contains("Name:           phoreus-python-3.11"));
        assert!(spec.contains("Version:        3.11.14"));
        assert!(spec.contains(
            "Source0:        https://www.python.org/ftp/python/%{version}/Python-%{version}.tar.xz"
        ));
        assert!(spec.contains("BuildRequires:  openssl-devel"));
        assert!(spec.contains("BuildRequires:  sqlite-devel"));
    }

    #[test]
    fn phoreus_python_313_bootstrap_spec_is_rendered_with_expected_name() {
        let spec = render_phoreus_python_bootstrap_spec(PHOREUS_PYTHON_RUNTIME_313);
        assert!(spec.contains("Name:           phoreus-python-3.13"));
        assert!(spec.contains("Version:        3.13.2"));
        assert!(spec.contains(
            "Source0:        https://www.python.org/ftp/python/%{version}/Python-%{version}.tar.xz"
        ));
    }

    #[test]
    fn phoreus_perl_bootstrap_spec_is_rendered_with_expected_name() {
        let spec = render_phoreus_perl_bootstrap_spec();
        assert!(spec.contains("Name:           phoreus-perl-5.32"));
        assert!(spec.contains("Version:        5.32"));
        assert!(spec.contains("Requires:       phoreus"));
        assert!(spec.contains("Requires:       perl"));
        assert!(spec.contains("%{phoreus_prefix}/lib/perl5"));
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
        assert!(!reqs.contains(&"click>=8.0".to_string()));
        assert!(!reqs.iter().any(|r| r.contains("automake")));
    }

    #[test]
    fn python_runtime_selector_prefers_313_for_python_ge_312() {
        let parsed = ParsedMeta {
            package_name: "fusion-report".to_string(),
            version: "4.0.1".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/fusion-report-4.0.1.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/fusion-report".to_string(),
            license: "GPL-3.0-only".to_string(),
            summary: "fusion-report".to_string(),
            source_patches: Vec::new(),
            build_script: Some("$PYTHON -m pip install . --no-deps".to_string()),
            noarch_python: true,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: vec!["python >=3.12".to_string(), "pip".to_string()],
            run_dep_specs_raw: vec!["python >=3.12".to_string()],
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let runtime = select_phoreus_python_runtime(&parsed, true);
        assert_eq!(runtime.package, PHOREUS_PYTHON_PACKAGE_313);

        let spec = render_payload_spec(
            "fusion-report",
            &parsed,
            "bioconda-fusion-report-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            true,
            false,
            false,
        );
        assert!(spec.contains("BuildRequires:  phoreus-python-3.13"));
        assert!(spec.contains("Requires:  phoreus-python-3.13"));
        assert!(spec.contains("export PHOREUS_PYTHON_PREFIX=/usr/local/phoreus/python/3.13"));
        assert!(spec.contains("python3.13"));
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
    fn minimap2_arch_opts_sanitization_is_not_nested_under_samtools_block() {
        let parsed = ParsedMeta {
            package_name: "minimap2".to_string(),
            version: "2.30".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/minimap2-2.30.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://example.invalid/minimap2".to_string(),
            license: "MIT".to_string(),
            summary: "minimap2".to_string(),
            source_patches: Vec::new(),
            build_script: Some("make -j${CPU_COUNT} minimap2 sdust".to_string()),
            noarch_python: false,
            build_dep_specs_raw: Vec::new(),
            host_dep_specs_raw: Vec::new(),
            run_dep_specs_raw: Vec::new(),
            build_deps: BTreeSet::new(),
            host_deps: BTreeSet::new(),
            run_deps: BTreeSet::new(),
        };

        let spec = render_payload_spec(
            "minimap2",
            &parsed,
            "bioconda-minimap2-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );

        assert!(spec.contains("if [[ \"%{tool}\" == \"minimap2\" ]]; then"));
        assert!(spec.contains(
            "sed -i \"s|'\\\\$ARCH_OPTS'|${ARCH_OPTS:+$ARCH_OPTS}|g\" ./build.sh || true"
        ));
        assert!(
            spec.contains(
                "sed -i \"s|'${ARCH_OPTS}'|${ARCH_OPTS:+$ARCH_OPTS}|g\" ./build.sh || true"
            )
        );
        assert!(spec.contains("sed -i 's|[[:space:]]\"\"[[:space:]]| |g' ./build.sh || true"));
        assert!(spec.contains("sed -i \"s|[[:space:]]''[[:space:]]| |g\" ./build.sh || true"));
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
        assert!(spec.contains("Requires:  r-ggplot2"));
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
        assert!(spec.contains("Requires:  perl(Number::Compare)"));
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
        assert!(spec.contains("BuildRequires:  perl(Number::Compare)"));
        assert!(spec.contains("BuildRequires:  perl(Text::Glob)"));
        assert!(!spec.contains(&format!("BuildRequires:  {PHOREUS_PERL_PACKAGE}")));
        assert!(spec.contains("Provides:       perl(File::Find::Rule) = %{version}-%{release}"));
        assert!(spec.contains("lib64/perl5"));
    }

    #[test]
    fn perl_payload_filters_test_only_deps_from_hard_requires() {
        let mut host_deps = BTreeSet::new();
        host_deps.insert("perl-test-leaktrace".to_string());
        host_deps.insert("perl-list-moreutils-xs".to_string());

        let parsed = ParsedMeta {
            package_name: "perl-list-moreutils".to_string(),
            version: "0.430".to_string(),
            build_number: "0".to_string(),
            source_url: "https://example.invalid/perl-list-moreutils-0.430.tar.gz".to_string(),
            source_folder: String::new(),
            homepage: "https://metacpan.org".to_string(),
            license: "perl_5".to_string(),
            summary: "Perl package".to_string(),
            source_patches: Vec::new(),
            build_script: Some("perl Makefile.PL".to_string()),
            noarch_python: false,
            build_dep_specs_raw: vec!["make".to_string()],
            host_dep_specs_raw: vec![
                "perl-test-leaktrace".to_string(),
                "perl-list-moreutils-xs".to_string(),
            ],
            run_dep_specs_raw: vec!["perl-list-moreutils-xs".to_string()],
            build_deps: BTreeSet::new(),
            host_deps,
            run_deps: BTreeSet::from(["perl-list-moreutils-xs".to_string()]),
        };

        let spec = render_payload_spec(
            "perl-list-moreutils",
            &parsed,
            "bioconda-perl-list-moreutils-build.sh",
            &[],
            Path::new("/tmp/meta.yaml"),
            Path::new("/tmp"),
            false,
            false,
            false,
            false,
        );
        assert!(!spec.contains("perl(Test::LeakTrace)"));
        assert!(spec.contains("BuildRequires:  perl(List::MoreUtils::XS)"));
    }

    #[test]
    fn perl_dependency_filter_drops_test_capability_forms() {
        let mapped_test = map_build_dependency("perl-test-leaktrace");
        assert_eq!(mapped_test, "perl(Test::LeakTrace)".to_string());
        assert!(!should_keep_rpm_dependency_for_perl(&mapped_test));
        assert!(!should_keep_rpm_dependency_for_perl("perl-test-leaktrace"));
        assert!(should_keep_rpm_dependency_for_perl(
            "perl(List::MoreUtils::XS)"
        ));
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
    - {{ cdt('libxext') }}
  run:
    - {{ pin_subpackage(name, max_pin="x.x") }}
"#;
        let rendered = render_meta_yaml(src).expect("render jinja");
        assert!(rendered.contains("bwa"));
        assert!(rendered.contains("c-compiler"));
        assert!(rendered.contains("libxext"));
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

    #[test]
    fn arch_adjusted_kpi_excludes_arch_incompatible_entries() {
        let entries = vec![
            ReportEntry {
                software: "ok-tool".to_string(),
                priority: 0,
                status: "generated".to_string(),
                reason: "generated".to_string(),
                overlap_recipe: "ok-tool".to_string(),
                overlap_reason: "test".to_string(),
                variant_dir: String::new(),
                package_name: "ok-tool".to_string(),
                version: "1.0".to_string(),
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            },
            ReportEntry {
                software: "arch-limited".to_string(),
                priority: 0,
                status: "quarantined".to_string(),
                reason: "build failed arch_policy=amd64_only".to_string(),
                overlap_recipe: "arch-limited".to_string(),
                overlap_reason: "test".to_string(),
                variant_dir: String::new(),
                package_name: "arch-limited".to_string(),
                version: "1.0".to_string(),
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            },
            ReportEntry {
                software: "real-failure".to_string(),
                priority: 0,
                status: "quarantined".to_string(),
                reason: "payload build failure".to_string(),
                overlap_recipe: "real-failure".to_string(),
                overlap_reason: "test".to_string(),
                variant_dir: String::new(),
                package_name: "real-failure".to_string(),
                version: "1.0".to_string(),
                payload_spec_path: String::new(),
                meta_spec_path: String::new(),
                staged_build_sh: String::new(),
            },
        ];
        let kpi = compute_arch_adjusted_kpi(&entries);
        assert_eq!(kpi.scope_entries, 3);
        assert_eq!(kpi.excluded_arch, 1);
        assert_eq!(kpi.denominator, 2);
        assert_eq!(kpi.successes, 1);
        assert!((kpi.success_rate - 50.0).abs() < 1e-9);
    }

    #[test]
    fn parallel_unstable_cache_is_persisted_per_reports_dir() {
        let unique = format!(
            "bioconda2rpm-stability-cache-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let reports_dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&reports_dir).expect("create temp reports dir");
        let key = "phoreus-blast";
        assert!(!is_parallel_unstable_cached(&reports_dir, key));
        mark_parallel_unstable_cache(&reports_dir, key, "retry succeeded", 8)
            .expect("write stability cache");
        assert!(is_parallel_unstable_cached(&reports_dir, key));
        let _ = std::fs::remove_dir_all(&reports_dir);
    }

    #[test]
    fn package_specific_heuristics_require_retirement_issue_tag() {
        const SOURCE: &str = include_str!("priority_specs.rs");
        let lines: Vec<&str> = SOURCE.lines().collect();
        let mut violations = Vec::new();
        let mut in_software_slug_match = false;

        for (idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("match software_slug {") {
                in_software_slug_match = true;
                continue;
            }
            if in_software_slug_match && trimmed.starts_with('}') {
                in_software_slug_match = false;
                continue;
            }

            let is_direct_package_heuristic = trimmed.starts_with("if software_slug ==")
                || trimmed.starts_with("if package_slug ==");
            let is_match_arm_heuristic =
                in_software_slug_match && trimmed.starts_with('"') && trimmed.contains("=>");
            if !is_direct_package_heuristic && !is_match_arm_heuristic {
                continue;
            }

            if has_heuristic_policy_marker(&lines, idx) {
                continue;
            }
            violations.push(format!("line {}: {}", idx + 1, trimmed));
        }

        assert!(
            violations.is_empty(),
            "missing HEURISTIC-TEMP(issue=...) tags:\n{}",
            violations.join("\n")
        );
    }

    fn has_heuristic_policy_marker(lines: &[&str], idx: usize) -> bool {
        let start = idx.saturating_sub(3);
        lines[start..=idx]
            .iter()
            .any(|line| line.contains("HEURISTIC-TEMP(issue="))
    }
}
