use anyhow::{Context, Result};
use glob::Pattern;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub software: String,
    pub version: String,
    pub arch: String,
    pub target_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
}

const CATALOG_FILENAME: &str = ".catalog.json";

fn catalog_path(topdir: &Path) -> std::path::PathBuf {
    topdir.join(CATALOG_FILENAME)
}

fn read_catalog(topdir: &Path) -> Catalog {
    let path = catalog_path(topdir);
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Ok(cat) = serde_json::from_str::<Catalog>(&raw) {
            return cat;
        }
    }
    Catalog {
        entries: Vec::new(),
    }
}

fn write_catalog(topdir: &Path, catalog: &Catalog) -> Result<()> {
    let path = catalog_path(topdir);
    let tmp = path.with_extension("tmp");
    let payload = serde_json::to_string_pretty(catalog).context("serializing catalogue")?;
    fs::write(&tmp, payload)
        .with_context(|| format!("writing catalogue tmp {}", tmp.to_string_lossy()))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("committing catalogue {}", path.to_string_lossy()))?;
    Ok(())
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
    paths.sort();
    paths
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
                software,
                version,
                arch: arch.to_string(),
                target_id: target_id.to_string(),
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
            software,
            version,
            arch,
            target_id,
        })
        .collect()
}

fn update_catalog(topdir: &Path) -> Catalog {
    let existing = read_catalog(topdir);
    let scanned = scan_build_reports(topdir);

    // Build a set of (software, version, arch) -> target_ids for efficient merge
    let mut merged: BTreeMap<(String, String, String), BTreeSet<String>> = BTreeMap::new();

    // Start from existing entries
    for e in &existing.entries {
        let key = (e.software.clone(), e.version.clone(), e.arch.clone());
        merged.entry(key).or_default().insert(e.target_id.clone());
    }

    // Merge scanned entries
    for e in &scanned {
        let key = (e.software.clone(), e.version.clone(), e.arch.clone());
        merged.entry(key).or_default().insert(e.target_id.clone());
    }

    let entries: Vec<CatalogEntry> = merged
        .into_iter()
        .map(|(key, target_ids)| -> CatalogEntry {
            let (software, version, arch) = key;
            let target_id = target_ids.iter().next().cloned().unwrap_or_default();
            CatalogEntry {
                software,
                version,
                arch,
                target_id,
            }
        })
        .collect();

    let catalog = Catalog { entries };
    let _ = write_catalog(topdir, &catalog);
    catalog
}

pub fn add_entries_to_catalogue(topdir: &Path, entries: &[CatalogEntry]) -> Result<()> {
    let existing = read_catalog(topdir);
    let mut merged: BTreeMap<(String, String, String), BTreeSet<String>> = BTreeMap::new();

    // Start from existing entries
    for e in &existing.entries {
        let key = (e.software.clone(), e.version.clone(), e.arch.clone());
        merged.entry(key).or_default().insert(e.target_id.clone());
    }

    // Merge new entries
    for e in entries {
        let key = (e.software.clone(), e.version.clone(), e.arch.clone());
        merged.entry(key).or_default().insert(e.target_id.clone());
    }

    let entries: Vec<CatalogEntry> = merged
        .into_iter()
        .map(|(key, target_ids)| -> CatalogEntry {
            let (software, version, arch) = key;
            let target_id = target_ids.iter().next().cloned().unwrap_or_default();
            CatalogEntry {
                software,
                version,
                arch,
                target_id,
            }
        })
        .collect();

    let catalog = Catalog { entries };
    write_catalog(topdir, &catalog)?;
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
    out.push_str("| Software | Version | Arch | Target ID |\n");
    out.push_str("|---|---|---|---|\n");
    for e in entries {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            e.software, e.version, e.arch, e.target_id
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_by_name_returns_matches() {
        let entries = vec![
            CatalogEntry {
                software: "samtools".into(),
                version: "1.18".into(),
                arch: "x86_64".into(),
                target_id: "tgt-x86".into(),
            },
            CatalogEntry {
                software: "samtools".into(),
                version: "1.19".into(),
                arch: "aarch64".into(),
                target_id: "tgt-aarch".into(),
            },
            CatalogEntry {
                software: "fastqc".into(),
                version: "0.12.1".into(),
                arch: "x86_64".into(),
                target_id: "tgt-x86".into(),
            },
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
            CatalogEntry {
                software: "samtools".into(),
                version: "1.18".into(),
                arch: "x86_64".into(),
                target_id: "tgt-x86".into(),
            },
            CatalogEntry {
                software: "samtools".into(),
                version: "1.19".into(),
                arch: "aarch64".into(),
                target_id: "tgt-aarch".into(),
            },
        ];
        let filtered = filter_entries(&entries, None, None, Some("aarch64"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].arch, "aarch64");
    }

    #[test]
    fn filter_no_matches_returns_empty() {
        let entries = vec![CatalogEntry {
            software: "samtools".into(),
            version: "1.18".into(),
            arch: "x86_64".into(),
            target_id: "tgt-x86".into(),
        }];
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
                CatalogEntry {
                    software: "samtools".into(),
                    version: "1.18".into(),
                    arch: "x86_64".into(),
                    target_id: "tgt-x86".into(),
                },
                CatalogEntry {
                    software: "fastqc".into(),
                    version: "0.12.1".into(),
                    arch: "x86_64".into(),
                    target_id: "tgt-x86".into(),
                },
            ],
        };
        write_catalog(&tmp, &existing).expect("write initial catalog");

        // Add new entries
        let new_entries = vec![
            CatalogEntry {
                software: "abyss".into(),
                version: "2.3.10".into(),
                arch: "aarch64".into(),
                target_id: "tgt-aarch".into(),
            },
            CatalogEntry {
                software: "samtools".into(), // same software, new arch
                version: "1.18".into(),
                arch: "aarch64".into(),
                target_id: "tgt-aarch".into(),
            },
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
}
