use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use glob::Pattern;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub software: String,
    pub version: String,
    pub arch: String,
    pub target_id: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub build_host: String,
    #[serde(default)]
    pub build_user: String,
    #[serde(default)]
    pub built_at: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub srpm_path: Option<String>,
    #[serde(default)]
    pub binary_rpms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureEntry {
    pub software: String,
    pub version: String,
    pub arch: String,
    pub target_id: String,
    pub status: String,
    pub reason: String,
    pub report_path: String,
    pub failed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default = "current_catalog_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub packages: Vec<CatalogPackage>,
    #[serde(default)]
    pub entries: Vec<CatalogEntry>,
    #[serde(default)]
    pub failures: Vec<FailureEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogPackage {
    pub software: String,
    #[serde(default)]
    pub versions: Vec<CatalogVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogVersion {
    pub version: String,
    #[serde(default)]
    pub authoritative_srpm: Option<SrpmArtifact>,
    #[serde(default)]
    pub builds: Vec<CatalogBuild>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SrpmArtifact {
    pub path: String,
    #[serde(default)]
    pub recorded_at: String,
    #[serde(default)]
    pub build_host: String,
    #[serde(default)]
    pub build_user: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogBuild {
    pub os: String,
    pub arch: String,
    pub target_id: String,
    pub build_host: String,
    pub build_user: String,
    pub built_at: String,
    pub status: String,
    #[serde(default)]
    pub srpm: Option<SrpmArtifact>,
    #[serde(default)]
    pub binary_rpms: Vec<String>,
    #[serde(default)]
    pub report_path: String,
}

const CATALOG_FILENAME: &str = ".catalog.json";
const CATALOG_SCHEMA_VERSION: u32 = 2;

fn current_catalog_schema_version() -> u32 {
    CATALOG_SCHEMA_VERSION
}

fn catalog_path(topdir: &Path) -> std::path::PathBuf {
    topdir.join(CATALOG_FILENAME)
}

fn read_catalog(topdir: &Path) -> Catalog {
    let path = catalog_path(topdir);
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Ok(mut cat) = serde_json::from_str::<Catalog>(&raw) {
            migrate_catalog(topdir, &path, &mut cat);
            return cat;
        }
    }
    Catalog::default()
}

fn write_catalog(topdir: &Path, catalog: &Catalog) -> Result<()> {
    let path = catalog_path(topdir);
    let tmp = path.with_extension("tmp");
    let mut normalized = catalog.clone();
    normalize_catalog(topdir, &path, &mut normalized);
    let payload = serde_json::to_string_pretty(&normalized).context("serializing catalogue")?;
    fs::write(&tmp, payload)
        .with_context(|| format!("writing catalogue tmp {}", tmp.to_string_lossy()))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("committing catalogue {}", path.to_string_lossy()))?;
    Ok(())
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            schema_version: CATALOG_SCHEMA_VERSION,
            packages: Vec::new(),
            entries: Vec::new(),
            failures: Vec::new(),
        }
    }
}

fn current_host_name() -> String {
    if let Ok(host) = std::env::var("HOSTNAME") {
        let host = host.trim();
        if !host.is_empty() {
            return host.to_string();
        }
    }
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                None
            }
        })
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

fn current_user_name() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|user| !user.trim().is_empty())
        .unwrap_or_else(|| "unknown-user".to_string())
}

fn file_modified_at_utc(path: &Path) -> String {
    let modified = path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let dt: DateTime<Utc> = modified.into();
    dt.to_rfc3339()
}

fn file_owner_name(path: &Path) -> String {
    #[cfg(unix)]
    {
        if let Ok(metadata) = path.metadata() {
            let uid = metadata.uid().to_string();
            if let Ok(output) = Command::new("id").args(["-nu", &uid]).output()
                && output.status.success()
            {
                let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !user.is_empty() {
                    return user;
                }
            }
            return uid;
        }
    }
    current_user_name()
}

fn infer_os_from_target_id(target_id: &str) -> String {
    let lower = target_id.to_ascii_lowercase();
    for prefix in [
        "almalinux-10.1",
        "almalinux-9.7",
        "almalinux-9.5",
        "fedora-43",
    ] {
        if lower.contains(prefix) {
            return prefix.to_string();
        }
    }
    lower
        .rsplit_once('-')
        .map(|(os, _)| os.to_string())
        .filter(|os| !os.is_empty())
        .unwrap_or_else(|| "unknown-os".to_string())
}

