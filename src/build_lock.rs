use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

const LOCK_FILE_NAME: &str = ".bioconda2rpm-artifacts.lock";
const STATE_FILE_NAME: &str = ".bioconda2rpm-active-builds.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActiveBuildEntry {
    pid: u32,
    target_id: String,
    packages: Vec<String>,
    started_at_utc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ActiveBuildState {
    entries: Vec<ActiveBuildEntry>,
}

pub struct BuildSessionGuard {
    lock_file: fs::File,
    state_file: PathBuf,
    pid: u32,
}

impl BuildSessionGuard {
    pub fn acquire(topdir: &Path, target_id: &str, packages: &[String]) -> Result<Self> {
        fs::create_dir_all(topdir)
            .with_context(|| format!("creating topdir {}", topdir.to_string_lossy()))?;

        let lock_path = topdir.join(LOCK_FILE_NAME);
        let state_file = topdir.join(STATE_FILE_NAME);
        let mut lock_file = fs::OpenOptions::new()
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
                            "pid={} target={} packages={}",
                            entry.pid,
                            entry.target_id,
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

        let pid = std::process::id();
        let entry = ActiveBuildEntry {
            pid,
            target_id: target_id.to_string(),
            packages: packages.to_vec(),
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
            pid,
        })
    }
}

impl Drop for BuildSessionGuard {
    fn drop(&mut self) {
        let mut state = load_state(&self.state_file).unwrap_or_default();
        state.entries.retain(|entry| entry.pid != self.pid);
        if state.entries.is_empty() {
            let _ = fs::remove_file(&self.state_file);
        } else {
            let _ = write_state(&self.state_file, &state);
        }
        let _ = self.lock_file.unlock();
    }
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
