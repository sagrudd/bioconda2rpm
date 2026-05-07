use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const LOCK_FILE_NAME: &str = ".bioconda2rpm-artifacts.lock";
const STATE_FILE_NAME: &str = ".bioconda2rpm-active-builds.json";
const REQUESTS_FILE_NAME: &str = ".bioconda2rpm-build-requests.jsonl";
const REMOVE_REQUESTS_FILE_NAME: &str = ".bioconda2rpm-remove-requests.jsonl";

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
    pub owner_refresh_files: bool,
    pub queued_packages: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ForwardedQueuedPackage {
    pub package: String,
    pub force_rebuild: bool,
    pub refresh_files: bool,
    pub submitted_host: String,
    pub submitted_pid: u32,
    pub submitted_at_utc: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LookupActiveBuildEntry {
    pub pid: u32,
    pub target_id: String,
    pub packages: Vec<String>,
    pub session_kind: String,
    pub force_rebuild: bool,
    pub refresh_files: bool,
    pub host: String,
    pub started_at_utc: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LookupQueuedBuildRequest {
    pub pid: u32,
    pub target_id: String,
    pub packages: Vec<String>,
    pub force_rebuild: bool,
    pub refresh_files: bool,
    pub submitted_host: String,
    pub submitted_at_utc: String,
}

#[derive(Debug, Clone)]
pub struct RemovedQueuedPackage {
    pub package: String,
    pub target_id: String,
    pub submitted_host: String,
    pub submitted_pid: u32,
    pub submitted_at_utc: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct QueueRemovalSummary {
    pub removed_packages: usize,
    pub retained_requests: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildLookupSnapshot {
    pub topdir: String,
    pub lock_held: bool,
    pub active_entries: Vec<LookupActiveBuildEntry>,
    pub queued_requests: Vec<LookupQueuedBuildRequest>,
    pub running_containers: Vec<String>,
    pub container_probe_error: Option<String>,
    pub updated_at_utc: String,
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
    #[serde(default)]
    refresh_files: bool,
    #[serde(default = "default_host_name")]
    host: String,
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
    #[serde(default)]
    force_rebuild: bool,
    #[serde(default)]
    refresh_files: bool,
    #[serde(default = "default_host_name")]
    submitted_host: String,
    submitted_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildRemoveRequest {
    pid: u32,
    target_id: String,
    packages: Vec<String>,
    #[serde(default = "default_host_name")]
    submitted_host: String,
    submitted_at_utc: String,
    #[serde(default)]
    reason: String,
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

fn default_host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown-host".to_string())
}

pub fn current_host_name() -> String {
    default_host_name()
}

pub fn lookup_build_runtime(topdir: &Path) -> Result<BuildLookupSnapshot> {
    let lock_path = topdir.join(LOCK_FILE_NAME);
    let state_file = topdir.join(STATE_FILE_NAME);
    let requests_file = topdir.join(REQUESTS_FILE_NAME);
    let lock_held = detect_lock_held(&lock_path)?;
    let active_state = load_state(&state_file).unwrap_or_default();
    let active_entries = active_state
        .entries
        .into_iter()
        .map(|entry| LookupActiveBuildEntry {
            pid: entry.pid,
            target_id: entry.target_id,
            packages: entry.packages,
            session_kind: entry.session_kind,
            force_rebuild: entry.force_rebuild,
            refresh_files: entry.refresh_files,
            host: entry.host,
            started_at_utc: entry.started_at_utc,
        })
        .collect::<Vec<_>>();
    let queued_requests = load_queued_requests(&requests_file)?;
    let (running_containers, container_probe_error) = probe_running_containers();

    Ok(BuildLookupSnapshot {
        topdir: topdir.to_string_lossy().to_string(),
        lock_held,
        active_entries,
        queued_requests,
        running_containers,
        container_probe_error,
        updated_at_utc: chrono::Utc::now().to_rfc3339(),
    })
}

impl BuildSessionGuard {
    pub fn acquire(
        topdir: &Path,
        target_id: &str,
        packages: &[String],
        session_kind: BuildSessionKind,
        force_rebuild: bool,
        refresh_files: bool,
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
                            "pid={} target={} kind={} force={} refresh_files={} packages={}",
                            entry.pid,
                            entry.target_id,
                            entry.session_kind,
                            entry.force_rebuild,
                            entry.refresh_files,
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
            refresh_files,
        )
    }

    pub fn acquire_or_forward_build(
        topdir: &Path,
        target_id: &str,
        packages: &[String],
        force_rebuild: bool,
        refresh_files: bool,
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
                    refresh_files,
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
                append_build_request(
                    topdir,
                    target_id,
                    &queued_packages,
                    force_rebuild,
                    refresh_files,
                )?;
                Ok(BuildAcquireOutcome::Forwarded(ForwardedBuildRequest {
                    owner_pid: owner.pid,
                    owner_target_id: owner.target_id.clone(),
                    owner_force_rebuild: owner.force_rebuild,
                    owner_refresh_files: owner.refresh_files,
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
        refresh_files: bool,
    ) -> Result<Self> {
        let pid = std::process::id();
        let entry = ActiveBuildEntry {
            pid,
            target_id: target_id.to_string(),
            packages: packages.to_vec(),
            session_kind: session_kind.as_str().to_string(),
            force_rebuild,
            refresh_files,
            host: current_host_name(),
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
                if let Some(topdir) = self.requests_file.parent() {
                    let _ = fs::remove_file(topdir.join(REMOVE_REQUESTS_FILE_NAME));
                }
            }
        } else {
            let _ = write_state(&self.state_file, &state);
        }
        let _ = self.lock_file.unlock();
    }
}

pub fn drain_forwarded_build_requests(
    topdir: &Path,
    target_id: &str,
) -> Result<Vec<ForwardedQueuedPackage>> {
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
            for package in req.packages {
                let package = package.trim().to_string();
                if package.is_empty() {
                    continue;
                }
                queued.push(ForwardedQueuedPackage {
                    package,
                    force_rebuild: req.force_rebuild,
                    refresh_files: req.refresh_files,
                    submitted_host: req.submitted_host.clone(),
                    submitted_pid: req.pid,
                    submitted_at_utc: req.submitted_at_utc.clone(),
                });
            }
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

pub fn append_remove_request(
    topdir: &Path,
    target_id: &str,
    packages: &[String],
    reason: &str,
) -> Result<()> {
    fs::create_dir_all(topdir)
        .with_context(|| format!("creating topdir {}", topdir.to_string_lossy()))?;
    let remove_file = topdir.join(REMOVE_REQUESTS_FILE_NAME);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&remove_file)
        .with_context(|| format!("opening remove requests file {}", remove_file.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("locking remove requests file {}", remove_file.display()))?;

    let request = BuildRemoveRequest {
        pid: std::process::id(),
        target_id: target_id.to_string(),
        packages: packages.to_vec(),
        submitted_host: current_host_name(),
        submitted_at_utc: chrono::Utc::now().to_rfc3339(),
        reason: reason.to_string(),
    };
    let payload = serde_json::to_string(&request).context("serializing build remove request")?;
    writeln!(file, "{payload}")
        .with_context(|| format!("writing remove requests file {}", remove_file.display()))?;
    file.flush()
        .with_context(|| format!("flushing remove requests file {}", remove_file.display()))?;
    file.unlock()
        .with_context(|| format!("unlocking remove requests file {}", remove_file.display()))?;
    Ok(())
}

pub fn drain_removed_build_requests(
    topdir: &Path,
    target_id: &str,
) -> Result<Vec<RemovedQueuedPackage>> {
    let remove_file = topdir.join(REMOVE_REQUESTS_FILE_NAME);
    if !remove_file.exists() {
        return Ok(Vec::new());
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&remove_file)
        .with_context(|| format!("opening remove requests file {}", remove_file.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("locking remove requests file {}", remove_file.display())
            });
        }
    }

    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("seeking remove requests file {}", remove_file.display()))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .with_context(|| format!("reading remove requests file {}", remove_file.display()))?;

    let mut removed = Vec::new();
    let mut retained_lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<BuildRemoveRequest>(trimmed) else {
            retained_lines.push(trimmed.to_string());
            continue;
        };
        if req.target_id == target_id {
            for package in req.packages {
                let package = package.trim().to_string();
                if package.is_empty() {
                    continue;
                }
                removed.push(RemovedQueuedPackage {
                    package,
                    target_id: req.target_id.clone(),
                    submitted_host: req.submitted_host.clone(),
                    submitted_pid: req.pid,
                    submitted_at_utc: req.submitted_at_utc.clone(),
                    reason: req.reason.clone(),
                });
            }
        } else {
            retained_lines.push(trimmed.to_string());
        }
    }

    file.set_len(0)
        .with_context(|| format!("truncating remove requests file {}", remove_file.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("rewinding remove requests file {}", remove_file.display()))?;
    if !retained_lines.is_empty() {
        let payload = format!("{}\n", retained_lines.join("\n"));
        file.write_all(payload.as_bytes())
            .with_context(|| format!("writing remove requests file {}", remove_file.display()))?;
    }
    file.flush()
        .with_context(|| format!("flushing remove requests file {}", remove_file.display()))?;
    file.unlock()
        .with_context(|| format!("unlocking remove requests file {}", remove_file.display()))?;

    Ok(removed)
}

pub fn remove_queued_build_requests(
    topdir: &Path,
    target_id: Option<&str>,
    packages: &[String],
) -> Result<QueueRemovalSummary> {
    let requests_file = topdir.join(REQUESTS_FILE_NAME);
    if !requests_file.exists() {
        return Ok(QueueRemovalSummary {
            removed_packages: 0,
            retained_requests: 0,
        });
    }
    let remove_keys = packages
        .iter()
        .map(|pkg| normalize_package_key(pkg))
        .filter(|pkg| !pkg.is_empty())
        .collect::<std::collections::BTreeSet<_>>();
    if remove_keys.is_empty() {
        return Ok(QueueRemovalSummary {
            removed_packages: 0,
            retained_requests: 0,
        });
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&requests_file)
        .with_context(|| format!("opening build requests file {}", requests_file.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("locking build requests file {}", requests_file.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("seeking build requests file {}", requests_file.display()))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .with_context(|| format!("reading build requests file {}", requests_file.display()))?;

    let mut removed_packages = 0usize;
    let mut retained_lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(mut req) = serde_json::from_str::<BuildQueueRequest>(trimmed) else {
            retained_lines.push(trimmed.to_string());
            continue;
        };
        if target_id.is_some_and(|target| req.target_id != target) {
            retained_lines.push(trimmed.to_string());
            continue;
        }
        let before = req.packages.len();
        req.packages
            .retain(|pkg| !remove_keys.contains(&normalize_package_key(pkg)));
        removed_packages += before.saturating_sub(req.packages.len());
        if req.packages.is_empty() {
            continue;
        }
        retained_lines
            .push(serde_json::to_string(&req).context("serializing retained queue request")?);
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

    Ok(QueueRemovalSummary {
        removed_packages,
        retained_requests: retained_lines.len(),
    })
}

pub fn stop_matching_build_containers(engine: &str, packages: &[String]) -> Result<Vec<String>> {
    let package_prefixes = packages
        .iter()
        .map(|pkg| sanitize_container_component(pkg))
        .filter(|pkg| !pkg.is_empty())
        .map(|pkg| format!("bioconda2rpm-{pkg}-"))
        .collect::<Vec<_>>();
    if package_prefixes.is_empty() {
        return Ok(Vec::new());
    }
    let output = Command::new(engine)
        .args(["ps", "--format", "{{.Names}}"])
        .output()
        .with_context(|| format!("probing running build containers with {engine}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!(
            "container probe failed: {}",
            if err.is_empty() {
                output.status.to_string()
            } else {
                err
            }
        );
    }
    let matches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| {
            !name.is_empty()
                && package_prefixes
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    for name in &matches {
        let status = Command::new(engine)
            .args(["rm", "-f", name])
            .status()
            .with_context(|| format!("stopping build container {name} with {engine}"))?;
        if !status.success() {
            bail!("failed to stop build container {name}: {status}");
        }
    }
    Ok(matches)
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

fn normalize_package_key(raw: &str) -> String {
    raw.trim().to_ascii_lowercase().replace('_', "-")
}

fn sanitize_container_component(input: &str) -> String {
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

fn detect_lock_held(lock_path: &Path) -> Result<bool> {
    let Some(parent) = lock_path.parent() else {
        return Ok(false);
    };
    if !parent.exists() {
        return Ok(false);
    }
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .with_context(|| format!("opening workspace lock file {}", lock_path.display()))?;
    match lock_file.try_lock_exclusive() {
        Ok(()) => {
            lock_file.unlock().with_context(|| {
                format!("unlocking workspace lock file {}", lock_path.display())
            })?;
            Ok(false)
        }
        Err(err) if err.kind() == ErrorKind::WouldBlock => Ok(true),
        Err(err) => Err(err).with_context(|| {
            format!(
                "probing workspace lock state for {}",
                lock_path.to_string_lossy()
            )
        }),
    }
}

fn load_queued_requests(path: &Path) -> Result<Vec<LookupQueuedBuildRequest>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading build requests file {}", path.display()))?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<BuildQueueRequest>(trimmed) else {
            continue;
        };
        out.push(LookupQueuedBuildRequest {
            pid: req.pid,
            target_id: req.target_id,
            packages: req.packages,
            force_rebuild: req.force_rebuild,
            refresh_files: req.refresh_files,
            submitted_host: req.submitted_host,
            submitted_at_utc: req.submitted_at_utc,
        });
    }
    Ok(out)
}

fn probe_running_containers() -> (Vec<String>, Option<String>) {
    let output = Command::new("docker")
        .args(["ps", "--format", "{{.Names}}"])
        .output();
    let Ok(output) = output else {
        return (Vec::new(), Some("docker command unavailable".to_string()));
    };
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if err.is_empty() {
            format!("docker ps exit={}", output.status)
        } else {
            err
        };
        return (Vec::new(), Some(detail));
    }
    let mut containers = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|name| !name.is_empty() && name.contains("bioconda2rpm-"))
        .collect::<Vec<_>>();
    containers.sort();
    (containers, None)
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

fn append_build_request(
    topdir: &Path,
    target_id: &str,
    packages: &[String],
    force_rebuild: bool,
    refresh_files: bool,
) -> Result<()> {
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
        force_rebuild,
        refresh_files,
        submitted_host: current_host_name(),
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
            force_rebuild: true,
            refresh_files: true,
            submitted_host: "host-a".to_string(),
            submitted_at_utc: "2026-03-01T00:00:00Z".to_string(),
        };
        let req_b = BuildQueueRequest {
            pid: 2,
            target_id: "target-b".to_string(),
            packages: vec!["blast".to_string()],
            force_rebuild: false,
            refresh_files: false,
            submitted_host: "host-b".to_string(),
            submitted_at_utc: "2026-03-01T00:00:01Z".to_string(),
        };
        let payload = format!(
            "{}\n{}\n",
            serde_json::to_string(&req_a).expect("serialize req a"),
            serde_json::to_string(&req_b).expect("serialize req b")
        );
        fs::write(&requests, payload).expect("seed requests file");

        let drained = drain_forwarded_build_requests(&topdir, "target-a").expect("drain requests");
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].package, "samtools");
        assert!(drained[0].force_rebuild);
        assert!(drained[0].refresh_files);
        assert_eq!(drained[0].submitted_host, "host-a");
        assert_eq!(drained[1].package, "bcftools");
        assert!(drained[1].force_rebuild);
        assert!(drained[1].refresh_files);
        assert_eq!(drained[1].submitted_host, "host-a");

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
        assert!(!entry.refresh_files);

        let _ = fs::remove_dir_all(&topdir);
    }

    #[test]
    fn drain_forwarded_build_requests_backfills_legacy_submit_host() {
        let topdir = tempdir("legacy-queue-host");
        let requests = topdir.join(REQUESTS_FILE_NAME);
        fs::write(
            &requests,
            r#"{"pid":3,"target_id":"target-a","packages":["blast"],"submitted_at_utc":"2026-03-01T00:00:02Z"}"#,
        )
        .expect("write legacy request");

        let drained = drain_forwarded_build_requests(&topdir, "target-a").expect("drain requests");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].package, "blast");
        assert!(!drained[0].force_rebuild);
        assert!(!drained[0].refresh_files);
        assert!(!drained[0].submitted_host.is_empty());

        let _ = fs::remove_dir_all(&topdir);
    }

    #[test]
    fn remove_queued_build_requests_prunes_matching_packages() {
        let topdir = tempdir("remove-queued");
        let requests = topdir.join(REQUESTS_FILE_NAME);
        let req_a = BuildQueueRequest {
            pid: 1,
            target_id: "target-a".to_string(),
            packages: vec![
                "emboss".to_string(),
                "blast".to_string(),
                "samtools".to_string(),
            ],
            force_rebuild: false,
            refresh_files: false,
            submitted_host: "host-a".to_string(),
            submitted_at_utc: "2026-05-07T00:00:00Z".to_string(),
        };
        let req_b = BuildQueueRequest {
            pid: 2,
            target_id: "target-b".to_string(),
            packages: vec!["emboss".to_string()],
            force_rebuild: false,
            refresh_files: false,
            submitted_host: "host-b".to_string(),
            submitted_at_utc: "2026-05-07T00:00:01Z".to_string(),
        };
        let payload = format!(
            "{}\n{}\n",
            serde_json::to_string(&req_a).expect("serialize req a"),
            serde_json::to_string(&req_b).expect("serialize req b")
        );
        fs::write(&requests, payload).expect("seed queue");

        let summary = remove_queued_build_requests(
            &topdir,
            Some("target-a"),
            &["blast".to_string(), "emboss".to_string()],
        )
        .expect("remove queued packages");
        assert_eq!(summary.removed_packages, 2);
        assert_eq!(summary.retained_requests, 2);

        let remaining = fs::read_to_string(&requests).expect("read queue");
        assert!(!remaining.contains("\"blast\""));
        assert!(remaining.contains("\"samtools\""));
        assert!(remaining.contains("\"target-b\""));
        assert!(remaining.contains("\"emboss\""));

        let _ = fs::remove_dir_all(&topdir);
    }

    #[test]
    fn drain_removed_build_requests_filters_by_target() {
        let topdir = tempdir("drain-removed");
        let remove_file = topdir.join(REMOVE_REQUESTS_FILE_NAME);
        let req_a = BuildRemoveRequest {
            pid: 9,
            target_id: "target-a".to_string(),
            packages: vec!["emboss".to_string(), "blast".to_string()],
            submitted_host: "host-a".to_string(),
            submitted_at_utc: "2026-05-07T00:00:00Z".to_string(),
            reason: "operator removed".to_string(),
        };
        let req_b = BuildRemoveRequest {
            pid: 10,
            target_id: "target-b".to_string(),
            packages: vec!["samtools".to_string()],
            submitted_host: "host-b".to_string(),
            submitted_at_utc: "2026-05-07T00:00:01Z".to_string(),
            reason: "other target".to_string(),
        };
        let payload = format!(
            "{}\n{}\n",
            serde_json::to_string(&req_a).expect("serialize req a"),
            serde_json::to_string(&req_b).expect("serialize req b")
        );
        fs::write(&remove_file, payload).expect("seed remove queue");

        let removed =
            drain_removed_build_requests(&topdir, "target-a").expect("drain remove requests");
        assert_eq!(removed.len(), 2);
        assert_eq!(removed[0].package, "emboss");
        assert_eq!(removed[0].reason, "operator removed");
        assert_eq!(removed[1].target_id, "target-a");

        let remaining = fs::read_to_string(&remove_file).expect("read remaining remove queue");
        assert!(remaining.contains("\"target-b\""));
        assert!(!remaining.contains("\"target-a\""));

        let _ = fs::remove_dir_all(&topdir);
    }

    #[test]
    fn lookup_build_runtime_reports_active_and_queued_state() {
        let topdir = tempdir("lookup-runtime");
        let state_file = topdir.join(STATE_FILE_NAME);
        let requests_file = topdir.join(REQUESTS_FILE_NAME);

        write_state(
            &state_file,
            &ActiveBuildState {
                entries: vec![ActiveBuildEntry {
                    pid: 1234,
                    target_id: "target-a".to_string(),
                    packages: vec!["trinity".to_string()],
                    session_kind: BuildSessionKind::Build.as_str().to_string(),
                    force_rebuild: false,
                    refresh_files: false,
                    host: "host-a".to_string(),
                    started_at_utc: "2026-03-02T00:00:00Z".to_string(),
                }],
            },
        )
        .expect("write state");
        fs::write(
            &requests_file,
            r#"{"pid":77,"target_id":"target-a","packages":["pplacer","mothur"],"submitted_host":"host-b","submitted_at_utc":"2026-03-02T00:01:00Z"}"#,
        )
        .expect("write queue request");

        let snapshot = lookup_build_runtime(&topdir).expect("lookup build runtime");
        assert_eq!(snapshot.topdir, topdir.to_string_lossy().to_string());
        assert_eq!(snapshot.active_entries.len(), 1);
        assert_eq!(snapshot.active_entries[0].packages, vec!["trinity"]);
        assert_eq!(snapshot.queued_requests.len(), 1);
        assert_eq!(
            snapshot.queued_requests[0].packages,
            vec!["pplacer".to_string(), "mothur".to_string()]
        );

        let _ = fs::remove_dir_all(&topdir);
    }
}
