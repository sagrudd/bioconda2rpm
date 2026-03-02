use anyhow::{Context, Result, anyhow};
use chrono::{TimeZone, Utc};
use git2::{Repository, Sort, Time};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BiocondaRecipeMetadata {
    pub recipe_name: String,
    pub canonical_url: Option<String>,
    pub license_raw: String,
    pub language: String,
    pub latest_release: Option<String>,
    pub release_date: String,
    pub release_date_strategy: String,
    pub description: String,
    pub meta_yaml_path: PathBuf,
}

pub fn lookup_recipe_metadata(
    recipe_root: &Path,
    recipe_name: &str,
) -> Result<BiocondaRecipeMetadata> {
    let normalized_name = normalize_recipe_name(recipe_name)
        .ok_or_else(|| anyhow!("invalid Bioconda recipe name: {recipe_name}"))?;
    let meta_yaml_path = resolve_recipe_meta_yaml_path(recipe_root, &normalized_name)?;
    let raw_meta = fs::read_to_string(&meta_yaml_path).with_context(|| {
        format!(
            "read meta.yaml for recipe {normalized_name} at {}",
            meta_yaml_path.display()
        )
    })?;
    let rendered = render_meta_yaml_for_ingress(&raw_meta);
    let meta: Value = serde_yaml::from_str(&rendered).with_context(|| {
        format!(
            "parse rendered meta.yaml for recipe {normalized_name} at {}",
            meta_yaml_path.display()
        )
    })?;

    let package_name = yaml_lookup(&meta, &["package", "name"])
        .and_then(yaml_value_to_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| normalized_name.clone());
    let latest_release = yaml_lookup(&meta, &["package", "version"])
        .and_then(yaml_value_to_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let description = [
        yaml_lookup(&meta, &["about", "description"]).and_then(yaml_value_to_string),
        yaml_lookup(&meta, &["about", "summary"]).and_then(yaml_value_to_string),
    ]
    .into_iter()
    .flatten()
    .map(|value| value.trim().to_string())
    .find(|value| !value.is_empty())
    .unwrap_or_else(|| format!("Imported from Bioconda recipe {package_name}"));

    let urls = collect_candidate_urls(&meta);
    let canonical_url = urls
        .into_iter()
        .find(|value| is_valid_http_url(value))
        .or_else(|| Some(format!("https://anaconda.org/bioconda/{normalized_name}")));

    let license_raw = yaml_lookup(&meta, &["about", "license"])
        .map(read_license_field)
        .unwrap_or_default();
    let language = infer_primary_language(&meta);

    let (release_date, release_date_strategy) =
        if let Some(date) = git_last_modified_date_for_path(&meta_yaml_path) {
            (date, "git_last_modified_commit".to_string())
        } else {
            return Err(anyhow!(
                "could not derive release date from git history for {}",
                meta_yaml_path.display()
            ));
        };

    Ok(BiocondaRecipeMetadata {
        recipe_name: package_name,
        canonical_url,
        license_raw,
        language,
        latest_release,
        release_date,
        release_date_strategy,
        description,
        meta_yaml_path,
    })
}

pub fn is_valid_recipe_name(recipe_name: &str) -> bool {
    normalize_recipe_name(recipe_name).is_some()
}

fn normalize_recipe_name(recipe_name: &str) -> Option<String> {
    let trimmed = recipe_name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lowered = trimmed.to_ascii_lowercase();
    if trimmed != lowered {
        return None;
    }
    let mut chars = lowered.chars();
    let first = chars.next()?;
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return None;
    }
    if !chars.all(|value| {
        value.is_ascii_lowercase() || value.is_ascii_digit() || matches!(value, '-' | '_' | '.')
    }) {
        return None;
    }
    Some(lowered)
}