fn normalize_package_slug(input: &str) -> String {
    input
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn catalog_entry_basic(
    software: String,
    version: String,
    arch: String,
    target_id: String,
) -> CatalogEntry {
    let os = infer_os_from_target_id(&target_id);
    CatalogEntry {
        software,
        version,
        arch,
        target_id,
        os,
        build_host: current_host_name(),
        build_user: current_user_name(),
        built_at: Utc::now().to_rfc3339(),
        status: "generated".to_string(),
        srpm_path: None,
        binary_rpms: Vec::new(),
    }
}

fn srpm_artifact_from_path(path: &Path) -> SrpmArtifact {
    SrpmArtifact {
        path: path.display().to_string(),
        recorded_at: file_modified_at_utc(path),
        build_host: current_host_name(),
        build_user: file_owner_name(path),
    }
}

#[derive(Debug, Clone)]
struct SrpmCandidate {
    software: String,
    version: String,
    os: String,
    arch: String,
    target_id: String,
    path: PathBuf,
    artifact: SrpmArtifact,
}

fn srpm_target_id(path: &Path) -> Option<String> {
    let srpms = path.parent()?;
    if srpms.file_name().and_then(|v| v.to_str())? != "SRPMS" {
        return None;
    }
    srpms
        .parent()?
        .file_name()
        .and_then(|v| v.to_str())
        .map(|v| v.to_string())
}

fn parse_srpm_filename(path: &Path) -> Option<(String, String)> {
    let name = path.file_name()?.to_str()?.strip_suffix(".src.rpm")?;
    let (without_release, _) = name.rsplit_once('-')?;
    let (rpm_name, version) = without_release.rsplit_once('-')?;
    let software = rpm_name.strip_prefix("phoreus-").unwrap_or(rpm_name);
    if software.ends_with("-default") {
        return None;
    }
    Some((software.to_string(), version.to_string()))
}

fn query_srpm_identity(path: &Path) -> Option<(String, String)> {
    let output = Command::new("rpm")
        .args(["-qp", "--qf", "%{NAME}\t%{VERSION}\n"])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let mut fields = raw.trim().split('\t');
    let rpm_name = fields.next()?.trim();
    let version = fields.next()?.trim();
    if rpm_name.is_empty() || version.is_empty() {
        return None;
    }
    let software = rpm_name.strip_prefix("phoreus-").unwrap_or(rpm_name);
    if software.ends_with("-default") {
        return None;
    }
    Some((software.to_string(), version.to_string()))
}

fn srpm_candidate_from_path(path: PathBuf) -> Option<SrpmCandidate> {
    let target_id = srpm_target_id(&path)?;
    let (software, version) = query_srpm_identity(&path).or_else(|| parse_srpm_filename(&path))?;
    let arch = target_id.rsplit('-').next().unwrap_or("").to_string();
    Some(SrpmCandidate {
        software,
        version,
        os: infer_os_from_target_id(&target_id),
        arch,
        target_id,
        artifact: srpm_artifact_from_path(&path),
        path,
    })
}

fn scan_srpm_candidates(topdir: &Path) -> Vec<SrpmCandidate> {
    let targets = topdir.join("targets");
    if !targets.exists() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    let Ok(target_dirs) = fs::read_dir(targets) else {
        return candidates;
    };
    for target in target_dirs.flatten() {
        let srpms_dir = target.path().join("SRPMS");
        let Ok(entries) = fs::read_dir(srpms_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .and_then(|v| v.to_str())
                .map(|name| name.ends_with(".src.rpm"))
                .unwrap_or(false)
            {
                if let Some(candidate) = srpm_candidate_from_path(path) {
                    candidates.push(candidate);
                }
            }
        }
    }
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    candidates
}

fn best_srpm_for(topdir: &Path, software: &str, version: &str) -> Option<SrpmCandidate> {
    let software_key = normalize_package_slug(software);
    scan_srpm_candidates(topdir)
        .into_iter()
        .filter(|candidate| {
            normalize_package_slug(&candidate.software) == software_key
                && candidate.version == version
        })
        .max_by_key(|candidate| report_modified_at(&candidate.path))
}

pub fn authoritative_srpm_for(topdir: &Path, software: &str, version: &str) -> Option<PathBuf> {
    let mut catalog = read_catalog(topdir);
    inject_srpm_artifacts(topdir, &mut catalog);
    let software_key = normalize_package_slug(software);
    catalog
        .packages
        .iter()
        .find(|package| normalize_package_slug(&package.software) == software_key)
        .and_then(|package| {
            package
                .versions
                .iter()
                .find(|entry| entry.version == version)
        })
        .and_then(|entry| entry.authoritative_srpm.as_ref())
        .map(|artifact| PathBuf::from(&artifact.path))
        .filter(|path| path.exists())
        .or_else(|| best_srpm_for(topdir, software, version).map(|candidate| candidate.path))
}

fn inject_srpm_artifacts(topdir: &Path, catalog: &mut Catalog) -> usize {
    let mut injected = 0usize;
    for candidate in scan_srpm_candidates(topdir) {
        let version = package_version_mut(
            &mut catalog.packages,
            &candidate.software,
            &candidate.version,
        );
        if version.authoritative_srpm.as_ref() != Some(&candidate.artifact) {
            let replace = version
                .authoritative_srpm
                .as_ref()
                .map(|existing| {
                    report_modified_at(Path::new(&candidate.artifact.path))
                        >= report_modified_at(Path::new(&existing.path))
                })
                .unwrap_or(true);
            if replace {
                version.authoritative_srpm = Some(candidate.artifact.clone());
                injected += 1;
            }
        }
        merge_build(
            version,
            CatalogBuild {
                os: candidate.os,
                arch: candidate.arch,
                target_id: candidate.target_id,
                build_host: candidate.artifact.build_host.clone(),
                build_user: candidate.artifact.build_user.clone(),
                built_at: candidate.artifact.recorded_at.clone(),
                status: "srpm-prepared".to_string(),
                srpm: Some(candidate.artifact),
                binary_rpms: Vec::new(),
                report_path: String::new(),
            },
        );
    }
    injected
}

fn discover_binary_rpms(topdir: &Path, target_id: &str, software: &str) -> Vec<String> {
    let rpm_dir = topdir.join("targets").join(target_id).join("RPMS");
    if !rpm_dir.exists() {
        return Vec::new();
    }
    let slug = normalize_package_slug(software);
    let prefix = format!("phoreus-{slug}-");
    let mut out = Vec::new();
    let mut stack = vec![rpm_dir];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
                continue;
            };
            if name.ends_with(".rpm") && !name.ends_with(".src.rpm") && name.starts_with(&prefix) {
                out.push(path.display().to_string());
            }
        }
    }
    out.sort();
    out
}

