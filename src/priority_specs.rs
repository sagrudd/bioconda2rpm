use crate::cli::GeneratePrioritySpecsArgs;
use anyhow::{Context, Result};
use csv::{ReaderBuilder, Writer};
use minijinja::{Environment, context, value::Kwargs};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs;
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
    homepage: String,
    license: String,
    summary: String,
    build_deps: BTreeSet<String>,
    host_deps: BTreeSet<String>,
    run_deps: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct BuildConfig {
    topdir: PathBuf,
    container_engine: String,
    container_image: String,
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

    let mut tools = load_top_tools(&args.tools_csv, args.top_n)?;
    tools.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.line_no.cmp(&b.line_no)));

    let recipe_dirs = discover_recipe_dirs(&args.recipe_root)?;
    let build_config = BuildConfig {
        topdir: topdir.clone(),
        container_engine: args.container_engine.clone(),
        container_image: args.container_image.clone(),
    };

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
    } else {
        let reason = "recipe does not provide build.sh in selected or root variant".to_string();
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

    let payload_spec_path = specs_dir.join(format!("phoreus-{}.spec", software_slug));
    let meta_spec_path = specs_dir.join(format!("phoreus-{}-default.spec", software_slug));

    let payload_spec = render_payload_spec(
        &software_slug,
        &parsed,
        &staged_build_sh_name,
        &resolved.meta_path,
        &resolved.variant_dir,
    );
    let default_spec = render_default_spec(&software_slug, &parsed);

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

    ReportEntry {
        software: tool.software.clone(),
        priority: tool.priority,
        status: "generated".to_string(),
        reason: "spec/srpm/rpm generated from bioconda metadata in container".to_string(),
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

    let key = normalize_identifier_key(&lower);
    if let Some(recipe) = find_recipe_by_identifier(recipe_root, &key)? {
        return build_resolved(&recipe, "identifier-match");
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
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();

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
        candidates.push((name, path));
    }

    if candidates.is_empty() {
        return Ok(recipe_dir.to_path_buf());
    }

    candidates.sort_by(|a, b| compare_version_labels(&a.0, &b.0));
    Ok(candidates
        .last()
        .map(|(_, p)| p.clone())
        .unwrap_or_else(|| recipe_dir.to_path_buf()))
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
        .render(context! {})
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

    let requirements = root.get("requirements").and_then(Value::as_mapping);
    let build_deps = requirements
        .and_then(|m| m.get(Value::String("build".to_string())))
        .map(extract_deps)
        .unwrap_or_default();

    let host_deps = requirements
        .and_then(|m| m.get(Value::String("host".to_string())))
        .map(extract_deps)
        .unwrap_or_default();

    let run_deps = requirements
        .and_then(|m| m.get(Value::String("run".to_string())))
        .map(extract_deps)
        .unwrap_or_default();

    Ok(ParsedMeta {
        package_name,
        version,
        source_url,
        homepage,
        license,
        summary,
        build_deps,
        host_deps,
        run_deps,
    })
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
    let mapped = match normalized.as_str() {
        "c-compiler" | "ccompiler" => "gcc".to_string(),
        "cxx-compiler" | "cpp-compiler" => "gcc-c++".to_string(),
        "fortran-compiler" => "gcc-gfortran".to_string(),
        other => other.to_string(),
    };

    Some(mapped)
}

fn render_payload_spec(
    software_slug: &str,
    parsed: &ParsedMeta,
    staged_build_sh_name: &str,
    meta_path: &Path,
    variant_dir: &Path,
) -> String {
    let license = spec_escape(&parsed.license);
    let summary = spec_escape(&parsed.summary);
    let homepage = spec_escape_or_default(&parsed.homepage, "https://bioconda.github.io");
    let source_url =
        spec_escape_or_default(&parsed.source_url, "https://example.invalid/source.tar.gz");

    let mut build_requires = BTreeSet::new();
    build_requires.insert("bash".to_string());
    build_requires.extend(parsed.build_deps.iter().cloned());
    build_requires.extend(parsed.host_deps.iter().cloned());

    let mut runtime_requires = BTreeSet::new();
    runtime_requires.insert("phoreus".to_string());
    runtime_requires.extend(parsed.run_deps.iter().cloned());

    let build_requires_lines = format_dep_lines("BuildRequires", &build_requires);
    let requires_lines = format_dep_lines("Requires", &runtime_requires);

    format!(
        "%global debug_package %{{nil}}\n\
%global __brp_mangle_shebangs %{{nil}}\n\
\n\
%global tool {tool}\n\
%global upstream_version {version}\n\
\n\
Name:           phoreus-%{{tool}}-%{{upstream_version}}\n\
Version:        %{{upstream_version}}\n\
Release:        1%{{?dist}}\n\
Summary:        {summary}\n\
License:        {license}\n\
URL:            {homepage}\n\
Source0:        {source_url}\n\
Source1:        {build_sh}\n\
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
mkdir -p buildsrc\n\
tar -xf %{{SOURCE0}} -C buildsrc --strip-components=1\n\
cp %{{SOURCE1}} buildsrc/build.sh\n\
chmod 0755 buildsrc/build.sh\n\
\n\
%build\n\
cd buildsrc\n\
%ifarch aarch64\n\
export BIOCONDA_TARGET_ARCH=aarch64\n\
%else\n\
export BIOCONDA_TARGET_ARCH=x86_64\n\
%endif\n\
export CPU_COUNT=%{{?_smp_build_ncpus}}\n\
\n\
%install\n\
rm -rf %{{buildroot}}\n\
mkdir -p %{{buildroot}}%{{phoreus_prefix}}\n\
cd buildsrc\n\
export PREFIX=%{{buildroot}}%{{phoreus_prefix}}\n\
export CPU_COUNT=%{{?_smp_build_ncpus}}\n\
export CC=${{CC:-gcc}}\n\
export CXX=${{CXX:-g++}}\n\
export CFLAGS=\"${{CFLAGS:-}}\"\n\
export CXXFLAGS=\"${{CXXFLAGS:-}}\"\n\
export CPPFLAGS=\"${{CPPFLAGS:-}}\"\n\
export LDFLAGS=\"${{LDFLAGS:-}}\"\n\
bash ./build.sh\n\
\n\
mkdir -p %{{buildroot}}%{{phoreus_moddir}}\n\
cat > %{{buildroot}}%{{phoreus_moddir}}/%{{version}}.lua <<'LUAEOF'\n\
help([[ {summary} ]])\n\
whatis(\"Name: {tool}\")\n\
whatis(\"Version: {version}\")\n\
whatis(\"URL: {homepage}\")\n\
local prefix = \"/usr/local/phoreus/{tool}/{version}\"\n\
prepend_path(\"PATH\", pathJoin(prefix, \"bin\"))\n\
prepend_path(\"LD_LIBRARY_PATH\", pathJoin(prefix, \"lib\"))\n\
prepend_path(\"MANPATH\", pathJoin(prefix, \"share/man\"))\n\
LUAEOF\n\
chmod 0644 %{{buildroot}}%{{phoreus_moddir}}/%{{version}}.lua\n\
\n\
%files\n\
%{{phoreus_prefix}}/\n\
%{{phoreus_moddir}}/%{{version}}.lua\n\
\n\
%changelog\n\
* Thu Feb 27 2026 bioconda2rpm <packaging@bioconda2rpm.local> - {version}-1\n\
- Auto-generated from Bioconda metadata and build.sh\n",
        tool = software_slug,
        version = spec_escape(&parsed.version),
        summary = summary,
        license = license,
        homepage = homepage,
        source_url = source_url,
        build_sh = spec_escape(staged_build_sh_name),
        build_requires = build_requires_lines,
        requires = requires_lines,
        meta_path = spec_escape(&meta_path.display().to_string()),
        variant_dir = spec_escape(&variant_dir.display().to_string()),
    )
}

fn render_default_spec(software_slug: &str, parsed: &ParsedMeta) -> String {
    let license = spec_escape(&parsed.license);
    let version = spec_escape(&parsed.version);

    format!(
        "%global tool {tool}\n\
%global upstream_version {version}\n\
\n\
Name:           phoreus-%{{tool}}\n\
Version:        1\n\
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
* Thu Feb 27 2026 bioconda2rpm <packaging@bioconda2rpm.local> - 1-1\n\
- Auto-generated default pointer for {tool} {version}\n",
        tool = software_slug,
        version = version,
        license = license,
    )
}

fn format_dep_lines(prefix: &str, deps: &BTreeSet<String>) -> String {
    deps.iter()
        .map(|dep| format!("{prefix}:  {dep}"))
        .collect::<Vec<_>>()
        .join("\n")
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
mkdir -p /work/BUILD /work/BUILDROOT /work/RPMS /work/SOURCES /work/SPECS /work/SRPMS\n\
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
spectool -g -R --define '_topdir /work' --define '_sourcedir /work/SOURCES' '{spec}'\n\
rpmbuild -bs --define '_topdir /work' --define '_sourcedir /work/SOURCES' '{spec}'\n\
rpmbuild -ba --define '_topdir /work' --define '_sourcedir /work/SOURCES' '{spec}'\n",
        label = build_label,
        spec = sh_single_quote(&spec_in_container),
    );

    let status = Command::new(&build_config.container_engine)
        .args([
            "run",
            "--rm",
            "-v",
            &work_mount,
            "-w",
            "/work",
            &build_config.container_image,
            "bash",
            "-lc",
            &script,
        ])
        .status()
        .with_context(|| {
            format!(
                "running container build chain for {} using image {}",
                spec_name, build_config.container_image
            )
        })?;

    if !status.success() {
        anyhow::bail!(
            "container build chain failed for {} (exit status: {})",
            spec_name,
            status
        );
    }

    Ok(())
}

fn sh_single_quote(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
}

fn quarantine_note(bad_spec_dir: &Path, slug: &str, reason: &str) {
    let note_path = bad_spec_dir.join(format!("{slug}.txt"));
    let body = format!("status=quarantined\nreason={reason}\n");
    let _ = fs::write(note_path, body);
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
            Some("openjdk".to_string())
        );
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
