use crate::priority_specs;
use anyhow::{Context, Result};
use git2::build::{CheckoutBuilder, RepoBuilder};
use git2::{AutotagOption, FetchOptions, ObjectType, Oid, RemoteCallbacks, Repository};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const BIOCONDA_RECIPES_REMOTE: &str = "https://github.com/bioconda/bioconda-recipes.git";

#[derive(Debug, Clone)]
pub struct RecipeRepoRequest {
    pub recipe_root: PathBuf,
    pub recipe_repo_root: PathBuf,
    pub recipe_ref: Option<String>,
    pub sync: bool,
}

#[derive(Debug, Clone)]
pub struct RecipeRepoOutcome {
    pub recipe_root: PathBuf,
    pub recipe_repo_root: PathBuf,
    pub cloned: bool,
    pub fetched: bool,
    pub checked_out: Option<String>,
    pub head: Option<String>,
    pub managed_git: bool,
}

pub fn ensure_recipe_repository(request: &RecipeRepoRequest) -> Result<RecipeRepoOutcome> {
    priority_specs::log_external_progress(format!(
        "phase=recipe-sync status=started action=prepare repo={} recipes={}",
        request.recipe_repo_root.to_string_lossy(),
        request.recipe_root.to_string_lossy()
    ));

    let mut cloned = false;
    if !request.recipe_repo_root.exists() {
        if let Some(parent) = request.recipe_repo_root.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "creating recipe repo parent directory {}",
                    parent.to_string_lossy()
                )
            })?;
        }
        priority_specs::log_external_progress(format!(
            "phase=recipe-sync status=started action=clone remote={} repo={}",
            BIOCONDA_RECIPES_REMOTE,
            request.recipe_repo_root.to_string_lossy()
        ));
        clone_repository(&request.recipe_repo_root).with_context(|| {
            format!(
                "cloning {} into {}",
                BIOCONDA_RECIPES_REMOTE,
                request.recipe_repo_root.to_string_lossy()
            )
        })?;
        cloned = true;
    } else {
        priority_specs::log_external_progress(format!(
            "phase=recipe-sync status=skipped action=clone reason=repo_exists repo={}",
            request.recipe_repo_root.to_string_lossy()
        ));
    }

    let fallback_recipe_root =
        resolve_recipe_root_after_prepare(&request.recipe_root, &request.recipe_repo_root);
    let repo = match Repository::open(&request.recipe_repo_root) {
        Ok(repo) => repo,
        Err(err) => {
            if fallback_recipe_root.exists() && !request.sync && request.recipe_ref.is_none() {
                priority_specs::log_external_progress(format!(
                    "phase=recipe-sync status=completed action=prepare managed_git=false recipes={}",
                    fallback_recipe_root.to_string_lossy()
                ));
                return Ok(RecipeRepoOutcome {
                    recipe_root: fallback_recipe_root,
                    recipe_repo_root: request.recipe_repo_root.clone(),
                    cloned,
                    fetched: false,
                    checked_out: None,
                    head: None,
                    managed_git: false,
                });
            }
            priority_specs::log_external_progress(format!(
                "phase=recipe-sync status=failed action=open-repo repo={} reason={}",
                request.recipe_repo_root.to_string_lossy(),
                sanitize_progress_value(err.message())
            ));
            return Err(err).with_context(|| {
                format!(
                    "opening recipes git repository at {}",
                    request.recipe_repo_root.to_string_lossy()
                )
            });
        }
    };
    priority_specs::log_external_progress(format!(
        "phase=recipe-sync status=running action=open-repo repo={} managed_git=true",
        request.recipe_repo_root.to_string_lossy()
    ));

    let mut fetched = false;
    if request.sync || request.recipe_ref.is_some() {
        fetch_origin(&repo)?;
        fetched = true;
    } else {
        priority_specs::log_external_progress(
            "phase=recipe-sync status=skipped action=fetch reason=not_requested".to_string(),
        );
    }

    let mut checked_out = None;
    if let Some(ref_name) = request.recipe_ref.as_deref() {
        priority_specs::log_external_progress(format!(
            "phase=recipe-sync status=started action=checkout target={}",
            sanitize_progress_value(ref_name)
        ));
        checked_out = Some(checkout_named_ref(&repo, ref_name)?);
        priority_specs::log_external_progress(format!(
            "phase=recipe-sync status=completed action=checkout result={}",
            sanitize_progress_value(checked_out.as_deref().unwrap_or("unknown"))
        ));
    } else if request.sync {
        let default_branch = default_origin_branch_name(&repo)?;
        priority_specs::log_external_progress(format!(
            "phase=recipe-sync status=started action=checkout target={}",
            sanitize_progress_value(&default_branch)
        ));
        checked_out = Some(checkout_named_ref(&repo, &default_branch)?);
        priority_specs::log_external_progress(format!(
            "phase=recipe-sync status=completed action=checkout result={}",
            sanitize_progress_value(checked_out.as_deref().unwrap_or("unknown"))
        ));
    } else {
        priority_specs::log_external_progress(
            "phase=recipe-sync status=skipped action=checkout reason=not_requested".to_string(),
        );
    }

    let recipe_root =
        resolve_recipe_root_after_prepare(&request.recipe_root, &request.recipe_repo_root);
    if !recipe_root.exists() {
        anyhow::bail!(
            "recipes path not found after repository preparation: {}",
            recipe_root.to_string_lossy()
        );
    }

    let head = head_summary(&repo).ok();
    priority_specs::log_external_progress(format!(
        "phase=recipe-sync status=completed action=prepare managed_git=true cloned={} fetched={} checkout={} head={}",
        cloned,
        fetched,
        sanitize_progress_value(checked_out.as_deref().unwrap_or("none")),
        sanitize_progress_value(head.as_deref().unwrap_or("unknown"))
    ));

    Ok(RecipeRepoOutcome {
        recipe_root,
        recipe_repo_root: request.recipe_repo_root.clone(),
        cloned,
        fetched,
        checked_out,
        head,
        managed_git: true,
    })
}