fn resolve_recipe_meta_yaml_path(recipe_root: &Path, recipe_name: &str) -> Result<PathBuf> {
    let recipe_dir = recipe_root.join(recipe_name);
    if !recipe_dir.is_dir() {
        return Err(anyhow!(
            "recipe directory missing for {recipe_name}: {}",
            recipe_dir.display()
        ));
    }

    let mut candidates = Vec::<RecipeMetaCandidate>::new();
    for meta_name in ["meta.yaml", "meta.yml"] {
        let direct = recipe_dir.join(meta_name);
        if direct.is_file() {
            candidates.push(RecipeMetaCandidate {
                variant_key: "direct".to_string(),
                release_key: recipe_release_key_from_meta(&direct),
                path: direct,
            });
            break;
        }
    }

    let entries = fs::read_dir(&recipe_dir).with_context(|| {
        format!(
            "read recipe variants directory for {recipe_name}: {}",
            recipe_dir.display()
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        for meta_name in ["meta.yaml", "meta.yml"] {
            let candidate_meta = path.join(meta_name);
            if candidate_meta.is_file() {
                let variant = path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
                    .to_string();
                candidates.push(RecipeMetaCandidate {
                    variant_key: variant,
                    release_key: recipe_release_key_from_meta(&candidate_meta),
                    path: candidate_meta,
                });
                break;
            }
        }
    }

    candidates.sort_by(|left, right| compare_recipe_meta_candidates(right, left));
    candidates
        .into_iter()
        .next()
        .map(|candidate| candidate.path)
        .ok_or_else(|| anyhow!("meta.yaml not found for recipe {recipe_name}"))
}

#[derive(Debug, Clone)]
struct RecipeMetaCandidate {
    variant_key: String,
    release_key: Option<String>,
    path: PathBuf,
}

fn recipe_release_key_from_meta(path: &Path) -> Option<String> {
    let raw_meta = fs::read_to_string(path).ok()?;
    let rendered = render_meta_yaml_for_ingress(&raw_meta);
    let meta: Value = serde_yaml::from_str(&rendered).ok()?;
    yaml_lookup(&meta, &["package", "version"])
        .and_then(yaml_value_to_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn compare_recipe_meta_candidates(
    left: &RecipeMetaCandidate,
    right: &RecipeMetaCandidate,
) -> Ordering {
    match (left.release_key.as_deref(), right.release_key.as_deref()) {
        (Some(left_release), Some(right_release)) => {
            let release_cmp = compare_variant_keys(left_release, right_release);
            if release_cmp != Ordering::Equal {
                return release_cmp;
            }
        }
        (Some(_), None) => return Ordering::Greater,
        (None, Some(_)) => return Ordering::Less,
        (None, None) => {}
    }
    compare_variant_keys(&left.variant_key, &right.variant_key)
}

fn compare_variant_keys(left: &str, right: &str) -> Ordering {
    let left_parts = tokenize_version_key(left);
    let right_parts = tokenize_version_key(right);
    for (left_part, right_part) in left_parts.iter().zip(right_parts.iter()) {
        match (left_part, right_part) {
            (VersionPart::Number(l), VersionPart::Number(r)) => match l.cmp(r) {
                Ordering::Equal => {}
                other => return other,
            },
            (VersionPart::Text(l), VersionPart::Text(r)) => match l.cmp(r) {
                Ordering::Equal => {}
                other => return other,
            },
            (VersionPart::Number(_), VersionPart::Text(_)) => return Ordering::Greater,
            (VersionPart::Text(_), VersionPart::Number(_)) => return Ordering::Less,
        }
    }
    left_parts.len().cmp(&right_parts.len())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionPart {
    Number(u64),
    Text(String),
}

fn tokenize_version_key(raw: &str) -> Vec<VersionPart> {
    let mut parts = Vec::<VersionPart>::new();
    let mut current = String::new();
    let mut in_number = None::<bool>;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            let is_number = ch.is_ascii_digit();
            if in_number == Some(is_number) {
                current.push(ch);
            } else {
                if !current.is_empty() {
                    push_version_part(&mut parts, &current, in_number.unwrap_or(false));
                }
                current.clear();
                current.push(ch);
                in_number = Some(is_number);
            }
        } else if !current.is_empty() {
            push_version_part(&mut parts, &current, in_number.unwrap_or(false));
            current.clear();
            in_number = None;
        }
    }
    if !current.is_empty() {
        push_version_part(&mut parts, &current, in_number.unwrap_or(false));
    }
    if parts.is_empty() {
        vec![VersionPart::Text(raw.to_ascii_lowercase())]
    } else {
        parts
    }
}

fn push_version_part(parts: &mut Vec<VersionPart>, raw: &str, numeric: bool) {
    if numeric && let Ok(value) = raw.parse::<u64>() {
        parts.push(VersionPart::Number(value));
        return;
    }
    parts.push(VersionPart::Text(raw.to_ascii_lowercase()));
}

fn render_meta_yaml_for_ingress(raw: &str) -> String {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("{%") || trimmed.starts_with("{#") {
                return None;
            }
            Some(replace_jinja_expressions(line))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn replace_jinja_expressions(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut remaining = line;
    while let Some(start) = remaining.find("{{") {
        output.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];
        let Some(end) = after_start.find("}}") else {
            output.push_str(&remaining[start..]);
            return output;
        };
        let expression = after_start[..end].trim();
        let replacement = expression
            .split(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '-' | '_' | '.'))
            .rfind(|value| !value.is_empty())
            .unwrap_or("value");
        output.push_str(replacement);
        remaining = &after_start[end + 2..];
    }
    output.push_str(remaining);
    output
}

fn yaml_lookup<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = root;
    for key in path {
        let map = current.as_mapping()?;
        let key_value = Value::String((*key).to_string());
        current = map.get(&key_value)?;
    }
    Some(current)
}

fn yaml_value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.trim().to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn read_license_field(value: &Value) -> String {
    if let Some(text) = yaml_value_to_string(value) {
        return text;
    }
    if let Some(sequence) = value.as_sequence() {
        return sequence
            .iter()
            .filter_map(yaml_value_to_string)
            .collect::<Vec<_>>()
            .join(" OR ");
    }
    String::new()
}

fn collect_candidate_urls(meta: &Value) -> Vec<String> {
    let mut urls = Vec::<String>::new();
    for field in ["home", "dev_url", "doc_url"] {
        if let Some(value) = yaml_lookup(meta, &["about", field]).and_then(yaml_value_to_string)
            && !value.is_empty()
        {
            urls.push(value);
        }
    }
    if let Some(source) = yaml_lookup(meta, &["source"]) {
        collect_urls_from_value(source, &mut urls);
    }
    urls
}

fn collect_urls_from_value(value: &Value, urls: &mut Vec<String>) {
    if let Some(text) = yaml_value_to_string(value)
        && is_valid_http_url(&text)
    {
        urls.push(text);
        return;
    }
    if let Some(sequence) = value.as_sequence() {
        for entry in sequence {
            collect_urls_from_value(entry, urls);
        }
        return;
    }
    if let Some(mapping) = value.as_mapping() {
        for key in ["url", "git_url", "hg_url", "svn_url"] {
            if let Some(entry) = mapping.get(Value::String(key.to_string())) {
                collect_urls_from_value(entry, urls);
            }
        }
    }
}

fn is_valid_http_url(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn infer_primary_language(meta: &Value) -> String {
    let mut scores = HashMap::<&'static str, i32>::new();

    let weighted_paths: [(&[&str], i32); 6] = [
        (&["requirements", "run"], 3),
        (&["requirements", "host"], 2),
        (&["requirements", "build"], 1),
        (&["requirements"], 1),
        (&["build"], 1),
        (&["about"], 1),
    ];
    for (path, weight) in weighted_paths {
        if let Some(value) = yaml_lookup(meta, path) {
            score_language_value(value, weight, &mut scores);
        }
    }

    for field in [
        ("build", "script"),
        ("about", "summary"),
        ("about", "description"),
    ] {
        if let Some(text) = yaml_lookup(meta, &[field.0, field.1]).and_then(yaml_value_to_string) {
            score_language_signal(&text.to_ascii_lowercase(), 1, &mut scores);
        }
    }

    let ordered_languages = [
        "Python",
        "R",
        "Perl",
        "Rust",
        "Go",
        "Java",
        "JavaScript",
        "C++",
        "C",
        "Fortran",
    ];
    let mut best_language = "Unknown";
    let mut best_score = 0;
    for language in ordered_languages {
        let score = *scores.get(language).unwrap_or(&0);
        if score > best_score {
            best_score = score;
            best_language = language;
        }
    }
    if best_score > 0 {
        return best_language.to_string();
    }

    // Fallback from rendered `package.name` in meta.yaml for recipes that
    // do not expose language hints in requirements/about sections.
    let package_name = yaml_lookup(meta, &["package", "name"])
        .and_then(yaml_value_to_string)
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    infer_language_from_package_name(&package_name)
        .unwrap_or(best_language)
        .to_string()
}

fn infer_language_from_package_name(name: &str) -> Option<&'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "python"
        || trimmed.starts_with("python-")
        || trimmed.starts_with("py")
        || trimmed.starts_with("pypy")
    {
        return Some("Python");
    }
    if trimmed == "r-base" || trimmed.starts_with("r-") || trimmed.starts_with("bioconductor-") {
        return Some("R");
    }
    if trimmed.starts_with("perl-") {
        return Some("Perl");
    }
    if trimmed.starts_with("rust-") {
        return Some("Rust");
    }
    if trimmed.starts_with("go-") || trimmed.starts_with("golang-") {
        return Some("Go");
    }
    if trimmed.starts_with("node-") || trimmed.starts_with("javascript-") {
        return Some("JavaScript");
    }
    if trimmed.starts_with("java-") {
        return Some("Java");
    }
    None
}

fn score_language_value(value: &Value, weight: i32, scores: &mut HashMap<&'static str, i32>) {
    if let Some(text) = yaml_value_to_string(value) {
        score_language_signal(&text.to_ascii_lowercase(), weight, scores);
        return;
    }
    if let Some(sequence) = value.as_sequence() {
        for item in sequence {
            score_language_value(item, weight, scores);
        }
        return;
    }
    if let Some(mapping) = value.as_mapping() {
        for item in mapping.values() {
            score_language_value(item, weight, scores);
        }
    }
}

fn score_language_signal(signal: &str, weight: i32, scores: &mut HashMap<&'static str, i32>) {
    let mut bump = |language: &'static str, delta: i32| {
        *scores.entry(language).or_insert(0) += delta;
    };
    let tokens = signal
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '+')
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let has_token = |needle: &str| tokens.contains(&needle);

    if signal.contains("python")
        || signal.contains("pip")
        || signal.contains("setuptools")
        || signal.contains("cython")
        || signal.contains("pypy")
    {
        bump("Python", weight * 3);
    }
    if signal.contains("r-base")
        || signal.starts_with("r-")
        || signal.contains(" bioconductor-")
        || signal.contains(" cran-")
    {
        bump("R", weight * 3);
    }
    if signal.contains("perl") {
        bump("Perl", weight * 3);
    }
    if signal.contains("rust") || signal.contains("cargo") {
        bump("Rust", weight * 3);
    }
    if signal.contains("golang") || signal.contains("go_compiler") || signal.contains("go-") {
        bump("Go", weight * 3);
    }
    if signal.contains("openjdk")
        || signal.contains("java")
        || signal.contains("maven")
        || signal.contains("gradle")
    {
        bump("Java", weight * 3);
    }
    if signal.contains("nodejs")
        || signal.contains("npm")
        || signal.contains("yarn")
        || signal.contains("javascript")
    {
        bump("JavaScript", weight * 3);
    }
    if signal.contains("cxx_compiler")
        || signal.contains("compiler('cxx')")
        || signal.contains("gcc-c++")
        || signal.contains("libstdc++")
        || signal.contains("cmake")
        || signal.contains("meson")
    {
        bump("C++", weight * 2);
    }
    if signal.contains("fortran") || signal.contains("gfortran") {
        bump("Fortran", weight * 2);
    }
    if signal.contains("c_compiler")
        || signal.contains("compiler('c')")
        || signal == "gcc"
        || signal == "clang"
        || signal.contains("autoconf")
        || signal.contains("automake")
        || signal.contains("libtool")
    {
        bump("C", weight * 2);
    }
    if has_token("make") || has_token("ninja") {
        bump("C++", weight);
        bump("C", weight);
    }
}