fn newest_existing_artifact(paths: &[String]) -> Option<&Path> {
    paths
        .iter()
        .map(Path::new)
        .filter(|path| path.exists())
        .max_by_key(|path| report_modified_at(path))
}

fn build_key(build: &CatalogBuild) -> (String, String, String) {
    (
        build.os.clone(),
        build.arch.clone(),
        build.target_id.clone(),
    )
}

fn package_version_mut<'a>(
    packages: &'a mut Vec<CatalogPackage>,
    software: &str,
    version: &str,
) -> &'a mut CatalogVersion {
    let package_pos = packages
        .iter()
        .position(|pkg| pkg.software == software)
        .unwrap_or_else(|| {
            packages.push(CatalogPackage {
                software: software.to_string(),
                versions: Vec::new(),
            });
            packages.len() - 1
        });
    let versions = &mut packages[package_pos].versions;
    let version_pos = versions
        .iter()
        .position(|v| v.version == version)
        .unwrap_or_else(|| {
            versions.push(CatalogVersion {
                version: version.to_string(),
                authoritative_srpm: None,
                builds: Vec::new(),
            });
            versions.len() - 1
        });
    &mut versions[version_pos]
}

fn merge_build(version: &mut CatalogVersion, incoming: CatalogBuild) {
    let key = build_key(&incoming);
    if let Some(existing) = version
        .builds
        .iter_mut()
        .find(|build| build_key(build) == key)
    {
        if incoming.status != "unknown" {
            existing.status = incoming.status;
        }
        if !incoming.built_at.is_empty() {
            existing.built_at = incoming.built_at;
        }
        if !incoming.build_host.is_empty() {
            existing.build_host = incoming.build_host;
        }
        if !incoming.build_user.is_empty() {
            existing.build_user = incoming.build_user;
        }
        if incoming.srpm.is_some() {
            existing.srpm = incoming.srpm;
        }
        if !incoming.binary_rpms.is_empty() {
            existing.binary_rpms = incoming.binary_rpms;
        }
        if !incoming.report_path.is_empty() {
            existing.report_path = incoming.report_path;
        }
    } else {
        version.builds.push(incoming);
    }
}

fn flat_entries_from_packages(packages: &[CatalogPackage]) -> Vec<CatalogEntry> {
    let mut entries = Vec::new();
    for package in packages {
        for version in &package.versions {
            for build in &version.builds {
                entries.push(CatalogEntry {
                    software: package.software.clone(),
                    version: version.version.clone(),
                    arch: build.arch.clone(),
                    target_id: build.target_id.clone(),
                    os: build.os.clone(),
                    build_host: build.build_host.clone(),
                    build_user: build.build_user.clone(),
                    built_at: build.built_at.clone(),
                    status: build.status.clone(),
                    srpm_path: build.srpm.as_ref().map(|srpm| srpm.path.clone()),
                    binary_rpms: build.binary_rpms.clone(),
                });
            }
        }
    }
    entries.sort_by(|a, b| {
        a.software
            .cmp(&b.software)
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| a.os.cmp(&b.os))
            .then_with(|| a.arch.cmp(&b.arch))
            .then_with(|| a.target_id.cmp(&b.target_id))
    });
    entries
}

fn migrate_catalog(topdir: &Path, catalog_path: &Path, catalog: &mut Catalog) {
    normalize_catalog(topdir, catalog_path, catalog);
}