fn resolve_recipe_root_after_prepare(requested_root: &Path, repo_root: &Path) -> PathBuf {
    if requested_root
        .file_name()
        .and_then(|v| v.to_str())
        .map(|v| v == "recipes")
        .unwrap_or(false)
    {
        return requested_root.to_path_buf();
    }
    if requested_root.join("recipes").is_dir() {
        return requested_root.join("recipes");
    }
    if repo_root.join("recipes").is_dir() {
        return repo_root.join("recipes");
    }
    requested_root.to_path_buf()
}

fn fetch_origin(repo: &Repository) -> Result<()> {
    let started = Instant::now();
    priority_specs::log_external_progress(
        "phase=recipe-sync status=started action=fetch remote=origin".to_string(),
    );
    let mut remote = repo
        .find_remote("origin")
        .context("finding origin remote in recipes repository")?;
    let mut fetch_options = FetchOptions::new();
    fetch_options.download_tags(AutotagOption::All);
    fetch_options.remote_callbacks(make_transfer_callbacks("fetch"));
    remote
        .fetch(
            &[
                "refs/heads/*:refs/remotes/origin/*",
                "refs/tags/*:refs/tags/*",
            ],
            Some(&mut fetch_options),
            None,
        )
        .context("fetching origin refs for recipes repository")?;
    priority_specs::log_external_progress(format!(
        "phase=recipe-sync status=completed action=fetch remote=origin elapsed={}",
        format_elapsed(started.elapsed())
    ));
    Ok(())
}

fn default_origin_branch_name(repo: &Repository) -> Result<String> {
    if let Ok(origin_head) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Some(symbolic) = origin_head.symbolic_target() {
            if let Some(branch) = symbolic.strip_prefix("refs/remotes/origin/") {
                return Ok(branch.to_string());
            }
        }
    }
    for candidate in ["main", "master"] {
        if repo
            .find_reference(&format!("refs/remotes/origin/{candidate}"))
            .is_ok()
        {
            return Ok(candidate.to_string());
        }
    }
    if let Ok(head) = repo.head() {
        if let Some(name) = head.shorthand() {
            return Ok(name.to_string());
        }
    }
    anyhow::bail!("unable to determine default branch for recipes repository");
}

fn checkout_named_ref(repo: &Repository, name: &str) -> Result<String> {
    let remote_ref_name = format!("refs/remotes/origin/{name}");
    if let Ok(remote_ref) = repo.find_reference(&remote_ref_name) {
        let commit = remote_ref
            .peel_to_commit()
            .with_context(|| format!("peeling remote branch origin/{name}"))?;
        upsert_local_branch(repo, name, commit.id())?;
        checkout_local_branch(repo, name)?;
        return Ok(format!("branch:{name}"));
    }

    let local_ref_name = format!("refs/heads/{name}");
    if repo.find_reference(&local_ref_name).is_ok() {
        checkout_local_branch(repo, name)?;
        return Ok(format!("branch:{name}"));
    }

    let tag_ref_name = format!("refs/tags/{name}");
    if let Ok(tag_ref) = repo.find_reference(&tag_ref_name) {
        let tag_obj = tag_ref
            .peel(ObjectType::Commit)
            .with_context(|| format!("peeling tag {name} to commit"))?;
        checkout_detached(repo, &tag_obj)?;
        return Ok(format!("tag:{name}"));
    }

    let obj = repo
        .revparse_single(name)
        .with_context(|| format!("resolving ref '{name}'"))?;
    let commit_obj = obj
        .peel(ObjectType::Commit)
        .with_context(|| format!("peeling '{name}' to commit"))?;
    checkout_detached(repo, &commit_obj)?;
    Ok(format!("rev:{name}"))
}

