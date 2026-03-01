use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const LOCK_FILE_NAME: &str = ".bioconda2rpm-artifacts.lock";
const STATE_FILE_NAME: &str = ".bioconda2rpm-active-builds.json";
const REQUESTS_FILE_NAME: &str = ".bioconda2rpm-build-requests.jsonl";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSessionKind {
    Build,
    GeneratePrioritySpecs,
    Regression,
}

impl BuildSessionKind {
    fn as_str(self) -> &'static str {
        match self {
            BuildSessionKind::Build => "build",
            BuildSessionKind::GeneratePrioritySpecs => "generate-priority-specs",
            BuildSessionKind::Regression => "regression",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ForwardedBuildRequest {
    pub owner_pid: u32,
    pub owner_target_id: String,
    pub owner_force_rebuild: bool,
    pub queued_packages: Vec<String>,
}

pub enum BuildAcquireOutcome {
    Owner(BuildSessionGuard),
    Forwarded(ForwardedBuildRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActiveBuildEntry {
    pid: u32,
    target_id: String,
    packages: Vec<String>,
    #[serde(default = "default_session_kind")]
    session_kind: String,
    #[serde(default)]
    force_rebuild: bool,
    started_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ActiveBuildState {
    entries: Vec<ActiveBuildEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildQueueRequest {
    pid: u32,
    target_id: String,
    packages: Vec<String>,
    submitted_at_utc: String,
}

pub struct BuildSessionGuard {
    lock_file: fs::File,
    state_file: PathBuf,
    requests_file: PathBuf,
    pid: u32,
    session_kind: BuildSessionKind,
}

fn default_session_kind() -> String {
    "build".to_string()
}

impl BuildSessionGuard {
    pub fn acquire(
        topdir: &Path,
        target_id: &str,
        packages: &[String],
        session_kind: BuildSessionKind,
        force_rebuild: bool,
    ) -> Result<Self> {
        fs::create_dir_all(topdir)
            .with_context(|| format!("creating topdir {}", topdir.to_string_lossy()))?;

        let lock_path = topdir.join(LOCK_FILE_NAME);
        let state_file = topdir.join(STATE_FILE_NAME);
        let requests_file = topdir.join(REQUESTS_FILE_NAME);
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening lock file {}", lock_path.to_string_lossy()))?;

        if let Err(err) = lock_file.try_lock_exclusive() {
            if err.kind() == ErrorKind::WouldBlock {
                let active = load_state(&state_file).unwrap_or_default();
                let owner = active
                    .entries
                    .first()
                    .map(|entry| {
                        format!(
                            "pid={} target={} kind={} force={} packages={}",
                            entry.pid,
                            entry.target_id,
                            entry.session_kind,
                            entry.force_rebuild,
                            entry.packages.join(",")
                        )
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                bail!(
                    "workspace is already in use: {} (state file: {})",
                    owner,
                    state_file.to_string_lossy()
                );
            }
            return Err(err).with_context(|| {
                format!("acquiring workspace lock {}", lock_path.to_string_lossy())
            });
        }
        Self::initialize_locked_session(
            lock_file,
            lock_path.as_path(),
            state_file,
            requests_file,
            target_id,
            packages,
            session_kind,
            force_rebuild,
        )
    }

    pub fn acquire_or_forward_build(
        topdir: &Path,
        target_id: &str,
        packages: &[String],
        force_rebuild: bool,
    ) -> Result<BuildAcquireOutcome> {
        fs::create_dir_all(topdir)
            .with_context(|| format!("creating topdir {}", topdir.to_string_lossy()))?;
        let lock_path = topdir.join(LOCK_FILE_NAME);
        let state_file = topdir.join(STATE_FILE_NAME);
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening lock file {}", lock_path.to_string_lossy()))?;

        match lock_file.try_lock_exclusive() {
            Ok(()) => {
                let requests_file = topdir.join(REQUESTS_FILE_NAME);
                let state_file = topdir.join(STATE_FILE_NAME);
                let guard = Self::initialize_locked_session(
                    lock_file,
                    lock_path.as_path(),
                    state_file,
                    requests_file,
                    target_id,
                    packages,
                    BuildSessionKind::Build,
                    force_rebuild,
                )?;
                Ok(BuildAcquireOutcome::Owner(guard))
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                let active = load_state(&state_file).unwrap_or_default();
                let Some(owner) = active.entries.first() else {
                    bail!(
                        "workspace lock is held by another process and active state is unavailable (state file: {})",
                        state_file.to_string_lossy()
                    );
                };
                if owner.session_kind != BuildSessionKind::Build.as_str() {
                    bail!(
                        "workspace is already in use by pid={} target={} kind={} (state file: {})",
                        owner.pid,
                        owner.target_id,
                        owner.session_kind,
                        state_file.to_string_lossy()
                    );
                }
                if owner.target_id != target_id {
                    bail!(
                        "workspace build session target mismatch: active target={} requested target={} (state file: {})",
                        owner.target_id,
                        target_id,
                        state_file.to_string_lossy()
                    );
                }
                let queued_packages = packages
                    .iter()
                    .map(|pkg| pkg.trim())
                    .filter(|pkg| !pkg.is_empty())
                    .map(|pkg| pkg.to_string())
                    .collect::<Vec<_>>();
                if queued_packages.is_empty() {
                    bail!("no package names to submit to active build queue");
                }
                append_build_request(topdir, target_id, &queued_packages)?;
                Ok(BuildAcquireOutcome::Forwarded(ForwardedBuildRequest {
                    owner_pid: owner.pid,
                    owner_target_id: owner.target_id.clone(),
                    owner_force_rebuild: owner.force_rebuild,
                    queued_packages,
                }))
            }
            Err(err) => Err(err).with_context(|| {
                format!("acquiring workspace lock {}", lock_path.to_string_lossy())
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn initialize_locked_session(
        mut lock_file: fs::File,
        lock_path: &Path,
        state_file: PathBuf,
        requests_file: PathBuf,
        target_id: &str,
        packages: &[String],
        session_kind: BuildSessionKind,
        force_rebuild: bool,
    ) -> Result<Self> {
        let pid = std::process::id();
        let entry = ActiveBuildEntry {
            pid,
            target_id: target_id.to_string(),
            packages: packages.to_vec(),
            session_kind: session_kind.as_str().to_string(),
            force_rebuild,
            started_at_utc: chrono::Utc::now().to_rfc3339(),
        };
        let state = ActiveBuildState {
            entries: vec![entry],
        };
        write_state(&state_file, &state)?;

        lock_file
            .set_len(0)
            .with_context(|| format!("truncating lock file {}", lock_path.to_string_lossy()))?;
        writeln!(lock_file, "pid={pid}")
            .with_context(|| format!("writing lock file {}", lock_path.to_string_lossy()))?;
        lock_file
            .flush()
            .with_context(|| format!("flushing lock file {}", lock_path.to_string_lossy()))?;

        Ok(Self {
            lock_file,
            state_file,
            requests_file,
            pid,
            session_kind,
        })
    }
}

impl Drop for BuildSessionGuard {
    fn drop(&mut self) {
        let mut state = load_state(&self.state_file).unwrap_or_default();
        state.entries.retain(|entry| entry.pid != self.pid);
        if state.entries.is_empty() {
            let _ = fs::remove_file(&self.state_file);
            if self.session_kind == BuildSessionKind::Build {
                let _ = fs::remove_file(&self.requests_file);
            }
        } else {
            let _ = write_state(&self.state_file, &state);
        }
        let _ = self.lock_file.unlock();
    }
}

pub fn drain_forwarded_build_requests(topdir: &Path, target_id: &str) -> Result<Vec<String>> {
    let requests_file = topdir.join(REQUESTS_FILE_NAME);
    if !requests_file.exists() {
        return Ok(Vec::new());
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&requests_file)
        .with_context(|| format!("opening build requests file {}", requests_file.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("locking build requests file {}", requests_file.display())
            });
        }
    }

    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("seeking build requests file {}", requests_file.display()))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .with_context(|| format!("reading build requests file {}", requests_file.display()))?;

    let mut queued = Vec::new();
    let mut retained_lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<BuildQueueRequest>(trimmed) else {
            retained_lines.push(trimmed.to_string());
            continue;
        };
        if req.target_id == target_id {
            queued.extend(
                req.packages
                    .into_iter()
                    .map(|pkg| pkg.trim().to_string())
                    .filter(|pkg| !pkg.is_empty()),
            );
        } else {
            retained_lines.push(trimmed.to_string());
        }
    }

    file.set_len(0)
        .with_context(|| format!("truncating build requests file {}", requests_file.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("rewinding build requests file {}", requests_file.display()))?;
    if !retained_lines.is_empty() {
        let payload = format!("{}\n", retained_lines.join("\n"));
        file.write_all(payload.as_bytes())
            .with_context(|| format!("writing build requests file {}", requests_file.display()))?;
    }
    file.flush()
        .with_context(|| format!("flushing build requests file {}", requests_file.display()))?;
    file.unlock()
        .with_context(|| format!("unlocking build requests file {}", requests_file.display()))?;

    Ok(queued)
}

fn load_state(path: &Path) -> Result<ActiveBuildState> {
    if !path.exists() {
        return Ok(ActiveBuildState::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading active build state {}", path.to_string_lossy()))?;
    if raw.trim().is_empty() {
        return Ok(ActiveBuildState::default());
    }
    serde_json::from_str(&raw)
        .with_context(|| format!("parsing active build state {}", path.to_string_lossy()))
}

fn write_state(path: &Path, state: &ActiveBuildState) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let payload = serde_json::to_vec_pretty(state).context("serializing active build state")?;
    fs::write(&tmp, payload)
        .with_context(|| format!("writing active build temp state {}", tmp.to_string_lossy()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("committing active build state {}", path.to_string_lossy()))?;
    Ok(())
}

fn append_build_request(topdir: &Path, target_id: &str, packages: &[String]) -> Result<()> {
    let requests_file = topdir.join(REQUESTS_FILE_NAME);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&requests_file)
        .with_context(|| format!("opening build requests file {}", requests_file.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("locking build requests file {}", requests_file.display()))?;

    let request = BuildQueueRequest {
        pid: std::process::id(),
        target_id: target_id.to_string(),
        packages: packages.to_vec(),
        submitted_at_utc: chrono::Utc::now().to_rfc3339(),
    };
    let payload = serde_json::to_string(&request).context("serializing build queue request")?;
    writeln!(file, "{payload}")
        .with_context(|| format!("writing build requests file {}", requests_file.display()))?;
    file.flush()
        .with_context(|| format!("flushing build requests file {}", requests_file.display()))?;
    file.unlock()
        .with_context(|| format!("unlocking build requests file {}", requests_file.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "bioconda2rpm-build-lock-test-{}-{}-{}",
            name,
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&path).expect("create temp test dir");
        path
    }

    #[test]
    fn drain_forwarded_build_requests_filters_by_target() {
        let topdir = tempdir("drain-forwarded");
        let requests = topdir.join(REQUESTS_FILE_NAME);
        let req_a = BuildQueueRequest {
            pid: 1,
            target_id: "target-a".to_string(),
            packages: vec!["samtools".to_string(), "bcftools".to_string()],
            submitted_at_utc: "2026-03-01T00:00:00Z".to_string(),
        };
        let req_b = BuildQueueRequest {
            pid: 2,
            target_id: "target-b".to_string(),
            packages: vec!["blast".to_string()],
            submitted_at_utc: "2026-03-01T00:00:01Z".to_string(),
        };
        let payload = format!(
            "{}\n{}\n",
            serde_json::to_string(&req_a).expect("serialize req a"),
            serde_json::to_string(&req_b).expect("serialize req b")
        );
        fs::write(&requests, payload).expect("seed requests file");

        let drained = drain_forwarded_build_requests(&topdir, "target-a").expect("drain requests");
        assert_eq!(
            drained,
            vec!["samtools".to_string(), "bcftools".to_string()]
        );

        let remainder = fs::read_to_string(&requests).expect("read remaining requests");
        assert!(remainder.contains("\"target_id\":\"target-b\""));
        assert!(!remainder.contains("\"target_id\":\"target-a\""));

        let second = drain_forwarded_build_requests(&topdir, "target-a").expect("drain empty");
        assert!(second.is_empty());

        let _ = fs::remove_dir_all(&topdir);
    }

    #[test]
    fn load_state_backfills_defaults_for_legacy_entries() {
        let topdir = tempdir("legacy-state");
        let state_file = topdir.join(STATE_FILE_NAME);
        fs::write(
            &state_file,
            r#"{"entries":[{"pid":42,"target_id":"x","packages":["blast"],"started_at_utc":"2026-03-01T00:00:00Z"}]}"#,
        )
        .expect("write legacy state");

        let loaded = load_state(&state_file).expect("load state");
        assert_eq!(loaded.entries.len(), 1);
        let entry = &loaded.entries[0];
        assert_eq!(entry.session_kind, "build");
        assert!(!entry.force_rebuild);

        let _ = fs::remove_dir_all(&topdir);
    }
}