fn normalize_catalog(topdir: &Path, catalog_path: &Path, catalog: &mut Catalog) {
    let fallback_host = current_host_name();
    let fallback_user = file_owner_name(catalog_path);
    let fallback_time = file_modified_at_utc(catalog_path);
    if catalog.packages.is_empty() && !catalog.entries.is_empty() {
        for entry in catalog.entries.clone() {
            if entry.software.is_empty() || entry.version.is_empty() {
                continue;
            }
            let os = if entry.os.is_empty() {
                infer_os_from_target_id(&entry.target_id)
            } else {
                entry.os.clone()
            };
            let binary_rpms = if entry.binary_rpms.is_empty() {
                discover_binary_rpms(topdir, &entry.target_id, &entry.software)
            } else {
                entry.binary_rpms.clone()
            };
            let artifact_path = entry
                .srpm_path
                .as_deref()
                .map(Path::new)
                .filter(|path| path.exists())
                .or_else(|| newest_existing_artifact(&binary_rpms));
            let inferred_time = if entry.built_at.is_empty() {
                artifact_path
                    .map(file_modified_at_utc)
                    .unwrap_or_else(|| fallback_time.clone())
            } else {
                entry.built_at.clone()
            };
            let inferred_user = if entry.build_user.is_empty() {
                artifact_path
                    .map(file_owner_name)
                    .unwrap_or_else(|| fallback_user.clone())
            } else {
                entry.build_user.clone()
            };
            let srpm = entry.srpm_path.clone().map(|path| SrpmArtifact {
                path,
                recorded_at: inferred_time.clone(),
                build_host: if entry.build_host.is_empty() {
                    fallback_host.clone()
                } else {
                    entry.build_host.clone()
                },
                build_user: inferred_user.clone(),
            });
            let build = CatalogBuild {
                os,
                arch: entry.arch,
                target_id: entry.target_id,
                build_host: if entry.build_host.is_empty() {
                    fallback_host.clone()
                } else {
                    entry.build_host
                },
                build_user: inferred_user,
                built_at: inferred_time,
                status: if entry.status.is_empty() {
                    "generated".to_string()
                } else {
                    entry.status
                },
                srpm: srpm.clone(),
                binary_rpms,
                report_path: String::new(),
            };
            let version =
                package_version_mut(&mut catalog.packages, &entry.software, &entry.version);
            if version.authoritative_srpm.is_none() {
                version.authoritative_srpm = srpm;
            }
            merge_build(version, build);
        }
    }
    for package in &mut catalog.packages {
        for version in &mut package.versions {
            if version.authoritative_srpm.is_none() {
                version.authoritative_srpm = version
                    .builds
                    .iter()
                    .filter_map(|build| build.srpm.clone())
                    .next();
            }
        }
    }
    catalog.schema_version = CATALOG_SCHEMA_VERSION;
    catalog.entries = flat_entries_from_packages(&catalog.packages);
    let _ = topdir;
}

/// Collect all JSON report files from the reports directory.
fn collect_report_paths(reports_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if !reports_dir.exists() {
        return paths;
    }
    let glob_pattern = reports_dir.join("*.json");
    for entry in glob::glob(&glob_pattern.to_string_lossy())
        .ok()
        .into_iter()
        .flatten()
    {
        if let Ok(entry) = entry {
            if entry.is_file() {
                paths.push(entry);
            }
        }
    }
    paths.sort_by(|a, b| {
        report_modified_at(a)
            .cmp(&report_modified_at(b))
            .then_with(|| a.cmp(b))
    });
    paths
}

