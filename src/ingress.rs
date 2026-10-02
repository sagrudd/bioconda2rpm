use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};

use crate::priority_specs::{
    SelectorContext, apply_selectors, meta_file_path, parse_rendered_meta, render_meta_yaml,
    select_recipe_variant_dir_within_root,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeMetadata {
    pub recipe_name: String,
    pub meta_yaml_path: PathBuf,
    pub canonical_url: Option<String>,
    pub latest_release: Option<String>,
    pub description: String,
    pub license_raw: String,
    pub language: String,
    pub release_date: String,
    pub release_date_strategy: String,
}

pub fn is_valid_recipe_name(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed != "."
        && trimmed != ".."
        && trimmed.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.' | '+')
        })
}

pub fn recipe_exists(recipe_root: &Path, recipe_name: &str) -> Result<bool> {
    let normalized = recipe_name.trim().to_ascii_lowercase();
    if !is_valid_recipe_name(&normalized) {
        return Ok(false);
    }
    let canonical_root = fs::canonicalize(recipe_root)
        .with_context(|| format!("resolving recipe root {}", recipe_root.display()))?;
    Ok(resolve_recipe_dir(&canonical_root, &normalized)?.is_some())
}

pub fn lookup_recipe_metadata(recipe_root: &Path, recipe_name: &str) -> Result<RecipeMetadata> {
    let normalized = recipe_name.trim().to_ascii_lowercase();
    if !is_valid_recipe_name(&normalized) {
        return Err(anyhow!("invalid recipe name"));
    }
    let canonical_root = fs::canonicalize(recipe_root)
        .with_context(|| format!("resolving recipe root {}", recipe_root.display()))?;
    let recipe_dir = resolve_recipe_dir(&canonical_root, &normalized)?
        .ok_or_else(|| anyhow!("recipe not found"))?;
    let variant_dir = select_recipe_variant_dir_within_root(&canonical_root, &recipe_dir)?;
    let meta_yaml_path = match canonical_metadata_path(&canonical_root, &variant_dir)? {
        Some(path) => path,
        None => canonical_metadata_path(&canonical_root, &recipe_dir)?
            .ok_or_else(|| anyhow!("missing meta.yaml/meta.yml"))?,
    };
    let raw_meta = fs::read_to_string(&meta_yaml_path)
        .with_context(|| format!("reading {}", meta_yaml_path.display()))?;
    let selector_ctx = SelectorContext::for_rpm_build(std::env::consts::ARCH);
    let selected_meta = apply_selectors(&raw_meta, &selector_ctx);
    let rendered_meta = render_meta_yaml(&selected_meta)
        .with_context(|| format!("rendering {}", meta_yaml_path.display()))?;
    let parsed = parse_rendered_meta(&rendered_meta)
        .with_context(|| format!("parsing {}", meta_yaml_path.display()))?;
    let (release_date, release_date_strategy) = infer_release_date(&meta_yaml_path);

    Ok(RecipeMetadata {
        recipe_name: normalized,
        meta_yaml_path,
        canonical_url: canonical_url_from_parsed_meta(&parsed),
        latest_release: non_empty_value(&parsed.version),
        description: non_empty_value(&parsed.summary)
            .unwrap_or_else(|| parsed.package_name.clone()),
        license_raw: non_empty_value(&parsed.license).unwrap_or_else(|| "NOASSERTION".to_string()),
        language: infer_language(&parsed),
        release_date,
        release_date_strategy,
    })
}

fn resolve_recipe_dir(recipe_root: &Path, recipe_name: &str) -> Result<Option<PathBuf>> {
    let direct = recipe_root.join(recipe_name);
    if let Some(canonical) = canonical_recipe_dir(recipe_root, &direct)? {
        return Ok(Some(canonical));
    }
    let normalized = recipe_name.to_ascii_lowercase();
    for entry in fs::read_dir(recipe_root)
        .with_context(|| format!("reading recipe root {}", recipe_root.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", recipe_root.display()))?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().to_ascii_lowercase() == normalized
            && let Some(canonical) = canonical_recipe_dir(recipe_root, &path)?
        {
            return Ok(Some(canonical));
        }
    }
    Ok(None)
}

fn canonical_recipe_dir(root: &Path, candidate: &Path) -> Result<Option<PathBuf>> {
    let metadata = match fs::metadata(candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting recipe directory {}", candidate.display()));
        }
    };
    if !metadata.is_dir() {
        return Ok(None);
    }

    let canonical = fs::canonicalize(candidate)
        .with_context(|| format!("resolving recipe directory {}", candidate.display()))?;
    if !canonical.starts_with(root) {
        return Err(anyhow!(
            "recipe directory escapes recipe root: {}",
            candidate.display()
        ));
    }
    Ok(Some(canonical))
}

fn canonical_metadata_path(root: &Path, directory: &Path) -> Result<Option<PathBuf>> {
    let Some(candidate) = meta_file_path(directory) else {
        return Ok(None);
    };
    let canonical = fs::canonicalize(&candidate)
        .with_context(|| format!("resolving recipe metadata {}", candidate.display()))?;
    if !canonical.starts_with(root) {
        return Err(anyhow!(
            "recipe metadata escapes recipe root: {}",
            candidate.display()
        ));
    }
    Ok(Some(canonical))
}

fn non_empty_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn canonical_url_from_parsed_meta(parsed: &crate::priority_specs::ParsedMeta) -> Option<String> {
    non_empty_value(&parsed.homepage).or_else(|| non_empty_value(&parsed.source_url))
}

