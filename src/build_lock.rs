use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{ffi::OsStr, process::Output};
use wait_timeout::ChildExt;

const LOCK_FILE_NAME: &str = ".bioconda2rpm-artifacts.lock";
const STATE_FILE_NAME: &str = ".bioconda2rpm-active-builds.json";
const REQUESTS_FILE_NAME: &str = ".bioconda2rpm-build-requests.jsonl";
const REMOVE_REQUESTS_FILE_NAME: &str = ".bioconda2rpm-remove-requests.jsonl";
const SERVER_CONTROL_REQUESTS_FILE_NAME: &str = ".bioconda2rpm-server-control.jsonl";
const SERVER_STATUS_FILE_NAME: &str = ".bioconda2rpm-server-status.json";
const SERVER_PROGRESS_FILE_NAME: &str = ".bioconda2rpm-server-progress.json";
const RUNTIME_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RUNTIME_PROBE_OUTPUT_BYTES: u64 = 1024 * 1024;

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
    pub manual_source_files: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ForwardedQueuedPackage {
    pub package: String,
    pub force_rebuild: bool,
    pub refresh_files: bool,
    pub manual_source_files: Vec<PathBuf>,
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
    pub manual_source_files: Vec<String>,
    pub submitted_host: String,
    pub submitted_at_utc: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LookupServerStatus {
    pub target_id: String,
    pub pid: u32,
    pub closing: bool,
    pub current_phase: String,
    pub current_status: String,
    pub current_packages: Vec<String>,
    pub pending_packages: Vec<String>,
    pub last_progress: Option<String>,
    pub recent_progress: Vec<String>,
    pub active_containers: Vec<LookupBuildContainer>,
    pub recent_logs: Vec<LookupBuildLog>,
    pub updated_at_utc: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LookupBuildContainer {
    pub name: String,
    pub package: String,
    pub spec: String,
    pub attempt: Option<usize>,
    pub pid: Option<u32>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LookupBuildLog {
    pub package: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_at_utc: String,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServerControlAction {
    Close,
    Kill,
}

#[derive(Debug, Clone)]
pub struct ServerControlRequest {
    pub action: ServerControlAction,
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
    pub runtime_probe_status: String,
    pub container_probe_error: Option<String>,
    pub server_status: Option<LookupServerStatus>,
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
    #[serde(default)]
    manual_source_files: Vec<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildServerControlRequest {
    pid: u32,
    target_id: String,
    action: ServerControlAction,
    #[serde(default = "default_host_name")]
    submitted_host: String,
    submitted_at_utc: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BuildServerStatus {
    target_id: String,
    pid: u32,
    #[serde(default)]
    closing: bool,
    updated_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BuildServerProgressState {
    target_id: String,
    pid: u32,
    #[serde(default)]
    current_phase: String,
    #[serde(default)]
    current_status: String,
    #[serde(default)]
    current_packages: Vec<String>,
    #[serde(default)]
    pending_packages: Vec<String>,
    #[serde(default)]
    recent_progress: Vec<String>,
    updated_at_utc: String,
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
    lookup_build_runtime_with_probe(topdir, &DockerRuntimeProbe::default())
}

fn lookup_build_runtime_with_probe(
    topdir: &Path,
    probe: &dyn RuntimeProbe,
) -> Result<BuildLookupSnapshot> {
    let lock_path = topdir.join(LOCK_FILE_NAME);
    let state_file = topdir.join(STATE_FILE_NAME);
    let requests_file = topdir.join(REQUESTS_FILE_NAME);
    let progress_file = topdir.join(SERVER_PROGRESS_FILE_NAME);
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
    let probe_outcome = probe.probe();
    let runtime_containers = probe_outcome.healthy_containers().unwrap_or_default();
    let (running_containers, runtime_probe_status, container_probe_error) =
        probe_outcome.into_lookup_fields();
    let server_status = load_lookup_server_status(
        topdir,
        &progress_file,
        &active_entries,
        &queued_requests,
        &runtime_containers,
    )?;

    Ok(BuildLookupSnapshot {
        topdir: topdir.to_string_lossy().to_string(),
        lock_held,
        active_entries,
        queued_requests,
        running_containers,
        runtime_probe_status,
        container_probe_error,
        server_status,
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
        Self::acquire_with_probe(
            topdir,
            target_id,
            packages,
            session_kind,
            force_rebuild,
            refresh_files,
            &DockerRuntimeProbe::default(),
        )
    }

    fn acquire_with_probe(
        topdir: &Path,
        target_id: &str,
        packages: &[String],
        session_kind: BuildSessionKind,
        force_rebuild: bool,
        refresh_files: bool,
        probe: &dyn RuntimeProbe,
    ) -> Result<Self> {
        if session_kind != BuildSessionKind::GeneratePrioritySpecs {
            ensure_runtime_known(probe)?;
        }
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
        manual_source_files: &[PathBuf],
    ) -> Result<BuildAcquireOutcome> {
        Self::acquire_or_forward_build_with_probe(
            topdir,
            target_id,
            packages,
            force_rebuild,
            refresh_files,
            manual_source_files,
            &DockerRuntimeProbe::default(),
        )
    }

    fn acquire_or_forward_build_with_probe(
        topdir: &Path,
        target_id: &str,
        packages: &[String],
        force_rebuild: bool,
        refresh_files: bool,
        manual_source_files: &[PathBuf],
        probe: &dyn RuntimeProbe,
    ) -> Result<BuildAcquireOutcome> {
        ensure_runtime_known(probe)?;
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
                if server_is_closing(topdir, target_id)? {
                    bail!(
                        "active build server for target={} is closing and no longer accepts forwarded build requests",
                        target_id
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
                    manual_source_files,
                )?;
                Ok(BuildAcquireOutcome::Forwarded(ForwardedBuildRequest {
                    owner_pid: owner.pid,
                    owner_target_id: owner.target_id.clone(),
                    owner_force_rebuild: owner.force_rebuild,
                    owner_refresh_files: owner.refresh_files,
                    queued_packages,
                    manual_source_files: manual_source_files.to_vec(),
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
        if let Some(topdir) = lock_path.parent() {
            let _ = fs::remove_file(topdir.join(SERVER_STATUS_FILE_NAME));
            let _ = fs::remove_file(topdir.join(SERVER_PROGRESS_FILE_NAME));
        }
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
                    let _ = fs::remove_file(topdir.join(SERVER_CONTROL_REQUESTS_FILE_NAME));
                    let _ = fs::remove_file(topdir.join(SERVER_STATUS_FILE_NAME));
                    let _ = fs::remove_file(topdir.join(SERVER_PROGRESS_FILE_NAME));
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
            let manual_source_files = req
                .manual_source_files
                .iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            for package in req.packages {
                let package = package.trim().to_string();
                if package.is_empty() {
                    continue;
                }
                queued.push(ForwardedQueuedPackage {
                    package,
                    force_rebuild: req.force_rebuild,
                    refresh_files: req.refresh_files,
                    manual_source_files: manual_source_files.clone(),
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

pub fn append_server_control_request(
    topdir: &Path,
    target_id: &str,
    action: ServerControlAction,
    reason: &str,
) -> Result<()> {
    fs::create_dir_all(topdir)
        .with_context(|| format!("creating topdir {}", topdir.to_string_lossy()))?;
    let control_file = topdir.join(SERVER_CONTROL_REQUESTS_FILE_NAME);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&control_file)
        .with_context(|| format!("opening server control file {}", control_file.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("locking server control file {}", control_file.display()))?;

    let request = BuildServerControlRequest {
        pid: std::process::id(),
        target_id: target_id.to_string(),
        action,
        submitted_host: current_host_name(),
        submitted_at_utc: chrono::Utc::now().to_rfc3339(),
        reason: reason.to_string(),
    };
    let payload = serde_json::to_string(&request).context("serializing server control request")?;
    writeln!(file, "{payload}")
        .with_context(|| format!("writing server control file {}", control_file.display()))?;
    file.flush()
        .with_context(|| format!("flushing server control file {}", control_file.display()))?;
    file.unlock()
        .with_context(|| format!("unlocking server control file {}", control_file.display()))?;
    Ok(())
}

pub fn record_server_progress(
    topdir: &Path,
    target_id: &str,
    pending_packages: &[String],
    current_packages: &[String],
    progress_line: &str,
) -> Result<()> {
    let progress_file = topdir.join(SERVER_PROGRESS_FILE_NAME);
    let mut state = load_server_progress_state(&progress_file).unwrap_or_default();
    if state.target_id != target_id {
        state = BuildServerProgressState {
            target_id: target_id.to_string(),
            pid: std::process::id(),
            ..BuildServerProgressState::default()
        };
    }
    state.pid = std::process::id();
    state.target_id = target_id.to_string();
    state.pending_packages = pending_packages.to_vec();
    state.current_packages = current_packages.to_vec();
    let cleaned = progress_line
        .strip_prefix("progress ")
        .unwrap_or(progress_line)
        .trim()
        .to_string();
    if !cleaned.is_empty() {
        let kv = parse_progress_kv(&cleaned);
        if let Some(phase) = kv.get("phase") {
            state.current_phase = phase.clone();
        }
        if let Some(status) = kv.get("status") {
            state.current_status = status.clone();
        }
        if let Some(package) = kv.get("package") {
            state.current_packages = vec![package.clone()];
        } else if let Some(label) = kv.get("label") {
            state.current_packages = vec![label.clone()];
        }
        if let Some(packages) = kv.get("packages") {
            let parsed = packages
                .split(',')
                .map(str::trim)
                .filter(|pkg| !pkg.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            if !parsed.is_empty() {
                state.current_packages = parsed;
            }
        }
        state.recent_progress.push(cleaned);
        let keep_from = state.recent_progress.len().saturating_sub(24);
        if keep_from > 0 {
            state.recent_progress.drain(0..keep_from);
        }
    }
    state.updated_at_utc = chrono::Utc::now().to_rfc3339();
    write_server_progress_state(&progress_file, &state)
}

pub fn mark_server_closing(topdir: &Path, target_id: &str) -> Result<()> {
    let status_file = topdir.join(SERVER_STATUS_FILE_NAME);
    let status = BuildServerStatus {
        target_id: target_id.to_string(),
        pid: std::process::id(),
        closing: true,
        updated_at_utc: chrono::Utc::now().to_rfc3339(),
    };
    let tmp = status_file.with_extension("tmp");
    let payload = serde_json::to_vec_pretty(&status).context("serializing server status")?;
    fs::write(&tmp, payload)
        .with_context(|| format!("writing server status temp file {}", tmp.display()))?;
    fs::rename(&tmp, &status_file)
        .with_context(|| format!("committing server status file {}", status_file.display()))?;
    Ok(())
}

fn server_is_closing(topdir: &Path, target_id: &str) -> Result<bool> {
    let status_file = topdir.join(SERVER_STATUS_FILE_NAME);
    if !status_file.exists() {
        return Ok(false);
    }
    let raw = fs::read_to_string(&status_file)
        .with_context(|| format!("reading server status file {}", status_file.display()))?;
    if raw.trim().is_empty() {
        return Ok(false);
    }
    let status: BuildServerStatus = serde_json::from_str(&raw)
        .with_context(|| format!("parsing server status file {}", status_file.display()))?;
    Ok(status.target_id == target_id && status.closing)
}

pub fn drain_server_control_requests(
    topdir: &Path,
    target_id: &str,
) -> Result<Vec<ServerControlRequest>> {
    drain_server_control_requests_matching(topdir, target_id, |_| true)
}

pub fn drain_kill_server_control_requests(
    topdir: &Path,
    target_id: &str,
) -> Result<Vec<ServerControlRequest>> {
    drain_server_control_requests_matching(topdir, target_id, |action| {
        action == ServerControlAction::Kill
    })
}

fn drain_server_control_requests_matching(
    topdir: &Path,
    target_id: &str,
    should_drain: impl Fn(ServerControlAction) -> bool,
) -> Result<Vec<ServerControlRequest>> {
    let control_file = topdir.join(SERVER_CONTROL_REQUESTS_FILE_NAME);
    if !control_file.exists() {
        return Ok(Vec::new());
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&control_file)
        .with_context(|| format!("opening server control file {}", control_file.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("locking server control file {}", control_file.display())
            });
        }
    }

    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("seeking server control file {}", control_file.display()))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .with_context(|| format!("reading server control file {}", control_file.display()))?;

    let mut drained = Vec::new();
    let mut retained_lines = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<BuildServerControlRequest>(trimmed) else {
            retained_lines.push(trimmed.to_string());
            continue;
        };
        if req.target_id == target_id && should_drain(req.action) {
            drained.push(ServerControlRequest {
                action: req.action,
                target_id: req.target_id,
                submitted_host: req.submitted_host,
                submitted_pid: req.pid,
                submitted_at_utc: req.submitted_at_utc,
                reason: req.reason,
            });
        } else {
            retained_lines
                .push(serde_json::to_string(&req).context("serializing retained control request")?);
        }
    }

    file.set_len(0)
        .with_context(|| format!("truncating server control file {}", control_file.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("rewinding server control file {}", control_file.display()))?;
    if !retained_lines.is_empty() {
        let payload = format!("{}\n", retained_lines.join("\n"));
        file.write_all(payload.as_bytes())
            .with_context(|| format!("writing server control file {}", control_file.display()))?;
    }
    file.flush()
        .with_context(|| format!("flushing server control file {}", control_file.display()))?;
    file.unlock()
        .with_context(|| format!("unlocking server control file {}", control_file.display()))?;

    Ok(drained)
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
    stop_build_containers(engine, |name| {
        package_prefixes
            .iter()
            .any(|prefix| name.starts_with(prefix))
    })
}

pub fn stop_all_build_containers(engine: &str) -> Result<Vec<String>> {
    stop_build_containers(engine, |name| name.starts_with("bioconda2rpm-"))
}

fn stop_build_containers(engine: &str, matches_name: impl Fn(&str) -> bool) -> Result<Vec<String>> {
    let output = run_bounded_command(
        OsStr::new(engine),
        &["ps".into(), "--format".into(), "{{.Names}}".into()],
        RUNTIME_PROBE_TIMEOUT,
    )
    .map_err(|err| anyhow::anyhow!("probing running build containers with {engine}: {err}"))?;
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
        .filter(|name| !name.is_empty() && matches_name(name))
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
            manual_source_files: req.manual_source_files,
            submitted_host: req.submitted_host,
            submitted_at_utc: req.submitted_at_utc,
        });
    }
    Ok(out)
}

fn load_server_progress_state(path: &Path) -> Result<BuildServerProgressState> {
    if !path.exists() {
        return Ok(BuildServerProgressState::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("reading server progress file {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(BuildServerProgressState::default());
    }
    serde_json::from_str(&raw)
        .with_context(|| format!("parsing server progress file {}", path.display()))
}

fn write_server_progress_state(path: &Path, state: &BuildServerProgressState) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let payload = serde_json::to_vec_pretty(state).context("serializing server progress state")?;
    fs::write(&tmp, payload)
        .with_context(|| format!("writing server progress temp file {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("committing server progress file {}", path.display()))?;
    Ok(())
}

fn load_lookup_server_status(
    topdir: &Path,
    progress_file: &Path,
    active_entries: &[LookupActiveBuildEntry],
    queued_requests: &[LookupQueuedBuildRequest],
    runtime_containers: &[RuntimeContainerInfo],
) -> Result<Option<LookupServerStatus>> {
    let progress = load_server_progress_state(progress_file).unwrap_or_default();
    let target_id = progress
        .target_id
        .clone()
        .into_empty_none()
        .or_else(|| active_entries.first().map(|entry| entry.target_id.clone()))
        .or_else(|| queued_requests.first().map(|entry| entry.target_id.clone()));
    let Some(target_id) = target_id else {
        return Ok(None);
    };
    let pid = if progress.pid != 0 {
        progress.pid
    } else {
        active_entries.first().map(|entry| entry.pid).unwrap_or(0)
    };
    let closing = server_is_closing(topdir, &target_id).unwrap_or(false);
    let active_containers = lookup_build_containers(topdir, runtime_containers);
    let recent_logs = lookup_recent_build_logs(topdir, 8)?;
    let mut pending_packages = progress.pending_packages;
    if pending_packages.is_empty() {
        for req in queued_requests
            .iter()
            .filter(|req| req.target_id == target_id)
        {
            pending_packages.extend(req.packages.clone());
        }
        pending_packages.sort();
        pending_packages.dedup();
    }
    let last_progress = progress.recent_progress.last().cloned();
    Ok(Some(LookupServerStatus {
        target_id,
        pid,
        closing,
        current_phase: progress.current_phase,
        current_status: progress.current_status,
        current_packages: progress.current_packages,
        pending_packages,
        last_progress,
        recent_progress: progress.recent_progress,
        active_containers,
        recent_logs,
        updated_at_utc: if progress.updated_at_utc.is_empty() {
            chrono::Utc::now().to_rfc3339()
        } else {
            progress.updated_at_utc
        },
    }))
}

trait EmptyNone {
    fn into_empty_none(self) -> Option<String>;
}

impl EmptyNone for String {
    fn into_empty_none(self) -> Option<String> {
        if self.trim().is_empty() {
            None
        } else {
            Some(self)
        }
    }
}

fn parse_progress_kv(line: &str) -> BTreeMap<String, String> {
    line.split_whitespace()
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

fn lookup_build_containers(
    topdir: &Path,
    runtime_containers: &[RuntimeContainerInfo],
) -> Vec<LookupBuildContainer> {
    let topdir_pid = active_state_pid(topdir).unwrap_or_default();
    let mut out = Vec::new();
    for container in runtime_containers {
        let name = container.name.as_str();
        if !name.starts_with("bioconda2rpm-") {
            continue;
        }
        let parsed = parse_build_container_name(name);
        if topdir_pid != 0 && parsed.pid.is_some() && parsed.pid != Some(topdir_pid) {
            continue;
        }
        out.push(LookupBuildContainer {
            name: name.to_string(),
            package: parsed.package,
            spec: parsed.spec,
            attempt: parsed.attempt,
            pid: parsed.pid,
            status: container.status.clone(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

struct ParsedContainerName {
    package: String,
    spec: String,
    attempt: Option<usize>,
    pid: Option<u32>,
}

fn parse_build_container_name(name: &str) -> ParsedContainerName {
    let stripped = name.strip_prefix("bioconda2rpm-").unwrap_or(name);
    let mut parts = stripped.rsplitn(4, '-');
    let _millis = parts.next();
    let pid = parts
        .next()
        .and_then(|part| part.strip_prefix('p'))
        .and_then(|part| part.parse::<u32>().ok());
    let attempt = parts
        .next()
        .and_then(|part| part.strip_prefix('a'))
        .and_then(|part| part.parse::<usize>().ok());
    let label_spec = parts.next().unwrap_or(stripped);
    let spec_marker = "-phoreus-";
    let (package, spec) = if let Some(idx) = label_spec.find(spec_marker) {
        (
            label_spec[..idx].to_string(),
            label_spec[idx + 1..].to_string(),
        )
    } else {
        (label_spec.to_string(), String::new())
    };
    ParsedContainerName {
        package,
        spec,
        attempt,
        pid,
    }
}

fn active_state_pid(topdir: &Path) -> Option<u32> {
    load_state(&topdir.join(STATE_FILE_NAME))
        .ok()
        .and_then(|state| state.entries.first().map(|entry| entry.pid))
}

fn lookup_recent_build_logs(topdir: &Path, limit: usize) -> Result<Vec<LookupBuildLog>> {
    let reports_root = topdir.join("targets");
    if !reports_root.exists() {
        return Ok(Vec::new());
    }
    let mut logs = Vec::new();
    collect_build_logs(&reports_root, &mut logs)?;
    logs.sort_by(|a, b| a.modified_at_utc.cmp(&b.modified_at_utc));
    let keep_from = logs.len().saturating_sub(limit);
    if keep_from > 0 {
        logs.drain(0..keep_from);
    }
    Ok(logs)
}

fn collect_build_logs(dir: &Path, out: &mut Vec<LookupBuildLog>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading entry under {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("reading file type for {}", path.display()))?;
        if file_type.is_dir() {
            if path.file_name().and_then(|v| v.to_str()) == Some("build_logs")
                || path.components().any(|c| c.as_os_str() == "build_logs")
                || path.file_name().and_then(|v| v.to_str()) == Some("reports")
                || path.file_name().and_then(|v| v.to_str()) == Some("targets")
                || path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|v| v.to_str())
                    == Some("targets")
            {
                collect_build_logs(&path, out)?;
            }
            continue;
        }
        if path.extension().and_then(|v| v.to_str()) != Some("log") {
            continue;
        }
        if !path.components().any(|c| c.as_os_str() == "build_logs") {
            continue;
        }
        let metadata = entry
            .metadata()
            .with_context(|| format!("reading metadata for {}", path.display()))?;
        let modified = metadata
            .modified()
            .ok()
            .map(DateTime::<Utc>::from)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();
        let package = path
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or_default()
            .split(".attempt")
            .next()
            .unwrap_or_default()
            .to_string();
        out.push(LookupBuildLog {
            package,
            path: path.to_string_lossy().to_string(),
            size_bytes: metadata.len(),
            modified_at_utc: modified,
        });
    }
    Ok(())
}

trait RuntimeProbe: Send + Sync {
    fn probe(&self) -> RuntimeProbeOutcome;
}

struct DockerRuntimeProbe {
    command: std::ffi::OsString,
    args: Vec<std::ffi::OsString>,
    timeout: Duration,
}

impl Default for DockerRuntimeProbe {
    fn default() -> Self {
        Self {
            command: "docker".into(),
            args: vec![
                "ps".into(),
                "--format".into(),
                "{{.Names}}\t{{.ID}}\t{{.Status}}".into(),
            ],
            timeout: RUNTIME_PROBE_TIMEOUT,
        }
    }
}

impl RuntimeProbe for DockerRuntimeProbe {
    fn probe(&self) -> RuntimeProbeOutcome {
        return match run_bounded_command(&self.command, &self.args, self.timeout) {
            Err(BoundedCommandError::Unavailable(detail)) => {
                RuntimeProbeOutcome::Unavailable(detail)
            }
            Err(BoundedCommandError::TimedOut(timeout)) => RuntimeProbeOutcome::TimedOut(timeout),
            Err(BoundedCommandError::Failed(detail)) => RuntimeProbeOutcome::Failed(detail),
            Ok(output) => {
                if !output.status.success() {
                    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    let detail = if detail.is_empty() {
                        format!("docker ps exited with {}", output.status)
                    } else {
                        detail
                    };
                    return RuntimeProbeOutcome::Failed(detail);
                }
                let stdout = match String::from_utf8(output.stdout) {
                    Ok(stdout) => stdout,
                    Err(err) => {
                        return RuntimeProbeOutcome::Malformed(format!(
                            "docker ps returned non-UTF-8 output: {err}"
                        ));
                    }
                };
                let mut containers = Vec::new();
                for line in stdout.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let fields = line.split('\t').collect::<Vec<_>>();
                    if fields.len() != 3 {
                        return RuntimeProbeOutcome::Malformed(format!(
                            "docker ps returned a malformed row {line:?}"
                        ));
                    }
                    let name = fields[0].trim();
                    let id = fields[1].trim();
                    let status = fields[2].trim();
                    if name.is_empty()
                        || id.is_empty()
                        || status.is_empty()
                        || name.chars().any(char::is_control)
                        || name.chars().any(char::is_whitespace)
                        || id.chars().any(char::is_control)
                        || id.chars().any(char::is_whitespace)
                        || status.chars().any(char::is_control)
                    {
                        return RuntimeProbeOutcome::Malformed(format!(
                            "docker ps returned invalid container fields {line:?}"
                        ));
                    }
                    containers.push(RuntimeContainerInfo {
                        name: name.to_string(),
                        status: status.to_string(),
                    });
                }
                containers.sort_by(|a, b| a.name.cmp(&b.name));
                RuntimeProbeOutcome::Healthy(containers)
            }
        };
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeContainerInfo {
    name: String,
    status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeProbeOutcome {
    Healthy(Vec<RuntimeContainerInfo>),
    Unavailable(String),
    TimedOut(Duration),
    Failed(String),
    Malformed(String),
}

impl RuntimeProbeOutcome {
    fn healthy_containers(&self) -> Option<Vec<RuntimeContainerInfo>> {
        match self {
            Self::Healthy(containers) => Some(containers.clone()),
            _ => None,
        }
    }

    fn status_name(&self) -> &'static str {
        match self {
            Self::Healthy(_) => "healthy",
            Self::Unavailable(_) => "unavailable",
            Self::TimedOut(_) => "timeout",
            Self::Failed(_) => "failed",
            Self::Malformed(_) => "malformed",
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::Healthy(_) => None,
            Self::Unavailable(detail) | Self::Failed(detail) | Self::Malformed(detail) => {
                Some(detail.clone())
            }
            Self::TimedOut(timeout) => Some(format!(
                "docker ps exceeded the {} second timeout",
                timeout.as_secs_f64()
            )),
        }
    }

    fn into_lookup_fields(self) -> (Vec<String>, String, Option<String>) {
        let containers = match &self {
            Self::Healthy(containers) => containers
                .iter()
                .map(|container| container.name.clone())
                .filter(|name| name.starts_with("bioconda2rpm-"))
                .collect(),
            _ => Vec::new(),
        };
        let status = self.status_name().to_string();
        let error = self.detail();
        (containers, status, error)
    }
}

#[derive(Debug)]
enum BoundedCommandError {
    Unavailable(String),
    TimedOut(Duration),
    Failed(String),
}

impl std::fmt::Display for BoundedCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(detail) | Self::Failed(detail) => f.write_str(detail),
            Self::TimedOut(timeout) => write!(f, "command exceeded {timeout:?} timeout"),
        }
    }
}

fn run_bounded_command(
    command: &OsStr,
    args: &[std::ffi::OsString],
    timeout: Duration,
) -> std::result::Result<Output, BoundedCommandError> {
    let mut stdout_file = tempfile::tempfile()
        .map_err(|err| BoundedCommandError::Failed(format!("creating stdout capture: {err}")))?;
    let mut stderr_file = tempfile::tempfile()
        .map_err(|err| BoundedCommandError::Failed(format!("creating stderr capture: {err}")))?;
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file.try_clone().map_err(|err| {
            BoundedCommandError::Failed(format!("cloning stdout capture: {err}"))
        })?))
        .stderr(Stdio::from(stderr_file.try_clone().map_err(|err| {
            BoundedCommandError::Failed(format!("cloning stderr capture: {err}"))
        })?))
        .spawn()
        .map_err(|err| {
            if err.kind() == ErrorKind::NotFound {
                BoundedCommandError::Unavailable(format!(
                    "{} executable not found",
                    command.to_string_lossy()
                ))
            } else {
                BoundedCommandError::Failed(format!(
                    "starting {}: {err}",
                    command.to_string_lossy()
                ))
            }
        })?;

    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let stdout = read_captured_output(&mut stdout_file)
                .map_err(|err| BoundedCommandError::Failed(format!("reading stdout: {err}")))?;
            let stderr = read_captured_output(&mut stderr_file)
                .map_err(|err| BoundedCommandError::Failed(format!("reading stderr: {err}")))?;
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(BoundedCommandError::TimedOut(timeout))
        }
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(BoundedCommandError::Failed(format!(
                "waiting for command: {err}"
            )))
        }
    }
}

fn read_captured_output(file: &mut fs::File) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut output = Vec::new();
    (&mut *file)
        .take(MAX_RUNTIME_PROBE_OUTPUT_BYTES + 1)
        .read_to_end(&mut output)?;
    if output.len() as u64 > MAX_RUNTIME_PROBE_OUTPUT_BYTES {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "container runtime output exceeded 1 MiB",
        ));
    }
    Ok(output)
}

fn ensure_runtime_known(probe: &dyn RuntimeProbe) -> Result<()> {
    match probe.probe() {
        RuntimeProbeOutcome::Healthy(_) => Ok(()),
        outcome => bail!(
            "cannot acquire build session: Docker runtime state is {}: {}",
            outcome.status_name(),
            outcome
                .detail()
                .unwrap_or_else(|| "unknown error".to_string())
        ),
    }
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
    manual_source_files: &[PathBuf],
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
        manual_source_files: manual_source_files
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
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

    struct FakeProbe(RuntimeProbeOutcome);

    impl RuntimeProbe for FakeProbe {
        fn probe(&self) -> RuntimeProbeOutcome {
            self.0.clone()
        }
    }

    fn healthy_probe() -> FakeProbe {
        FakeProbe(RuntimeProbeOutcome::Healthy(Vec::new()))
    }

    fn acquire_test_build(
        topdir: &Path,
        target_id: &str,
        packages: &[String],
    ) -> Result<BuildSessionGuard> {
        BuildSessionGuard::acquire_with_probe(
            topdir,
            target_id,
            packages,
            BuildSessionKind::Build,
            false,
            false,
            &healthy_probe(),
        )
    }

    fn acquire_or_forward_test_build(
        topdir: &Path,
        target_id: &str,
        packages: &[String],
        force_rebuild: bool,
        refresh_files: bool,
        manual_source_files: &[PathBuf],
    ) -> Result<BuildAcquireOutcome> {
        BuildSessionGuard::acquire_or_forward_build_with_probe(
            topdir,
            target_id,
            packages,
            force_rebuild,
            refresh_files,
            manual_source_files,
            &healthy_probe(),
        )
    }

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
            manual_source_files: vec!["/tmp/cap3.tar.gz".to_string()],
            submitted_host: "host-a".to_string(),
            submitted_at_utc: "2026-03-01T00:00:00Z".to_string(),
        };
        let req_b = BuildQueueRequest {
            pid: 2,
            target_id: "target-b".to_string(),
            packages: vec!["blast".to_string()],
            force_rebuild: false,
            refresh_files: false,
            manual_source_files: Vec::new(),
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
        assert_eq!(
            drained[0].manual_source_files,
            vec![PathBuf::from("/tmp/cap3.tar.gz")]
        );
        assert_eq!(drained[0].submitted_host, "host-a");
        assert_eq!(drained[1].package, "bcftools");
        assert!(drained[1].force_rebuild);
        assert!(drained[1].refresh_files);
        assert_eq!(
            drained[1].manual_source_files,
            vec![PathBuf::from("/tmp/cap3.tar.gz")]
        );
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
        assert!(drained[0].manual_source_files.is_empty());
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
            manual_source_files: Vec::new(),
            submitted_host: "host-a".to_string(),
            submitted_at_utc: "2026-05-07T00:00:00Z".to_string(),
        };
        let req_b = BuildQueueRequest {
            pid: 2,
            target_id: "target-b".to_string(),
            packages: vec!["emboss".to_string()],
            force_rebuild: false,
            refresh_files: false,
            manual_source_files: Vec::new(),
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
    fn drain_server_control_requests_filters_by_target_and_action() {
        let topdir = tempdir("drain-server-control");
        append_server_control_request(
            &topdir,
            "target-a",
            ServerControlAction::Close,
            "operator drain",
        )
        .expect("append close request");
        append_server_control_request(
            &topdir,
            "target-a",
            ServerControlAction::Kill,
            "operator kill",
        )
        .expect("append kill request");
        append_server_control_request(
            &topdir,
            "target-b",
            ServerControlAction::Kill,
            "other target",
        )
        .expect("append other target request");

        let kills = drain_kill_server_control_requests(&topdir, "target-a")
            .expect("drain kill control requests");
        assert_eq!(kills.len(), 1);
        assert_eq!(kills[0].action, ServerControlAction::Kill);
        assert_eq!(kills[0].target_id, "target-a");
        assert_eq!(kills[0].reason, "operator kill");

        let remaining =
            fs::read_to_string(topdir.join(SERVER_CONTROL_REQUESTS_FILE_NAME)).expect("read queue");
        assert!(remaining.contains("\"action\":\"close\""));
        assert!(remaining.contains("\"target-b\""));
        assert!(remaining.contains("other target"));

        let drained = drain_server_control_requests(&topdir, "target-a")
            .expect("drain remaining control requests");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].action, ServerControlAction::Close);

        let final_remaining =
            fs::read_to_string(topdir.join(SERVER_CONTROL_REQUESTS_FILE_NAME)).expect("read queue");
        assert!(final_remaining.contains("\"target-b\""));
        assert!(!final_remaining.contains("\"target-a\""));

        let _ = fs::remove_dir_all(&topdir);
    }

    #[test]
    fn closing_server_rejects_new_forwarded_builds() {
        let topdir = tempdir("closing-rejects-forward");
        let owner = acquire_test_build(&topdir, "target-a", &["emboss".to_string()])
            .expect("acquire owner");
        mark_server_closing(&topdir, "target-a").expect("mark closing");

        let err = match acquire_or_forward_test_build(
            &topdir,
            "target-a",
            &["blast".to_string()],
            false,
            false,
            &[],
        ) {
            Ok(_) => panic!("closing server should reject forwarded builds"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("no longer accepts"),
            "unexpected error: {err:#}"
        );

        drop(owner);
        let _ = fs::remove_dir_all(&topdir);
    }

    #[test]
    fn lookup_build_runtime_reports_server_progress() {
        let topdir = tempdir("server-progress");
        let _owner = acquire_test_build(&topdir, "target-a", &[]).expect("acquire owner");
        record_server_progress(
            &topdir,
            "target-a",
            &["blast".to_string()],
            &["emboss".to_string()],
            "progress phase=dependency-index status=running package=emboss roots_done=1/1",
        )
        .expect("record progress");

        let snapshot =
            lookup_build_runtime_with_probe(&topdir, &healthy_probe()).expect("lookup runtime");
        let status = snapshot.server_status.expect("server status");
        assert_eq!(status.target_id, "target-a");
        assert_eq!(status.current_phase, "dependency-index");
        assert_eq!(status.current_status, "running");
        assert_eq!(status.current_packages, vec!["emboss"]);
        assert_eq!(status.pending_packages, vec!["blast"]);
        assert!(
            status
                .last_progress
                .as_deref()
                .unwrap_or_default()
                .contains("dependency-index")
        );

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

        let snapshot = lookup_build_runtime_with_probe(&topdir, &healthy_probe())
            .expect("lookup build runtime");
        assert_eq!(snapshot.topdir, topdir.to_string_lossy().to_string());
        assert_eq!(snapshot.active_entries.len(), 1);
        assert_eq!(snapshot.active_entries[0].packages, vec!["trinity"]);
        assert_eq!(snapshot.queued_requests.len(), 1);
        assert_eq!(
            snapshot.queued_requests[0].packages,
            vec!["pplacer".to_string(), "mothur".to_string()]
        );
        assert_eq!(snapshot.runtime_probe_status, "healthy");
        assert_eq!(snapshot.container_probe_error, None);

        let _ = fs::remove_dir_all(&topdir);
    }

    #[test]
    fn build_lock_rejects_timeout_unavailable_failed_and_malformed_runtime_probes() {
        let cases = [
            (
                RuntimeProbeOutcome::TimedOut(RUNTIME_PROBE_TIMEOUT),
                "timeout",
                "exceeded the 8 second timeout",
            ),
            (
                RuntimeProbeOutcome::Unavailable("docker executable not found".to_string()),
                "unavailable",
                "docker executable not found",
            ),
            (
                RuntimeProbeOutcome::Failed("Docker daemon returned an error".to_string()),
                "failed",
                "Docker daemon returned an error",
            ),
            (
                RuntimeProbeOutcome::Malformed("invalid container name".to_string()),
                "malformed",
                "invalid container name",
            ),
        ];

        for (outcome, expected_status, expected_detail) in cases {
            let topdir = tempdir("runtime-probe-fail-closed");
            let result = BuildSessionGuard::acquire_with_probe(
                &topdir,
                "target-a",
                &["samtools".to_string()],
                BuildSessionKind::Build,
                false,
                false,
                &FakeProbe(outcome),
            );
            let err = match result {
                Ok(_) => panic!("uncertain Docker state must prevent build lock acquisition"),
                Err(err) => err,
            };
            let message = format!("{err:#}");
            assert!(message.contains(expected_status), "{message}");
            assert!(message.contains(expected_detail), "{message}");
            assert!(
                !topdir.join(LOCK_FILE_NAME).exists(),
                "build lock file should not be created for {expected_status}"
            );
            let _ = fs::remove_dir_all(&topdir);
        }
    }

    #[test]
    fn lookup_reports_unknown_runtime_state_instead_of_empty_runtime() {
        let topdir = tempdir("runtime-probe-unknown");
        for (outcome, expected_status) in [
            (
                RuntimeProbeOutcome::Unavailable("docker executable not found".to_string()),
                "unavailable",
            ),
            (
                RuntimeProbeOutcome::TimedOut(RUNTIME_PROBE_TIMEOUT),
                "timeout",
            ),
            (
                RuntimeProbeOutcome::Failed("daemon error".to_string()),
                "failed",
            ),
            (
                RuntimeProbeOutcome::Malformed("bad output".to_string()),
                "malformed",
            ),
        ] {
            let snapshot = lookup_build_runtime_with_probe(&topdir, &FakeProbe(outcome))
                .expect("lookup should retain diagnostic snapshot");
            assert_eq!(snapshot.runtime_probe_status, expected_status);
            assert!(snapshot.running_containers.is_empty());
            assert!(snapshot.container_probe_error.is_some());
        }
        let _ = fs::remove_dir_all(&topdir);
    }

    #[cfg(unix)]
    #[test]
    fn production_probe_bounds_a_non_returning_runtime_command() {
        let probe = DockerRuntimeProbe {
            command: "/bin/sleep".into(),
            args: vec!["30".into()],
            timeout: Duration::from_millis(100),
        };
        let started = std::time::Instant::now();
        let outcome = probe.probe();
        assert_eq!(
            outcome,
            RuntimeProbeOutcome::TimedOut(Duration::from_millis(100))
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn production_probe_distinguishes_unavailable_failed_and_malformed_output() {
        let unavailable = DockerRuntimeProbe {
            command: "/missing/docker".into(),
            args: Vec::new(),
            timeout: Duration::from_secs(1),
        };
        assert!(matches!(
            unavailable.probe(),
            RuntimeProbeOutcome::Unavailable(_)
        ));

        let failed = DockerRuntimeProbe {
            command: "/usr/bin/false".into(),
            args: Vec::new(),
            timeout: Duration::from_secs(1),
        };
        assert!(matches!(failed.probe(), RuntimeProbeOutcome::Failed(_)));

        let malformed = DockerRuntimeProbe {
            command: "/usr/bin/printf".into(),
            args: vec!["bad name\tid\tUp 2 seconds\n".into()],
            timeout: Duration::from_secs(1),
        };
        assert!(matches!(
            malformed.probe(),
            RuntimeProbeOutcome::Malformed(_)
        ));

        let healthy_empty = DockerRuntimeProbe {
            command: "/usr/bin/printf".into(),
            args: vec![String::new().into()],
            timeout: Duration::from_secs(1),
        };
        assert_eq!(
            healthy_empty.probe(),
            RuntimeProbeOutcome::Healthy(Vec::new())
        );

        let healthy_active = DockerRuntimeProbe {
            command: "/usr/bin/printf".into(),
            args: vec!["bioconda2rpm-test\tdeadbeef\tUp 2 seconds\n".into()],
            timeout: Duration::from_secs(1),
        };
        assert_eq!(
            healthy_active.probe(),
            RuntimeProbeOutcome::Healthy(vec![RuntimeContainerInfo {
                name: "bioconda2rpm-test".to_string(),
                status: "Up 2 seconds".to_string(),
            }])
        );
    }

    #[test]
    fn acquire_or_forward_build_preserves_manual_source_files() {
        let topdir = tempdir("forward-files");
        let _owner = acquire_test_build(&topdir, "target-a", &["server-root".to_string()])
            .expect("acquire owner");

        let manual_files = vec![
            PathBuf::from("/home/stephen/Downloads/cap3.tar.gz"),
            PathBuf::from("/data/manual/special.zip"),
        ];
        let forwarded = match acquire_or_forward_test_build(
            &topdir,
            "target-a",
            &["cap3".to_string()],
            true,
            false,
            &manual_files,
        )
        .expect("forward build")
        {
            BuildAcquireOutcome::Forwarded(forwarded) => forwarded,
            BuildAcquireOutcome::Owner(_) => panic!("request should be forwarded"),
        };
        assert_eq!(forwarded.queued_packages, vec!["cap3".to_string()]);
        assert_eq!(forwarded.manual_source_files, manual_files);

        let drained = drain_forwarded_build_requests(&topdir, "target-a").expect("drain");
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].package, "cap3");
        assert!(drained[0].force_rebuild);
        assert_eq!(drained[0].manual_source_files, manual_files);

        let _ = fs::remove_dir_all(&topdir);
    }
}