fn report_modified_at(path: &Path) -> SystemTime {
    path.metadata()
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn report_modified_at_utc(path: &Path) -> String {
    let dt: DateTime<Utc> = report_modified_at(path).into();
    dt.to_rfc3339()
}

/// Scan a single report JSON file for generated entries.
fn scan_report_file(path: &Path, target_id: &str, arch: &str) -> Vec<CatalogEntry> {
    let Ok(payload) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(items): Result<Vec<serde_json::Value>, _> = serde_json::from_str(&payload) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for item in items {
        let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status != "generated" && status != "up-to-date" {
            continue;
        }
        let software = item
            .get("software")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let version = item
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !software.is_empty() && !version.is_empty() {
            entries.push(CatalogEntry {
                built_at: report_modified_at_utc(path),
                status: status.to_string(),
                ..catalog_entry_basic(software, version, arch.to_string(), target_id.to_string())
            });
        }
    }
    entries
}

fn scan_build_reports(topdir: &Path) -> Vec<CatalogEntry> {
    let targets = topdir.join("targets");
    if !targets.exists() {
        return Vec::new();
    }

    let mut seen: BTreeSet<(String, String, String, String)> = BTreeSet::new();

    let target_dirs: Vec<_> = fs::read_dir(&targets)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    for entry in target_dirs {
        let target_id = entry.file_name().to_string_lossy().into_owned();
        let reports_dir = entry.path().join("reports");

        // Derive arch from target_id (last component after last dash)
        let arch = target_id.rsplit('-').next().unwrap_or("").to_string();

        for report_path in collect_report_paths(&reports_dir) {
            for e in scan_report_file(&report_path, &target_id, &arch) {
                seen.insert((e.software, e.version, e.arch, e.target_id));
            }
        }
    }

    seen.into_iter()
        .map(|(software, version, arch, target_id)| CatalogEntry {
            ..catalog_entry_basic(software, version, arch, target_id)
        })
        .collect()
}

fn failure_key(software: &str, target_id: &str) -> (String, String) {
    (software.to_string(), target_id.to_string())
}

fn is_failure_status(status: &str) -> bool {
    matches!(status, "quarantined" | "failed")
}

fn is_success_status(status: &str) -> bool {
    matches!(status, "generated" | "up-to-date")
}

fn is_direct_build_failure(status: &str, reason: &str) -> bool {
    if !is_failure_status(status) {
        return false;
    }
    let lower = reason.to_ascii_lowercase();
    !lower.contains("blocked by failed dependencies")
        && !lower.contains("no overlapping recipe found")
        && !lower.contains("arch_policy=amd64_only")
        && !lower.contains("arch_policy=aarch64_only")
        && !lower.contains("build cancelled")
}

fn scan_report_file_for_failures(
    path: &Path,
    target_id: &str,
    arch: &str,
    failures: &mut BTreeMap<(String, String), FailureEntry>,
) {
    let Ok(payload) = fs::read_to_string(path) else {
        return;
    };
    let Ok(items): Result<Vec<serde_json::Value>, _> = serde_json::from_str(&payload) else {
        return;
    };
    let failed_at = report_modified_at_utc(path);
    let report_path = path.display().to_string();

    for item in items {
        let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let software = item.get("software").and_then(|v| v.as_str()).unwrap_or("");
        if software.is_empty() {
            continue;
        }
        let key = failure_key(software, target_id);
        if is_success_status(status) {
            failures.remove(&key);
            continue;
        }

        let reason = item
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !is_direct_build_failure(status, &reason) {
            continue;
        }
        let version = item
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        failures.insert(
            key,
            FailureEntry {
                software: software.to_string(),
                version,
                arch: arch.to_string(),
                target_id: target_id.to_string(),
                status: status.to_string(),
                reason,
                report_path: report_path.clone(),
                failed_at: failed_at.clone(),
            },
        );
    }
}

fn scan_build_failures(topdir: &Path) -> Vec<FailureEntry> {
    let targets = topdir.join("targets");
    if !targets.exists() {
        return Vec::new();
    }

    let mut failures: BTreeMap<(String, String), FailureEntry> = BTreeMap::new();

    let mut target_dirs: Vec<_> = fs::read_dir(&targets)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    target_dirs.sort_by_key(|e| e.file_name());

    for entry in target_dirs {
        let target_id = entry.file_name().to_string_lossy().into_owned();
        let reports_dir = entry.path().join("reports");
        let arch = target_id.rsplit('-').next().unwrap_or("").to_string();
        for report_path in collect_report_paths(&reports_dir) {
            scan_report_file_for_failures(&report_path, &target_id, &arch, &mut failures);
        }
    }

    let mut failures: Vec<_> = failures.into_values().collect();
    failures.sort_by(|a, b| {
        a.failed_at
            .cmp(&b.failed_at)
            .then_with(|| a.software.cmp(&b.software))
            .then_with(|| a.target_id.cmp(&b.target_id))
    });
    failures
}

fn update_catalog(topdir: &Path) -> Catalog {
    let mut existing = read_catalog(topdir);
    let scanned = scan_build_reports(topdir);
    let failures = scan_build_failures(topdir);

    for e in &scanned {
        let version = package_version_mut(&mut existing.packages, &e.software, &e.version);
        merge_build(
            version,
            CatalogBuild {
                os: e.os.clone(),
                arch: e.arch.clone(),
                target_id: e.target_id.clone(),
                build_host: e.build_host.clone(),
                build_user: e.build_user.clone(),
                built_at: e.built_at.clone(),
                status: e.status.clone(),
                srpm: e.srpm_path.as_ref().map(|path| SrpmArtifact {
                    path: path.clone(),
                    recorded_at: e.built_at.clone(),
                    build_host: e.build_host.clone(),
                    build_user: e.build_user.clone(),
                }),
                binary_rpms: discover_binary_rpms(topdir, &e.target_id, &e.software),
                report_path: String::new(),
            },
        );
    }

    existing.failures = failures;
    inject_srpm_artifacts(topdir, &mut existing);
    normalize_catalog(topdir, &catalog_path(topdir), &mut existing);
    let catalog = existing;
    let _ = write_catalog(topdir, &catalog);
    catalog
}

pub fn record_build_results(
    topdir: &Path,
    entries: &[crate::priority_specs::ReportEntry],
    arch: &str,
    target_id: &str,
    report_path: &Path,
) -> Result<()> {
    let mut existing = read_catalog(topdir);
    let mut failures: BTreeMap<(String, String), FailureEntry> = existing
        .failures
        .into_iter()
        .map(|entry| (failure_key(&entry.software, &entry.target_id), entry))
        .collect();
    let failed_at = Utc::now().to_rfc3339();
    let report_path = report_path.display().to_string();

    for entry in entries {
        let key = failure_key(&entry.software, target_id);
        let srpm = if entry.version.is_empty() {
            None
        } else {
            best_srpm_for(topdir, &entry.software, &entry.version)
        };
        if is_success_status(&entry.status) || srpm.is_some() {
            let binary_rpms = if is_success_status(&entry.status) {
                discover_binary_rpms(topdir, target_id, &entry.software)
            } else {
                Vec::new()
            };
            let status = if is_success_status(&entry.status) {
                entry.status.clone()
            } else {
                "srpm-prepared".to_string()
            };
            let artifact = srpm.as_ref().map(|candidate| candidate.artifact.clone());
            let version =
                package_version_mut(&mut existing.packages, &entry.software, &entry.version);
            if version.authoritative_srpm.is_none() && artifact.is_some() {
                version.authoritative_srpm = artifact.clone();
            }
            merge_build(
                version,
                CatalogBuild {
                    os: infer_os_from_target_id(target_id),
                    arch: arch.to_string(),
                    target_id: target_id.to_string(),
                    build_host: current_host_name(),
                    build_user: current_user_name(),
                    built_at: failed_at.clone(),
                    status,
                    srpm: artifact,
                    binary_rpms,
                    report_path: report_path.clone(),
                },
            );
        }
        if is_success_status(&entry.status) {
            failures.remove(&key);
            continue;
        }
        if is_direct_build_failure(&entry.status, &entry.reason) {
            failures.insert(
                key,
                FailureEntry {
                    software: entry.software.clone(),
                    version: entry.version.clone(),
                    arch: arch.to_string(),
                    target_id: target_id.to_string(),
                    status: entry.status.clone(),
                    reason: entry.reason.clone(),
                    report_path: report_path.clone(),
                    failed_at: failed_at.clone(),
                },
            );
        }
    }

    let mut failures: Vec<_> = failures.into_values().collect();
    failures.sort_by(|a, b| {
        a.failed_at
            .cmp(&b.failed_at)
            .then_with(|| a.software.cmp(&b.software))
            .then_with(|| a.target_id.cmp(&b.target_id))
    });

    existing.failures = failures;
    inject_srpm_artifacts(topdir, &mut existing);
    write_catalog(topdir, &existing)?;
    Ok(())
}

#[cfg(test)]
fn add_entries_to_catalogue(topdir: &Path, entries: &[CatalogEntry]) -> Result<()> {
    let mut existing = read_catalog(topdir);
    for e in entries {
        let version = package_version_mut(&mut existing.packages, &e.software, &e.version);
        merge_build(
            version,
            CatalogBuild {
                os: e.os.clone(),
                arch: e.arch.clone(),
                target_id: e.target_id.clone(),
                build_host: e.build_host.clone(),
                build_user: e.build_user.clone(),
                built_at: e.built_at.clone(),
                status: if e.status.is_empty() {
                    "generated".to_string()
                } else {
                    e.status.clone()
                },
                srpm: e.srpm_path.as_ref().map(|path| SrpmArtifact {
                    path: path.clone(),
                    recorded_at: e.built_at.clone(),
                    build_host: e.build_host.clone(),
                    build_user: e.build_user.clone(),
                }),
                binary_rpms: e.binary_rpms.clone(),
                report_path: String::new(),
            },
        );
    }
    write_catalog(topdir, &existing)?;
    Ok(())
}

fn filter_entries<'a>(
    entries: &'a [CatalogEntry],
    name_pat: Option<&Pattern>,
    version_pat: Option<&Pattern>,
    arch: Option<&str>,
) -> Vec<&'a CatalogEntry> {
    entries
        .iter()
        .filter(|e| {
            if let Some(pat) = name_pat {
                if !pat.matches(&e.software) {
                    return false;
                }
            }
            if let Some(pat) = version_pat {
                if !pat.matches(&e.version) {
                    return false;
                }
            }
            if let Some(expected) = arch {
                if e.arch != expected {
                    return false;
                }
            }
            true
        })
        .collect()
}

