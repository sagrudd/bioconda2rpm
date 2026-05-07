mod build_lock;
mod cli;
mod list;
mod priority_specs;
mod recipe_repo;
mod ui;

use clap::Parser;
use std::fs;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

static SIGNAL_HANDLER_INSTALLED: OnceLock<()> = OnceLock::new();

fn package_list_summary(packages: &[String]) -> String {
    const LIMIT: usize = 24;
    if packages.len() <= LIMIT {
        return packages.join(",");
    }
    let preview = packages
        .iter()
        .take(LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    format!("{preview},...(+{} more)", packages.len() - LIMIT)
}

fn ensure_workspace_paths(
    topdir: &std::path::Path,
    bad_spec: &std::path::Path,
    reports: &std::path::Path,
) -> std::io::Result<()> {
    fs::create_dir_all(topdir)?;
    fs::create_dir_all(bad_spec)?;
    fs::create_dir_all(reports)?;
    Ok(())
}

fn install_signal_handler() {
    let _ = SIGNAL_HANDLER_INSTALLED.get_or_init(|| {
        if let Err(err) = ctrlc::set_handler(|| {
            priority_specs::request_cancellation("cancelled by user (SIGINT)");
        }) {
            eprintln!("warning: failed to install Ctrl-C handler: {err}");
        }
    });
}

fn collect_initial_server_packages(args: &cli::BuildArgs) -> anyhow::Result<Vec<String>> {
    if args.packages.is_empty() && args.packages_file.is_none() {
        return Ok(Vec::new());
    }
    priority_specs::collect_requested_build_packages(args)
}

fn update_server_status_packages(
    status: &Arc<Mutex<(Vec<String>, Vec<String>)>>,
    pending: &[(String, bool, bool)],
    current: &[String],
) {
    if let Ok(mut guard) = status.lock() {
        guard.0 = pending
            .iter()
            .map(|(package, _, _)| package.clone())
            .collect();
        guard.1 = current.to_vec();
    }
}

fn main() -> ExitCode {
    install_signal_handler();
    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Build(mut args) => {
            priority_specs::reset_cancellation();
            let topdir = args.effective_topdir();
            let bad_spec = args.effective_bad_spec_dir();
            let reports = args.effective_reports_dir();
            if let Err(err) = ensure_workspace_paths(&topdir, &bad_spec, &reports) {
                eprintln!("failed to prepare workspace directories: {err}");
                return ExitCode::FAILURE;
            }
            let ui_mode = args.effective_ui_mode();
            let mut progress_ui = if ui_mode == cli::UiMode::Ratatui {
                let title = format!("bioconda2rpm build ({})", args.effective_target_id());
                let ui = ui::ProgressUi::start(title);
                priority_specs::install_progress_sink(ui.sink());
                Some(ui)
            } else {
                None
            };
            if progress_ui.is_none() {
                println!("{}", args.execution_summary());
            }
            let requested_packages = match priority_specs::collect_requested_build_packages(&args) {
                Ok(packages) => packages,
                Err(err) => {
                    priority_specs::clear_progress_sink();
                    if let Some(ui) = progress_ui.take() {
                        ui.finish(format!("build failed: package selection error: {err}"));
                    }
                    eprintln!("failed to determine requested packages: {err:#}");
                    return ExitCode::FAILURE;
                }
            };
            let acquire_outcome = if args.files.is_empty() {
                build_lock::BuildSessionGuard::acquire_or_forward_build(
                    &topdir,
                    &args.effective_target_id(),
                    &requested_packages,
                    args.force,
                    args.refresh_files,
                )
            } else {
                build_lock::BuildSessionGuard::acquire(
                    &topdir,
                    &args.effective_target_id(),
                    &requested_packages,
                    build_lock::BuildSessionKind::Build,
                    args.force,
                    args.refresh_files,
                )
                .map(build_lock::BuildAcquireOutcome::Owner)
            };
            let _build_session = match acquire_outcome {
                Ok(build_lock::BuildAcquireOutcome::Owner(guard)) => {
                    priority_specs::log_external_progress(format!(
                        "phase=workspace-lock status=acquired topdir={} target_id={} packages={}",
                        topdir.display(),
                        args.effective_target_id(),
                        package_list_summary(&requested_packages)
                    ));
                    guard
                }
                Ok(build_lock::BuildAcquireOutcome::Forwarded(forwarded)) => {
                    priority_specs::log_external_progress(format!(
                        "phase=workspace-lock status=forwarded owner_pid={} target_id={} owner_force={} owner_refresh_files={} packages={}",
                        forwarded.owner_pid,
                        forwarded.owner_target_id,
                        forwarded.owner_force_rebuild,
                        forwarded.owner_refresh_files,
                        package_list_summary(&forwarded.queued_packages)
                    ));
                    priority_specs::clear_progress_sink();
                    if let Some(ui) = progress_ui.take() {
                        ui.finish(format!(
                            "request forwarded to active build session (owner pid={}, packages={})",
                            forwarded.owner_pid,
                            package_list_summary(&forwarded.queued_packages)
                        ));
                    }
                    println!(
                        "forwarded build request to active session owner_pid={} target_id={} owner_force={} owner_refresh_files={} packages={}",
                        forwarded.owner_pid,
                        forwarded.owner_target_id,
                        forwarded.owner_force_rebuild,
                        forwarded.owner_refresh_files,
                        package_list_summary(&forwarded.queued_packages)
                    );
                    return ExitCode::SUCCESS;
                }
                Err(err) => {
                    priority_specs::clear_progress_sink();
                    if let Some(ui) = progress_ui.take() {
                        ui.finish(format!("build failed: workspace lock error: {err}"));
                    }
                    eprintln!("failed to acquire workspace build session lock: {err:#}");
                    return ExitCode::FAILURE;
                }
            };

            let recipe_request = recipe_repo::RecipeRepoRequest {
                recipe_root: args.effective_recipe_root(),
                recipe_repo_root: args.effective_recipe_repo_root(),
                recipe_ref: args.recipe_ref.clone(),
                sync: args.effective_recipe_sync(),
            };
            let recipes = match recipe_repo::ensure_recipe_repository(&recipe_request) {
                Ok(state) => state,
                Err(err) => {
                    priority_specs::clear_progress_sink();
                    if let Some(ui) = progress_ui.take() {
                        ui.finish(format!("build failed: recipe sync error: {err}"));
                    }
                    eprintln!("failed to prepare bioconda recipes repository: {err:#}");
                    return ExitCode::FAILURE;
                }
            };
            args.recipe_root = Some(recipes.recipe_root.clone());
            priority_specs::log_external_progress(format!(
                "phase=recipe-sync status=ready action=prepared recipes={} repo={} managed_git={} cloned={} fetched={} checkout={} head={}",
                recipes.recipe_root.display(),
                recipes.recipe_repo_root.display(),
                recipes.managed_git,
                recipes.cloned,
                recipes.fetched,
                recipes.checked_out.as_deref().unwrap_or("none"),
                recipes.head.as_deref().unwrap_or("unknown")
            ));

            let outcome = priority_specs::run_build(&args);
            priority_specs::clear_progress_sink();

            if let Some(ui) = progress_ui.take() {
                let summary = match &outcome {
                    Ok(summary) => format!(
                        "build completed requested={} generated={} up_to_date={} skipped={} quarantined={} kpi={:.2}%",
                        summary.requested,
                        summary.generated,
                        summary.up_to_date,
                        summary.skipped,
                        summary.quarantined,
                        summary.kpi_success_rate
                    ),
                    Err(err) => format!("build failed: {}", err),
                };
                ui.finish(summary);
            }

            match outcome {
                Ok(summary) => {
                    println!(
                        "build requested={} generated={} up_to_date={} skipped={} quarantined={} kpi_scope_entries={} kpi_excluded_arch={} kpi_denominator={} kpi_successes={} kpi_success_rate={:.2}% order={} report_json={} report_csv={} report_md={}",
                        summary.requested,
                        summary.generated,
                        summary.up_to_date,
                        summary.skipped,
                        summary.quarantined,
                        summary.kpi_scope_entries,
                        summary.kpi_excluded_arch,
                        summary.kpi_denominator,
                        summary.kpi_successes,
                        summary.kpi_success_rate,
                        summary.build_order.join("->"),
                        summary.report_json.display(),
                        summary.report_csv.display(),
                        summary.report_md.display()
                    );
                    if summary.generated == 0
                        && summary.up_to_date >= 1
                        && summary.quarantined == 0
                        && summary.skipped == 0
                    {
                        println!("package is already up-to-date");
                    }
                }
                Err(err) => {
                    eprintln!("build failed: {err:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        cli::Command::Server(server_args) => {
            priority_specs::reset_cancellation();
            let mut args = server_args.build;
            let topdir = args.effective_topdir();
            let target_id = args.effective_target_id();
            if server_args.status {
                match build_lock::lookup_build_runtime(&topdir) {
                    Ok(snapshot) => {
                        let rendered = if server_args.compact {
                            serde_json::to_string(&snapshot)
                        } else {
                            serde_json::to_string_pretty(&snapshot)
                        };
                        match rendered {
                            Ok(body) => println!("{body}"),
                            Err(err) => {
                                eprintln!("server status serialization failed: {err:#}");
                                return ExitCode::FAILURE;
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("server status failed: {err:#}");
                        return ExitCode::FAILURE;
                    }
                }
                return ExitCode::SUCCESS;
            }
            if server_args.close {
                if let Err(err) = ensure_workspace_paths(
                    &topdir,
                    &args.effective_bad_spec_dir(),
                    &args.effective_reports_dir(),
                ) {
                    eprintln!("failed to prepare workspace directories: {err}");
                    return ExitCode::FAILURE;
                }
                if let Err(err) = build_lock::append_server_control_request(
                    &topdir,
                    &target_id,
                    build_lock::ServerControlAction::Close,
                    "operator requested server close after queue drain",
                ) {
                    eprintln!("failed to submit server close request: {err:#}");
                    return ExitCode::FAILURE;
                }
                println!("server close requested target_id={target_id}");
                return ExitCode::SUCCESS;
            }
            if server_args.kill {
                if let Err(err) = ensure_workspace_paths(
                    &topdir,
                    &args.effective_bad_spec_dir(),
                    &args.effective_reports_dir(),
                ) {
                    eprintln!("failed to prepare workspace directories: {err}");
                    return ExitCode::FAILURE;
                }
                if let Err(err) = build_lock::append_server_control_request(
                    &topdir,
                    &target_id,
                    build_lock::ServerControlAction::Kill,
                    "operator requested immediate server kill",
                ) {
                    eprintln!("failed to submit server kill request: {err:#}");
                    return ExitCode::FAILURE;
                }
                let stopped = match build_lock::stop_all_build_containers(&args.container_engine) {
                    Ok(names) => names,
                    Err(err) => {
                        eprintln!(
                            "warning: submitted server kill request but failed to stop build containers immediately: {err:#}"
                        );
                        Vec::new()
                    }
                };
                println!(
                    "server kill requested target_id={} stopped_containers={}",
                    target_id,
                    stopped.join(",")
                );
                return ExitCode::SUCCESS;
            }
            if args.force {
                eprintln!(
                    "server does not accept --force; start the server without --force and submit forced work with `bioconda2rpm build --force <package>`"
                );
                return ExitCode::FAILURE;
            }
            if args.refresh_files {
                eprintln!(
                    "server does not accept --refresh-files; start the server without --refresh-files and submit refreshed work with `bioconda2rpm build --refresh-files <package>`"
                );
                return ExitCode::FAILURE;
            }
            if !args.files.is_empty() {
                eprintln!(
                    "server does not accept --files; run `bioconda2rpm build --files <file> <package>` when the manual source file is needed"
                );
                return ExitCode::FAILURE;
            }
            let bad_spec = args.effective_bad_spec_dir();
            let reports = args.effective_reports_dir();
            if let Err(err) = ensure_workspace_paths(&topdir, &bad_spec, &reports) {
                eprintln!("failed to prepare workspace directories: {err}");
                return ExitCode::FAILURE;
            }

            let server_status_packages =
                Arc::new(Mutex::new((Vec::<String>::new(), Vec::<String>::new())));
            let mut progress_ui = {
                let title = format!("bioconda2rpm server ({})", args.effective_target_id());
                let ui = ui::ProgressUi::start(title);
                let ui_sink = ui.sink();
                let status_topdir = topdir.clone();
                let status_target_id = target_id.clone();
                let status_packages_for_sink = Arc::clone(&server_status_packages);
                priority_specs::install_progress_sink(Arc::new(move |line: String| {
                    let (pending, current) = status_packages_for_sink
                        .lock()
                        .map(|guard| (guard.0.clone(), guard.1.clone()))
                        .unwrap_or_default();
                    let _ = build_lock::record_server_progress(
                        &status_topdir,
                        &status_target_id,
                        &pending,
                        &current,
                        &line,
                    );
                    ui_sink(line);
                }));
                let _ = build_lock::record_server_progress(
                    &topdir,
                    &target_id,
                    &[],
                    &[],
                    "phase=server status=starting",
                );
                Some(ui)
            };

            let initial_packages = match collect_initial_server_packages(&args) {
                Ok(packages) => packages,
                Err(err) => {
                    priority_specs::clear_progress_sink();
                    if let Some(ui) = progress_ui.take() {
                        ui.finish(format!("server failed: package selection error: {err}"));
                    }
                    eprintln!("failed to determine initial server packages: {err:#}");
                    return ExitCode::FAILURE;
                }
            };
            let owner_force = false;
            let mut pending_packages = initial_packages
                .iter()
                .cloned()
                .map(|package| (package, owner_force, false))
                .collect::<Vec<_>>();
            update_server_status_packages(&server_status_packages, &pending_packages, &[]);

            let _server_session = match build_lock::BuildSessionGuard::acquire(
                &topdir,
                &args.effective_target_id(),
                &initial_packages,
                build_lock::BuildSessionKind::Build,
                false,
                false,
            ) {
                Ok(guard) => {
                    priority_specs::log_external_progress(format!(
                        "phase=workspace-lock status=server-acquired topdir={} target_id={} initial_packages={}",
                        topdir.display(),
                        args.effective_target_id(),
                        package_list_summary(&initial_packages)
                    ));
                    guard
                }
                Err(err) => {
                    priority_specs::clear_progress_sink();
                    if let Some(ui) = progress_ui.take() {
                        ui.finish(format!("server failed: workspace lock error: {err}"));
                    }
                    eprintln!("failed to acquire persistent build server lock: {err:#}");
                    return ExitCode::FAILURE;
                }
            };

            let recipe_request = recipe_repo::RecipeRepoRequest {
                recipe_root: args.effective_recipe_root(),
                recipe_repo_root: args.effective_recipe_repo_root(),
                recipe_ref: args.recipe_ref.clone(),
                sync: args.effective_recipe_sync(),
            };
            let recipes = match recipe_repo::ensure_recipe_repository(&recipe_request) {
                Ok(state) => state,
                Err(err) => {
                    priority_specs::clear_progress_sink();
                    if let Some(ui) = progress_ui.take() {
                        ui.finish(format!("server failed: recipe sync error: {err}"));
                    }
                    eprintln!("failed to prepare bioconda recipes repository: {err:#}");
                    return ExitCode::FAILURE;
                }
            };
            args.recipe_root = Some(recipes.recipe_root.clone());
            args.packages_file = None;
            priority_specs::log_external_progress(format!(
                "phase=recipe-sync status=ready action=prepared recipes={} repo={} managed_git={} cloned={} fetched={} checkout={} head={}",
                recipes.recipe_root.display(),
                recipes.recipe_repo_root.display(),
                recipes.managed_git,
                recipes.cloned,
                recipes.fetched,
                recipes.checked_out.as_deref().unwrap_or("none"),
                recipes.head.as_deref().unwrap_or("unknown")
            ));

            let mut closing = false;
            let mut server_stop_reason = "server stopped by user".to_string();
            while !priority_specs::cancellation_requested_public() {
                if !closing {
                    match build_lock::drain_forwarded_build_requests(
                        topdir.as_path(),
                        &args.effective_target_id(),
                    ) {
                        Ok(forwarded) => {
                            for request in forwarded {
                                pending_packages.push((
                                    request.package,
                                    request.force_rebuild,
                                    request.refresh_files,
                                ));
                            }
                            update_server_status_packages(
                                &server_status_packages,
                                &pending_packages,
                                &[],
                            );
                        }
                        Err(err) => {
                            priority_specs::log_external_progress(format!(
                                "phase=server status=queue-drain-error target_id={} detail={}",
                                args.effective_target_id(),
                                err
                            ));
                        }
                    }
                }

                match build_lock::drain_server_control_requests(
                    topdir.as_path(),
                    &args.effective_target_id(),
                ) {
                    Ok(control_requests) => {
                        for request in control_requests {
                            match request.action {
                                build_lock::ServerControlAction::Close => {
                                    closing = true;
                                    if let Err(err) =
                                        build_lock::mark_server_closing(&topdir, &target_id)
                                    {
                                        priority_specs::log_external_progress(format!(
                                            "phase=server-control status=close-marker-error target_id={} detail={}",
                                            args.effective_target_id(),
                                            err
                                        ));
                                    }
                                    server_stop_reason =
                                        "server closed after queue drain".to_string();
                                    priority_specs::log_external_progress(format!(
                                        "phase=server-control status=close-received target_id={} submit_host={} submit_pid={} submit_ts={} reason={}",
                                        request.target_id,
                                        request.submitted_host,
                                        request.submitted_pid,
                                        request.submitted_at_utc,
                                        request.reason
                                    ));
                                }
                                build_lock::ServerControlAction::Kill => {
                                    if let Err(err) =
                                        build_lock::mark_server_closing(&topdir, &target_id)
                                    {
                                        priority_specs::log_external_progress(format!(
                                            "phase=server-control status=kill-marker-error target_id={} detail={}",
                                            args.effective_target_id(),
                                            err
                                        ));
                                    }
                                    server_stop_reason = "server killed by operator".to_string();
                                    priority_specs::log_external_progress(format!(
                                        "phase=server-control status=kill-received target_id={} submit_host={} submit_pid={} submit_ts={} reason={}",
                                        request.target_id,
                                        request.submitted_host,
                                        request.submitted_pid,
                                        request.submitted_at_utc,
                                        request.reason
                                    ));
                                    priority_specs::request_cancellation(
                                        "server kill requested by operator",
                                    );
                                    if let Err(err) = build_lock::stop_all_build_containers(
                                        &args.container_engine,
                                    ) {
                                        priority_specs::log_external_progress(format!(
                                            "phase=server-control status=kill-container-stop-error target_id={} detail={}",
                                            args.effective_target_id(),
                                            err
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    Err(err) => {
                        priority_specs::log_external_progress(format!(
                            "phase=server-control status=drain-error target_id={} detail={}",
                            args.effective_target_id(),
                            err
                        ));
                    }
                }
                if priority_specs::cancellation_requested_public() {
                    break;
                }

                pending_packages.sort_by(|a, b| a.0.cmp(&b.0));
                let mut merged_packages: Vec<(String, bool, bool)> = Vec::new();
                for (package, force, refresh_files) in std::mem::take(&mut pending_packages) {
                    if let Some((_, existing_force, existing_refresh_files)) = merged_packages
                        .iter_mut()
                        .find(|(existing, _, _)| existing == &package)
                    {
                        *existing_force |= force;
                        *existing_refresh_files |= refresh_files;
                    } else {
                        merged_packages.push((package, force, refresh_files));
                    }
                }
                pending_packages = merged_packages;
                update_server_status_packages(&server_status_packages, &pending_packages, &[]);
                if pending_packages.is_empty() {
                    if closing {
                        priority_specs::log_external_progress(format!(
                            "phase=server status=closed target_id={} detail=queue-drained",
                            args.effective_target_id()
                        ));
                        break;
                    }
                    priority_specs::log_external_progress(format!(
                        "phase=server status=waiting target_id={} detail=idle-until-forwarded-build-or-ctrl-c",
                        args.effective_target_id()
                    ));
                    thread::sleep(Duration::from_millis(750));
                    continue;
                }

                let batch_items = std::mem::take(&mut pending_packages);
                let mut normal_packages = batch_items
                    .iter()
                    .filter_map(|(package, force, refresh_files)| {
                        if *force || *refresh_files {
                            None
                        } else {
                            Some(package.clone())
                        }
                    })
                    .collect::<Vec<_>>();
                let mut forced_packages = batch_items
                    .iter()
                    .filter_map(|(package, force, refresh_files)| {
                        if *force && !*refresh_files {
                            Some(package.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                let mut refresh_packages = batch_items
                    .iter()
                    .filter_map(|(package, force, refresh_files)| {
                        if !*force && *refresh_files {
                            Some(package.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                let mut forced_refresh_packages = batch_items
                    .into_iter()
                    .filter_map(|(package, force, refresh_files)| {
                        if force && refresh_files {
                            Some(package)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                for (force, refresh_files, packages) in [
                    (false, false, &mut normal_packages),
                    (true, false, &mut forced_packages),
                    (false, true, &mut refresh_packages),
                    (true, true, &mut forced_refresh_packages),
                ] {
                    if packages.is_empty() {
                        continue;
                    }
                    packages.sort();
                    packages.dedup();
                    args.packages = packages.clone();
                    args.force = force;
                    args.refresh_files = refresh_files;
                    update_server_status_packages(&server_status_packages, &[], packages);
                    priority_specs::log_external_progress(format!(
                        "phase=server status=build-start target_id={} force={} refresh_files={} packages={}",
                        args.effective_target_id(),
                        args.force,
                        args.refresh_files,
                        package_list_summary(&args.packages)
                    ));
                    let outcome = priority_specs::run_build(&args);
                    update_server_status_packages(&server_status_packages, &[], &[]);
                    args.force = false;
                    args.refresh_files = false;
                    match outcome {
                        Ok(summary) => {
                            priority_specs::log_external_progress(format!(
                                "phase=server status=build-completed requested={} generated={} up_to_date={} skipped={} quarantined={} kpi_success_rate={:.2}",
                                summary.requested,
                                summary.generated,
                                summary.up_to_date,
                                summary.skipped,
                                summary.quarantined,
                                summary.kpi_success_rate
                            ));
                        }
                        Err(err) => {
                            if priority_specs::cancellation_requested_public() {
                                break;
                            }
                            priority_specs::log_external_progress(format!(
                                "phase=server status=build-error target_id={} force={} refresh_files={} detail={}",
                                args.effective_target_id(),
                                force,
                                refresh_files,
                                err
                            ));
                        }
                    }
                }
                args.packages.clear();
            }

            priority_specs::clear_progress_sink();
            if let Some(ui) = progress_ui.take() {
                ui.finish(server_stop_reason);
            }
        }
        cli::Command::Remove(args) => {
            let topdir = args.effective_topdir();
            let target_id = args.effective_target_id();
            let queue_summary = match build_lock::remove_queued_build_requests(
                &topdir,
                Some(&target_id),
                &args.packages,
            ) {
                Ok(summary) => summary,
                Err(err) => {
                    eprintln!("failed to remove pending queued requests: {err:#}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(err) =
                build_lock::append_remove_request(&topdir, &target_id, &args.packages, &args.reason)
            {
                eprintln!("failed to submit active remove request: {err:#}");
                return ExitCode::FAILURE;
            }

            let mut stopped = Vec::new();
            if !args.queue_only {
                match build_lock::stop_matching_build_containers(
                    &args.container_engine,
                    &args.packages,
                ) {
                    Ok(names) => stopped = names,
                    Err(err) => {
                        eprintln!("warning: failed to stop matching build containers: {err:#}")
                    }
                }
            }
            println!(
                "remove target_id={} packages={} queued_removed={} retained_queue_requests={} active_remove_request=submitted stopped_containers={}",
                target_id,
                package_list_summary(&args.packages),
                queue_summary.removed_packages,
                queue_summary.retained_requests,
                stopped.join(",")
            );
        }
        cli::Command::GeneratePrioritySpecs(mut args) => {
            priority_specs::reset_cancellation();
            let topdir = args.effective_topdir();
            let bad_spec = args.effective_bad_spec_dir();
            let reports = args.effective_reports_dir();
            if let Err(err) = ensure_workspace_paths(&topdir, &bad_spec, &reports) {
                eprintln!("failed to prepare workspace directories: {err}");
                return ExitCode::FAILURE;
            }
            let _build_session = match build_lock::BuildSessionGuard::acquire(
                &topdir,
                &args.effective_target_id(),
                &[format!(
                    "generate-priority-specs:{}",
                    args.tools_csv.to_string_lossy()
                )],
                build_lock::BuildSessionKind::GeneratePrioritySpecs,
                false,
                false,
            ) {
                Ok(guard) => guard,
                Err(err) => {
                    eprintln!("failed to acquire workspace build session lock: {err:#}");
                    return ExitCode::FAILURE;
                }
            };
            let recipe_request = recipe_repo::RecipeRepoRequest {
                recipe_root: args.effective_recipe_root(),
                recipe_repo_root: args.effective_recipe_repo_root(),
                recipe_ref: args.recipe_ref.clone(),
                sync: args.effective_recipe_sync(),
            };
            let recipes = match recipe_repo::ensure_recipe_repository(&recipe_request) {
                Ok(state) => state,
                Err(err) => {
                    eprintln!("failed to prepare bioconda recipes repository: {err:#}");
                    return ExitCode::FAILURE;
                }
            };
            args.recipe_root = Some(recipes.recipe_root.clone());
            println!(
                "recipes root={} repo={} managed_git={} cloned={} fetched={} checkout={} head={}",
                recipes.recipe_root.display(),
                recipes.recipe_repo_root.display(),
                recipes.managed_git,
                recipes.cloned,
                recipes.fetched,
                recipes.checked_out.as_deref().unwrap_or("none"),
                recipes.head.as_deref().unwrap_or("unknown")
            );

            match priority_specs::run_generate_priority_specs(&args) {
                Ok(summary) => {
                    println!(
                        "priority spec generation requested={} generated={} quarantined={} report_json={} report_csv={} report_md={}",
                        summary.requested,
                        summary.generated,
                        summary.quarantined,
                        summary.report_json.display(),
                        summary.report_csv.display(),
                        summary.report_md.display(),
                    );
                }
                Err(err) => {
                    eprintln!("priority spec generation failed: {err:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        cli::Command::Regression(mut args) => {
            let topdir = args.effective_topdir();
            let bad_spec = args.effective_bad_spec_dir();
            let reports = args.effective_reports_dir();
            if let Err(err) = ensure_workspace_paths(&topdir, &bad_spec, &reports) {
                eprintln!("failed to prepare workspace directories: {err}");
                return ExitCode::FAILURE;
            }
            let _build_session = match build_lock::BuildSessionGuard::acquire(
                &topdir,
                &args.effective_target_id(),
                &[format!("regression:{:?}", args.mode)],
                build_lock::BuildSessionKind::Regression,
                false,
                false,
            ) {
                Ok(guard) => guard,
                Err(err) => {
                    eprintln!("failed to acquire workspace build session lock: {err:#}");
                    return ExitCode::FAILURE;
                }
            };
            let recipe_request = recipe_repo::RecipeRepoRequest {
                recipe_root: args.effective_recipe_root(),
                recipe_repo_root: args.effective_recipe_repo_root(),
                recipe_ref: args.recipe_ref.clone(),
                sync: args.effective_recipe_sync(),
            };
            let recipes = match recipe_repo::ensure_recipe_repository(&recipe_request) {
                Ok(state) => state,
                Err(err) => {
                    eprintln!("failed to prepare bioconda recipes repository: {err:#}");
                    return ExitCode::FAILURE;
                }
            };
            args.recipe_root = Some(recipes.recipe_root.clone());
            println!(
                "recipes root={} repo={} managed_git={} cloned={} fetched={} checkout={} head={}",
                recipes.recipe_root.display(),
                recipes.recipe_repo_root.display(),
                recipes.managed_git,
                recipes.cloned,
                recipes.fetched,
                recipes.checked_out.as_deref().unwrap_or("none"),
                recipes.head.as_deref().unwrap_or("unknown")
            );

            match priority_specs::run_regression(&args) {
                Ok(summary) => {
                    println!(
                        "regression mode={:?} requested={} attempted={} succeeded={} failed={} excluded={} kpi_denominator={} kpi_successes={} kpi_success_rate={:.2}% report_json={} report_csv={} report_md={}",
                        summary.mode,
                        summary.requested,
                        summary.attempted,
                        summary.succeeded,
                        summary.failed,
                        summary.excluded,
                        summary.kpi_denominator,
                        summary.kpi_successes,
                        summary.kpi_success_rate,
                        summary.report_json.display(),
                        summary.report_csv.display(),
                        summary.report_md.display(),
                    );
                }
                Err(err) => {
                    eprintln!("regression failed: {err:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        cli::Command::Recipes(args) => {
            let topdir = args.effective_topdir();
            if let Err(err) = fs::create_dir_all(&topdir) {
                eprintln!(
                    "failed to prepare workspace directory {}: {err}",
                    topdir.display()
                );
                return ExitCode::FAILURE;
            }
            let recipe_request = recipe_repo::RecipeRepoRequest {
                recipe_root: args.effective_recipe_root(),
                recipe_repo_root: args.effective_recipe_repo_root(),
                recipe_ref: args.recipe_ref.clone(),
                sync: args.effective_recipe_sync(),
            };
            match recipe_repo::ensure_recipe_repository(&recipe_request) {
                Ok(state) => {
                    println!(
                        "recipes root={} repo={} managed_git={} cloned={} fetched={} checkout={} head={}",
                        state.recipe_root.display(),
                        state.recipe_repo_root.display(),
                        state.managed_git,
                        state.cloned,
                        state.fetched,
                        state.checked_out.as_deref().unwrap_or("none"),
                        state.head.as_deref().unwrap_or("unknown")
                    );
                }
                Err(err) => {
                    eprintln!("recipes command failed: {err:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        cli::Command::Lookup(args) => {
            let topdir = args.effective_topdir();
            match build_lock::lookup_build_runtime(&topdir) {
                Ok(snapshot) => {
                    let rendered = if args.compact {
                        serde_json::to_string(&snapshot)
                    } else {
                        serde_json::to_string_pretty(&snapshot)
                    };
                    match rendered {
                        Ok(body) => println!("{body}"),
                        Err(err) => {
                            eprintln!("lookup serialization failed: {err:#}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
                Err(err) => {
                    eprintln!("lookup failed: {err:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        cli::Command::List(args) => {
            let topdir = args.effective_topdir();
            match list::run_list(&topdir, &args) {
                Ok(_) => {}
                Err(err) => {
                    eprintln!("list failed: {err:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        cli::Command::Failures(args) => {
            let topdir = args.effective_topdir();
            match list::run_failures(&topdir, &args) {
                Ok(_) => {}
                Err(err) => {
                    eprintln!("failures failed: {err:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        cli::Command::Todo(args) => {
            let topdir = args.effective_topdir();
            match list::run_todo(&topdir, &args) {
                Ok(_) => {}
                Err(err) => {
                    eprintln!("todo failed: {err:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        cli::Command::Blacklist(args) => {
            let topdir = args.effective_topdir();
            match list::run_blacklist(&topdir, &args) {
                Ok(_) => {}
                Err(err) => {
                    eprintln!("blacklist failed: {err:#}");
                    return ExitCode::FAILURE;
                }
            }
        }
        cli::Command::Catalog(args) => match args.command {
            cli::CatalogCommand::Migrate(migrate_args) => {
                let topdir = migrate_args.effective_topdir();
                match list::run_catalog_migrate(&topdir, migrate_args.json) {
                    Ok(_) => {}
                    Err(err) => {
                        eprintln!("catalog migrate failed: {err:#}");
                        return ExitCode::FAILURE;
                    }
                }
            }
        },
    }

    ExitCode::SUCCESS
}