fn infer_language(parsed: &crate::priority_specs::ParsedMeta) -> String {
    let deps = parsed
        .build_deps
        .iter()
        .chain(parsed.host_deps.iter())
        .chain(parsed.run_deps.iter())
        .map(|dep| dep.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let raw_specs = parsed
        .build_dep_specs_raw
        .iter()
        .chain(parsed.host_dep_specs_raw.iter())
        .chain(parsed.run_dep_specs_raw.iter())
        .map(|dep| dep.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let has_spec_prefix = |prefix: &str| {
        raw_specs.iter().any(|spec| {
            spec == prefix
                || spec
                    .split_whitespace()
                    .next()
                    .is_some_and(|head| head == prefix)
        })
    };

    if deps.iter().any(|dep| dep.starts_with("python"))
        || has_spec_prefix("python")
        || parsed.noarch_python
    {
        "Python".to_string()
    } else if deps.iter().any(|dep| dep.starts_with("perl")) || has_spec_prefix("perl") {
        "Perl".to_string()
    } else if deps
        .iter()
        .any(|dep| dep.starts_with("r-") || dep.starts_with("bioconductor-") || dep == "r-base")
        || has_spec_prefix("r-base")
    {
        "R".to_string()
    } else if deps.iter().any(|dep| dep.contains("rust")) {
        "Rust".to_string()
    } else if deps
        .iter()
        .any(|dep| dep == "go" || dep.starts_with("golang"))
    {
        "Go".to_string()
    } else if deps
        .iter()
        .any(|dep| dep.starts_with("openjdk") || dep.starts_with("java-"))
    {
        "Java".to_string()
    } else if parsed.source_url.contains("github.com") && parsed.package_name.contains("rust") {
        "Rust".to_string()
    } else {
        "Unknown".to_string()
    }
}

fn infer_release_date(meta_yaml_path: &Path) -> (String, String) {
    match fs::metadata(meta_yaml_path)
        .and_then(|metadata| metadata.modified())
        .map(DateTime::<Utc>::from)
    {
        Ok(ts) => (
            ts.date_naive().to_string(),
            "filesystem_modified".to_string(),
        ),
        Err(_) => (
            Utc::now().date_naive().to_string(),
            "current_utc_date".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_valid_recipe_name, lookup_recipe_metadata, recipe_exists};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn recipe_name_validation_accepts_bioconda_style_names() {
        assert!(is_valid_recipe_name("blast"));
        assert!(is_valid_recipe_name("perl-text-glob"));
        assert!(is_valid_recipe_name("r-foo.bar"));
        assert!(!is_valid_recipe_name("BLAST"));
        assert!(!is_valid_recipe_name("bad name"));
        assert!(!is_valid_recipe_name("."));
        assert!(!is_valid_recipe_name(".."));
        assert!(!is_valid_recipe_name(" .. "));
    }

    #[test]
    fn recipe_exists_checks_recipe_directory() {
        let temp = tempdir().expect("tempdir should create");
        let recipes_root = temp.path().join("recipes");
        fs::create_dir_all(recipes_root.join("blast")).expect("recipe dir should create");
        assert!(recipe_exists(&recipes_root, "blast").expect("existence should resolve"));
        assert!(!recipe_exists(&recipes_root, "missing").expect("existence should resolve"));
        assert!(!recipe_exists(&recipes_root, ".").expect("invalid names should be rejected"));
        assert!(!recipe_exists(&recipes_root, "..").expect("invalid names should be rejected"));
    }

    #[test]
    fn lookup_rejects_parent_directory_names_without_reading_parent_metadata() {
        let temp = tempdir().expect("tempdir should create");
        let recipes_root = temp.path().join("recipes");
        fs::create_dir_all(&recipes_root).expect("recipes root should create");
        fs::write(
            temp.path().join("meta.yaml"),
            "package:\n  name: escaped\n  version: \"1\"\n",
        )
        .expect("parent metadata should write");

        assert!(lookup_recipe_metadata(&recipes_root, ".").is_err());
        assert!(lookup_recipe_metadata(&recipes_root, "..").is_err());
    }

    #[test]
    fn lookup_recipe_metadata_renders_meta_and_infers_language() {
        let temp = tempdir().expect("tempdir should create");
        let recipes_root = temp.path().join("recipes");
        let recipe_dir = recipes_root.join("blast");
        fs::create_dir_all(&recipe_dir).expect("recipe dir should create");
        fs::write(
            recipe_dir.join("meta.yaml"),
            r#"
{% set version = "2.16.0" %}
package:
  name: blast
  version: {{ version }}
about:
  home: "https://ftp.ncbi.nlm.nih.gov/blast/executables/blast+/"
  license: "Public Domain"
  summary: "NCBI BLAST suite"
requirements:
  run:
    - python >=3.10
"#,
        )
        .expect("meta.yaml should write");

        let metadata =
            lookup_recipe_metadata(&recipes_root, "blast").expect("metadata should resolve");
        assert_eq!(metadata.recipe_name, "blast");
        assert_eq!(metadata.latest_release.as_deref(), Some("2.16.0"));
        assert_eq!(
            metadata.canonical_url.as_deref(),
            Some("https://ftp.ncbi.nlm.nih.gov/blast/executables/blast+/")
        );
        assert_eq!(metadata.description, "NCBI BLAST suite");
        assert_eq!(metadata.license_raw, "Public Domain");
        assert_eq!(metadata.language, "Python");
        assert!(!metadata.release_date.is_empty());
    }
}