fn filter_failures<'a>(
    failures: &'a [FailureEntry],
    name_pat: Option<&Pattern>,
    version_pat: Option<&Pattern>,
    arch: Option<&str>,
) -> Vec<&'a FailureEntry> {
    failures
        .iter()
        .filter(|e| {
            if let Some(pat) = name_pat {
                if !pat.matches(&e.software) {
                    return false;
                }
            }
            if let Some(pat) = version_pat {
                if !pat.matches(&e.version) {
                    return false;
                }
            }
            if let Some(expected) = arch {
                if e.arch != expected {
                    return false;
                }
            }
            true
        })
        .collect()
}

fn render_text(entries: &[&CatalogEntry], cat_path: &Path) -> String {
    if entries.is_empty() {
        return format!(
            "no matching entries in catalogue (catalogue: {})\n",
            cat_path.display()
        );
    }
    let mut out = format!(
        "catalogue entries: {}\ncatalogue: {}\n\n",
        entries.len(),
        cat_path.display()
    );
    out.push_str(
        "| Software | Version | OS | Arch | Status | Built At | Host | User | SRPM | Target ID |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|\n");
    for e in entries {
        let srpm = e.srpm_path.as_deref().unwrap_or("");
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            e.software.replace('|', "\\|"),
            e.version.replace('|', "\\|"),
            e.os.replace('|', "\\|"),
            e.arch.replace('|', "\\|"),
            e.status.replace('|', "\\|"),
            e.built_at.replace('|', "\\|"),
            e.build_host.replace('|', "\\|"),
            e.build_user.replace('|', "\\|"),
            srpm.replace('|', "\\|"),
            e.target_id.replace('|', "\\|")
        ));
    }
    out
}

fn render_failures_text(entries: &[&FailureEntry], cat_path: &Path) -> String {
    if entries.is_empty() {
        return format!(
            "no current build failures in catalogue (catalogue: {})\n",
            cat_path.display()
        );
    }
    let mut out = format!(
        "current build failures: {}\ncatalogue: {}\n\n",
        entries.len(),
        cat_path.display()
    );
    out.push_str("| Failed At | Software | Version | Arch | Target ID | Reason |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for e in entries {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            e.failed_at,
            e.software,
            e.version,
            e.arch,
            e.target_id,
            e.reason.replace('|', "\\|")
        ));
    }
    out
}