fn upsert_local_branch(repo: &Repository, name: &str, target: Oid) -> Result<()> {
    let local_ref_name = format!("refs/heads/{name}");
    if let Ok(mut local_ref) = repo.find_reference(&local_ref_name) {
        local_ref
            .set_target(target, "bioconda2rpm sync recipes branch")
            .with_context(|| format!("updating local branch {name}"))?;
        return Ok(());
    }
    let commit = repo
        .find_commit(target)
        .with_context(|| format!("finding target commit for branch {name}"))?;
    repo.branch(name, &commit, false)
        .with_context(|| format!("creating local branch {name}"))?;
    Ok(())
}

fn checkout_local_branch(repo: &Repository, name: &str) -> Result<()> {
    repo.set_head(&format!("refs/heads/{name}"))
        .with_context(|| format!("setting HEAD to local branch {name}"))?;
    let mut checkout = CheckoutBuilder::new();
    checkout.safe();
    repo.checkout_head(Some(&mut checkout))
        .with_context(|| format!("checking out local branch {name}"))?;
    Ok(())
}

fn checkout_detached(repo: &Repository, obj: &git2::Object<'_>) -> Result<()> {
    let mut checkout = CheckoutBuilder::new();
    checkout.safe();
    repo.checkout_tree(obj, Some(&mut checkout))
        .context("checking out detached commit tree")?;
    let commit = obj
        .peel_to_commit()
        .context("resolving detached checkout commit")?;
    repo.set_head_detached(commit.id())
        .context("setting detached HEAD")?;
    Ok(())
}

fn head_summary(repo: &Repository) -> Result<String> {
    let head = repo.head().context("reading repository HEAD")?;
    let commit = head
        .peel_to_commit()
        .context("resolving repository HEAD commit")?;
    let short = short_oid(commit.id());
    let mode = if head.is_branch() {
        format!("branch:{}", head.shorthand().unwrap_or("unknown"))
    } else {
        "detached".to_string()
    };
    Ok(format!("{mode}@{short}"))
}

fn short_oid(oid: Oid) -> String {
    let s = oid.to_string();
    s.chars().take(12).collect()
}

fn clone_repository(repo_root: &Path) -> Result<()> {
    let started = Instant::now();
    let mut fetch_options = FetchOptions::new();
    fetch_options.download_tags(AutotagOption::All);
    fetch_options.remote_callbacks(make_transfer_callbacks("clone"));
    let mut builder = RepoBuilder::new();
    builder.fetch_options(fetch_options);
    builder
        .clone(BIOCONDA_RECIPES_REMOTE, repo_root)
        .context("running git clone for recipes repository")?;
    priority_specs::log_external_progress(format!(
        "phase=recipe-sync status=completed action=clone elapsed={}",
        format_elapsed(started.elapsed())
    ));
    Ok(())
}

fn make_transfer_callbacks(action: &'static str) -> RemoteCallbacks<'static> {
    let mut callbacks = RemoteCallbacks::new();
    let started = Instant::now();
    let mut last_emit = Instant::now()
        .checked_sub(Duration::from_secs(3))
        .unwrap_or_else(Instant::now);
    let mut completion_reported = false;
    callbacks.transfer_progress(move |stats| {
        let now = Instant::now();
        let total = stats.total_objects();
        let received = stats.received_objects();
        let bytes = stats.received_bytes();
        let done = total > 0 && received >= total;
        let periodic = now.duration_since(last_emit) >= Duration::from_secs(2);
        let should_emit = (!done && periodic) || (done && !completion_reported);
        if should_emit {
            let percent = if total > 0 {
                received.saturating_mul(100) / total
            } else {
                0
            };
            priority_specs::log_external_progress(format!(
                "phase=recipe-sync status=running action={} objects={}/{} percent={} bytes={} elapsed={}",
                action,
                received,
                total,
                percent,
                bytes,
                format_elapsed(started.elapsed())
            ));
            last_emit = now;
            if done {
                completion_reported = true;
            }
        }
        true
    });
    callbacks
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

fn sanitize_progress_value(raw: impl AsRef<str>) -> String {
    raw.as_ref()
        .chars()
        .map(|c| match c {
            '=' | ' ' | '\n' | '\r' | '\t' | '|' => '_',
            _ => c,
        })
        .collect()
}
