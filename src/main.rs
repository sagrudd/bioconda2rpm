mod build_lock;
mod cli;
mod list;
mod priority_specs;
mod recipe_repo;
mod ui;

use clap::Parser;
use std::fs;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

static SIGNAL_HANDLER_INSTALLED: OnceLock<()> = OnceLock::new();

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
            let _build_session = match build_lock::BuildSessionGuard::acquire_or_forward_build(
                &topdir,
                &args.effective_target_id(),
                &requested_packages,
                args.force,
            ) {
                Ok(build_lock::BuildAcquireOutcome::Owner(guard)) => {
                    priority_specs::log_external_progress(format!(
                        "phase=workspace-lock status=acquired topdir={} target_id={} packages={}",
                        topdir.display(),
                        args.effective_target_id(),
                        requested_packages.join(",")
                    ));
                    guard
                }
                Ok(build_lock::BuildAcquireOutcome::Forwarded(forwarded)) => {
                    priority_specs::log_external_progress(format!(
                        "phase=workspace-lock status=forwarded owner_pid={} target_id={} owner_force={} packages={}",
                        forwarded.owner_pid,
                        forwarded.owner_target_id,
                        forwarded.owner_force_rebuild,
                        forwarded.queued_packages.join(",")
                    ));
                    priority_specs::clear_progress_sink();
                    if let Some(ui) = progress_ui.take() {
                        ui.finish(format!(
                            "request forwarded to active build session (owner pid={}, packages={})",
                            forwarded.owner_pid,
                            forwarded.queued_packages.join(",")
                        ));
                    }
                    println!(
                        "forwarded build request to active session owner_pid={} target_id={} owner_force={} packages={}",
                        forwarded.owner_pid,
                        forwarded.owner_target_id,
                        forwarded.owner_force_rebuild,
                        forwarded.queued_packages.join(",")
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
        cli::Command::Server(mut args) => {
            priority_specs::reset_cancellation();
            let topdir = args.effective_topdir();
            let bad_spec = args.effective_bad_spec_dir();
            let reports = args.effective_reports_dir();
            if let Err(err) = ensure_workspace_paths(&topdir, &bad_spec, &reports) {
                eprintln!("failed to prepare workspace directories: {err}");
                return ExitCode::FAILURE;
            }

            let mut progress_ui = {
                let title = format!("bioconda2rpm server ({})", args.effective_target_id());
                let ui = ui::ProgressUi::start(title);
                priority_specs::install_progress_sink(ui.sink());
                Some(ui)
            };

            let mut pending_packages = match collect_initial_server_packages(&args) {
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

            let _server_session = match build_lock::BuildSessionGuard::acquire(
                &topdir,
                &args.effective_target_id(),
                &pending_packages,
                build_lock::BuildSessionKind::Build,
                args.force,
            ) {
                Ok(guard) => {
                    priority_specs::log_external_progress(format!(
                        "phase=workspace-lock status=server-acquired topdir={} target_id={} initial_packages={}",
                        topdir.display(),
                        args.effective_target_id(),
                        pending_packages.join(",")
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

            while !priority_specs::cancellation_requested_public() {
                match build_lock::drain_forwarded_build_requests(
                    topdir.as_path(),
                    &args.effective_target_id(),
                ) {
                    Ok(forwarded) => {
                        for request in forwarded {
                            pending_packages.push(request.package);
                        }
                    }
                    Err(err) => {
                        priority_specs::log_external_progress(format!(
                            "phase=server status=queue-drain-error target_id={} detail={}",
                            args.effective_target_id(),
                            err
                        ));
                    }
                }

                pending_packages.sort();
                pending_packages.dedup();
                if pending_packages.is_empty() {
                    priority_specs::log_external_progress(format!(
                        "phase=server status=waiting target_id={} detail=idle-until-forwarded-build-or-ctrl-c",
                        args.effective_target_id()
                    ));
                    thread::sleep(Duration::from_millis(750));
                    continue;
                }

                args.packages = std::mem::take(&mut pending_packages);
                priority_specs::log_external_progress(format!(
                    "phase=server status=build-start target_id={} packages={}",
                    args.effective_target_id(),
                    args.packages.join(",")
                ));
                let outcome = priority_specs::run_build(&args);
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
                            "phase=server status=build-error target_id={} detail={}",
                            args.effective_target_id(),
                            err
                        ));
                    }
                }
                args.packages.clear();
            }

            priority_specs::clear_progress_sink();
            if let Some(ui) = progress_ui.take() {
                ui.finish("server stopped by user".to_string());
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
                args.packages.join(","),
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
    }

    ExitCode::SUCCESS
}