pub fn run_list(topdir: &Path, args: &crate::cli::ListArgs) -> Result<()> {
    let cat_path = catalog_path(topdir);

    let catalog = if args.refresh {
        update_catalog(topdir)
    } else {
        read_catalog(topdir)
    };

    let name_pat = args
        .name
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .with_context(|| "invalid --name glob pattern")?;
    let version_pat = args
        .version
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .with_context(|| "invalid --version glob pattern")?;

    let filtered = filter_entries(
        &catalog.entries,
        name_pat.as_ref(),
        version_pat.as_ref(),
        args.arch.as_deref(),
    );

    if args.json {
        let json =
            serde_json::to_string_pretty(&filtered).context("serializing catalogue entries")?;
        println!("{json}");
    } else {
        print!("{}", render_text(&filtered, &cat_path));
    }

    Ok(())
}

pub fn run_failures(topdir: &Path, args: &crate::cli::FailuresArgs) -> Result<()> {
    let cat_path = catalog_path(topdir);

    let catalog = if args.refresh {
        update_catalog(topdir)
    } else {
        read_catalog(topdir)
    };

    let name_pat = args
        .name
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .with_context(|| "invalid --name glob pattern")?;
    let version_pat = args
        .version
        .as_deref()
        .map(Pattern::new)
        .transpose()
        .with_context(|| "invalid --version glob pattern")?;

    let filtered = filter_failures(
        &catalog.failures,
        name_pat.as_ref(),
        version_pat.as_ref(),
        args.arch.as_deref(),
    );

    if args.json {
        let json =
            serde_json::to_string_pretty(&filtered).context("serializing failure entries")?;
        println!("{json}");
    } else {
        print!("{}", render_failures_text(&filtered, &cat_path));
    }

    Ok(())
}

#[derive(Debug, Serialize)]
pub struct CatalogMigrationSummary {
    pub catalog_path: String,
    pub schema_version: u32,
    pub packages: usize,
    pub versions: usize,
    pub builds: usize,
    pub authoritative_srpms: usize,
}

fn catalog_summary(topdir: &Path, catalog: &Catalog) -> CatalogMigrationSummary {
    let versions = catalog
        .packages
        .iter()
        .map(|package| package.versions.len())
        .sum();
    let builds = catalog
        .packages
        .iter()
        .flat_map(|package| &package.versions)
        .map(|version| version.builds.len())
        .sum();
    let authoritative_srpms = catalog
        .packages
        .iter()
        .flat_map(|package| &package.versions)
        .filter(|version| version.authoritative_srpm.is_some())
        .count();
    CatalogMigrationSummary {
        catalog_path: catalog_path(topdir).display().to_string(),
        schema_version: catalog.schema_version,
        packages: catalog.packages.len(),
        versions,
        builds,
        authoritative_srpms,
    }
}