fn git_last_modified_date_for_path(path: &Path) -> Option<String> {
    let repo = Repository::discover(path).ok()?;
    let repo_root = repo.workdir().or_else(|| repo.path().parent())?;
    let relative = path.strip_prefix(repo_root).ok()?;

    let head = repo.head().ok()?.peel_to_commit().ok()?;
    let mut walk = repo.revwalk().ok()?;
    let _ = walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME);
    walk.push(head.id()).ok()?;

    for oid_result in walk {
        let oid = oid_result.ok()?;
        let commit = repo.find_commit(oid).ok()?;
        if !commit_touches_path(&commit, relative) {
            continue;
        }
        return git_time_to_iso_date(commit.time());
    }

    None
}

fn commit_touches_path(commit: &git2::Commit<'_>, relative: &Path) -> bool {
    let Ok(tree) = commit.tree() else {
        return false;
    };
    let current_id = tree.get_path(relative).ok().map(|value| value.id());
    let Some(current_id) = current_id else {
        return false;
    };
    if commit.parent_count() == 0 {
        return true;
    }
    for parent in commit.parents() {
        let parent_id = parent
            .tree()
            .ok()
            .and_then(|value| value.get_path(relative).ok().map(|entry| entry.id()));
        if parent_id != Some(current_id) {
            return true;
        }
    }
    false
}