pub fn run_catalog_migrate(topdir: &Path, json: bool) -> Result<()> {
    let mut catalog = update_catalog(topdir);
    inject_srpm_artifacts(topdir, &mut catalog);
    write_catalog(topdir, &catalog)?;
    let summary = catalog_summary(topdir, &read_catalog(topdir));
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).context("serializing catalog migration")?
        );
    } else {
        println!(
            "catalog migrated: path={} schema={} packages={} versions={} builds={} authoritative_srpms={}",
            summary.catalog_path,
            summary.schema_version,
            summary.packages,
            summary.versions,
            summary.builds,
            summary.authoritative_srpms
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog_entry(software: &str, version: &str, arch: &str, target_id: &str) -> CatalogEntry {
        catalog_entry_basic(
            software.to_string(),
            version.to_string(),
            arch.to_string(),
            target_id.to_string(),
        )
    }

    #[test]
    fn filter_by_name_returns_matches() {
        let entries = vec![
            catalog_entry("samtools", "1.18", "x86_64", "tgt-x86"),
            catalog_entry("samtools", "1.19", "aarch64", "tgt-aarch"),
            catalog_entry("fastqc", "0.12.1", "x86_64", "tgt-x86"),
        ];
        let pat = Pattern::new("sam*").unwrap();
        let filtered = filter_entries(&entries, Some(&pat), None, None);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].software, "samtools");
        assert_eq!(filtered[1].software, "samtools");
    }

    #[test]
    fn filter_by_arch_returns_matches() {
        let entries = vec![
            catalog_entry("samtools", "1.18", "x86_64", "tgt-x86"),
            catalog_entry("samtools", "1.19", "aarch64", "tgt-aarch"),
        ];
        let filtered = filter_entries(&entries, None, None, Some("aarch64"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].arch, "aarch64");
    }

    #[test]
    fn filter_no_matches_returns_empty() {
        let entries = vec![catalog_entry("samtools", "1.18", "x86_64", "tgt-x86")];
        let filtered = filter_entries(&entries, Some(&Pattern::new("fastq*").unwrap()), None, None);
        assert!(filtered.is_empty());
    }

    #[test]
    fn add_entries_to_catalogue_merges_with_existing() {
        let tmp = std::env::temp_dir().join(format!(
            "bioconda2rpm-catalog-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&tmp).expect("create temp dir");

        // Seed existing entries
        let existing = Catalog {
            entries: vec![
                catalog_entry("samtools", "1.18", "x86_64", "tgt-x86"),
                catalog_entry("fastqc", "0.12.1", "x86_64", "tgt-x86"),
            ],
            failures: Vec::new(),
            ..Catalog::default()
        };
        write_catalog(&tmp, &existing).expect("write initial catalog");

        // Add new entries
        let new_entries = vec![
            catalog_entry("abyss", "2.3.10", "aarch64", "tgt-aarch"),
            catalog_entry("samtools", "1.18", "aarch64", "tgt-aarch"), // same software, new arch
        ];
        add_entries_to_catalogue(&tmp, &new_entries).expect("add entries");

        let loaded = read_catalog(&tmp);
        assert_eq!(loaded.entries.len(), 4); // samtools x86, samtools aarch, fastqc, abyss
        let software_names: BTreeSet<_> =
            loaded.entries.iter().map(|e| e.software.as_str()).collect();
        assert!(software_names.contains("samtools"));
        assert!(software_names.contains("fastqc"));
        assert!(software_names.contains("abyss"));

        let samtools_entries: Vec<_> = loaded
            .entries
            .iter()
            .filter(|e| e.software == "samtools")
            .collect();
        assert_eq!(samtools_entries.len(), 2);
        let arches: BTreeSet<_> = samtools_entries.iter().map(|e| e.arch.as_str()).collect();
        assert!(arches.contains("x86_64"));
        assert!(arches.contains("aarch64"));

        let _ = fs::remove_dir_all(&tmp);
    }

    fn report_entry(
        software: &str,
        status: &str,
        reason: &str,
    ) -> crate::priority_specs::ReportEntry {
        crate::priority_specs::ReportEntry {
            software: software.into(),
            priority: 0,
            status: status.into(),
            reason: reason.into(),
            overlap_recipe: software.into(),
            overlap_reason: "test".into(),
            variant_dir: String::new(),
            package_name: software.into(),
            version: "1.0".into(),
            payload_spec_path: String::new(),
            meta_spec_path: String::new(),
            staged_build_sh: String::new(),
        }
    }

    #[test]
    fn record_build_results_tracks_and_clears_failures() {
        let tmp = std::env::temp_dir().join(format!(
            "bioconda2rpm-failures-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&tmp).expect("create temp dir");
        let report_path = tmp.join("reports").join("build_test.json");

        record_build_results(
            &tmp,
            &[report_entry(
                "tbl2asn-forever",
                "quarantined",
                "payload spec build failed in container",
            )],
            "aarch64",
            "target-aarch64",
            &report_path,
        )
        .expect("record failure");
        let loaded = read_catalog(&tmp);
        assert_eq!(loaded.failures.len(), 1);
        assert_eq!(loaded.failures[0].software, "tbl2asn-forever");

        record_build_results(
            &tmp,
            &[report_entry(
                "tbl2asn-forever",
                "generated",
                "spec generated",
            )],
            "aarch64",
            "target-aarch64",
            &report_path,
        )
        .expect("record success");
        let loaded = read_catalog(&tmp);
        assert!(loaded.failures.is_empty());
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].software, "tbl2asn-forever");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn catalog_migrate_injects_discovered_srpm() {
        let tmp = std::env::temp_dir().join(format!(
            "bioconda2rpm-srpm-migrate-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let target_id = "phoreus-bioconda2rpm-build-almalinux-9.7-x86_64";
        let srpms = tmp.join("targets").join(target_id).join("SRPMS");
        fs::create_dir_all(&srpms).expect("create srpms dir");
        let srpm = srpms.join("phoreus-samtools-1.18-1.src.rpm");
        fs::write(&srpm, b"placeholder srpm").expect("write srpm placeholder");

        let old_catalog = Catalog {
            entries: vec![catalog_entry("samtools", "1.18", "x86_64", target_id)],
            failures: Vec::new(),
            ..Catalog::default()
        };
        write_catalog(&tmp, &old_catalog).expect("write old catalog");

        run_catalog_migrate(&tmp, true).expect("migrate catalog");
        let loaded = read_catalog(&tmp);
        assert_eq!(loaded.schema_version, CATALOG_SCHEMA_VERSION);
        assert_eq!(loaded.packages.len(), 1);
        let version = &loaded.packages[0].versions[0];
        assert!(version.authoritative_srpm.is_some());
        assert!(
            version
                .authoritative_srpm
                .as_ref()
                .unwrap()
                .path
                .ends_with("phoreus-samtools-1.18-1.src.rpm")
        );
        assert!(loaded.entries.iter().any(|entry| {
            entry.software == "samtools"
                && entry.version == "1.18"
                && entry
                    .srpm_path
                    .as_deref()
                    .unwrap_or("")
                    .ends_with(".src.rpm")
        }));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn blocked_dependencies_are_not_direct_failures() {
        assert!(!is_direct_build_failure(
            "quarantined",
            "blocked by failed dependencies: aragorn"
        ));
        assert!(!is_direct_build_failure(
            "quarantined",
            "no overlapping recipe found in bioconda metadata"
        ));
        assert!(is_direct_build_failure(
            "quarantined",
            "payload spec build failed in container"
        ));
    }
}