fn git_time_to_iso_date(value: Time) -> Option<String> {
    Utc.timestamp_opt(value.seconds(), 0)
        .single()
        .map(|date| date.date_naive().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{IndexAddOption, Oid, Signature};
    use tempfile::tempdir;

    fn commit_file(
        repo: &Repository,
        repo_root: &Path,
        relative_path: &Path,
        content: &str,
        message: &str,
        when_unix: i64,
    ) -> Result<Oid> {
        let target = repo_root.join(relative_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create parent directory for {}", target.display()))?;
        }
        fs::write(&target, content).with_context(|| format!("write {}", target.display()))?;

        let mut index = repo.index().context("open git index")?;
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .context("stage files")?;
        index.write().context("write index")?;
        let tree_id = index.write_tree().context("write tree")?;
        let tree = repo.find_tree(tree_id).context("find tree")?;
        let sig = Signature::new("codex", "codex@example.org", &Time::new(when_unix, 0))
            .context("build signature")?;
        let parent = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .and_then(|oid| repo.find_commit(oid).ok());
        let oid = if let Some(parent) = parent {
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])
                .context("commit with parent")?
        } else {
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])
                .context("initial commit")?
        };
        Ok(oid)
    }

    #[test]
    fn lookup_recipe_metadata_prefers_git_last_modified_date() {
        let temp = tempdir().expect("tmp");
        let repo_root = temp.path().join("bioconda-recipes");
        fs::create_dir_all(&repo_root).expect("create repo root");
        let repo = Repository::init(&repo_root).expect("init repo");

        let meta_rel = Path::new("recipes/blast/meta.yaml");
        commit_file(
            &repo,
            &repo_root,
            meta_rel,
            "package:\n  name: blast\n  version: \"2.0.0\"\nabout:\n  home: \"https://example.org/blast\"\n  license: \"MIT\"\nrequirements:\n  host:\n    - cxx_compiler\n",
            "initial blast recipe",
            1_704_067_200, // 2024-01-01
        )
        .expect("commit 1");
        commit_file(
            &repo,
            &repo_root,
            meta_rel,
            "package:\n  name: blast\n  version: \"2.1.0\"\nabout:\n  home: \"https://example.org/blast\"\n  license: \"MIT\"\nrequirements:\n  host:\n    - cxx_compiler\n",
            "update blast version",
            1_706_745_600, // 2024-02-01
        )
        .expect("commit 2");

        let metadata = lookup_recipe_metadata(&repo_root.join("recipes"), "blast").expect("lookup");
        assert_eq!(metadata.recipe_name, "blast");
        assert_eq!(metadata.latest_release.as_deref(), Some("2.1.0"));
        assert_eq!(metadata.language, "C++");
        assert_eq!(metadata.release_date, "2024-02-01");
        assert_eq!(metadata.release_date_strategy, "git_last_modified_commit");
    }

    #[test]
    fn lookup_recipe_metadata_selects_highest_version_across_recipe_entries() {
        let temp = tempdir().expect("tmp");
        let repo_root = temp.path().join("bioconda-recipes");
        fs::create_dir_all(&repo_root).expect("create repo root");
        let repo = Repository::init(&repo_root).expect("init repo");
        let recipes_root = repo_root.join("recipes");
        let recipe_dir = recipes_root.join("samtools");
        fs::create_dir_all(recipe_dir.join("1.20")).expect("create variant 1.20");
        fs::create_dir_all(recipe_dir.join("1.22")).expect("create variant 1.22");

        commit_file(
            &repo,
            &repo_root,
            Path::new("recipes/samtools/meta.yaml"),
            r#"
package:
  name: samtools
  version: "1.18"
about:
  home: "https://example.org/samtools"
  license: "MIT"
"#,
            "seed direct recipe",
            1_706_745_600,
        )
        .expect("commit direct");
        commit_file(
            &repo,
            &repo_root,
            Path::new("recipes/samtools/1.20/meta.yaml"),
            r#"
package:
  name: samtools
  version: "1.20"
about:
  home: "https://example.org/samtools"
  license: "MIT"
"#,
            "seed 1.20 recipe",
            1_706_832_000,
        )
        .expect("commit 1.20");
        commit_file(
            &repo,
            &repo_root,
            Path::new("recipes/samtools/1.22/meta.yaml"),
            r#"
package:
  name: samtools
  version: "1.22"
about:
  home: "https://example.org/samtools"
  license: "MIT"
"#,
            "seed 1.22 recipe",
            1_706_918_400,
        )
        .expect("commit 1.22");

        let metadata = lookup_recipe_metadata(&recipes_root, "samtools").expect("lookup");
        assert_eq!(metadata.latest_release.as_deref(), Some("1.22"));
        assert!(metadata.meta_yaml_path.ends_with("1.22/meta.yaml"));
    }

    #[test]
    fn lookup_recipe_metadata_prefers_about_description_over_summary() {
        let temp = tempdir().expect("tmp");
        let repo_root = temp.path().join("bioconda-recipes");
        fs::create_dir_all(&repo_root).expect("create repo root");
        let repo = Repository::init(&repo_root).expect("init repo");
        let recipes_root = repo_root.join("recipes");

        commit_file(
            &repo,
            &repo_root,
            Path::new("recipes/example/meta.yaml"),
            r#"
package:
  name: example
  version: "0.1.0"
about:
  summary: "Short summary"
  description: "Canonical long description"
  license: "MIT"
"#,
            "seed example recipe",
            1_706_745_600,
        )
        .expect("commit example");

        let metadata = lookup_recipe_metadata(&recipes_root, "example").expect("lookup");
        assert_eq!(metadata.description, "Canonical long description");
    }

    #[test]
    fn infer_primary_language_uses_requirements_and_compilers() {
        let parsed: Value = serde_yaml::from_str(
            r#"
requirements:
  run:
    - python >=3.11
  host:
    - cxx_compiler
    - cmake
"#,
        )
        .expect("yaml");
        assert_eq!(infer_primary_language(&parsed), "Python");

        let parsed_cpp: Value = serde_yaml::from_str(
            r#"
requirements:
  build:
    - cxx_compiler
    - cmake
  host:
    - zlib
"#,
        )
        .expect("yaml");
        assert_eq!(infer_primary_language(&parsed_cpp), "C++");
    }

    #[test]
    fn infer_primary_language_falls_back_to_package_name_when_signals_missing() {
        let parsed_r: Value = serde_yaml::from_str(
            r#"
package:
  name: r-ggplot2
"#,
        )
        .expect("yaml");
        assert_eq!(infer_primary_language(&parsed_r), "R");

        let parsed_python: Value = serde_yaml::from_str(
            r#"
package:
  name: python-click
"#,
        )
        .expect("yaml");
        assert_eq!(infer_primary_language(&parsed_python), "Python");
    }

    #[test]
    fn recipe_name_validation_enforces_bioconda_style() {
        assert!(is_valid_recipe_name("blast"));
        assert!(is_valid_recipe_name("rna-bloom"));
        assert!(!is_valid_recipe_name("Blast"));
        assert!(!is_valid_recipe_name("blast!"));
        assert!(!is_valid_recipe_name(""));
    }
}
