mod build_lock;
mod cli;
mod priority_specs;
mod recipe_repo;
mod ui;

use clap::Parser;
use std::fs;
use std::process::ExitCode;

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

fn main() -> ExitCode {
    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Build(mut args) => {
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
            let requested_packages = if args.packages.is_empty() {
                vec![format!(
                    "packages-file:{}",
                    args.packages_file
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                )]
            } else {
                args.packages.clone()
            };
            let _build_session = match build_lock::BuildSessionGuard::acquire(
                &topdir,
                &args.effective_target_id(),
                &requested_packages,
            ) {
                Ok(guard) => {
                    priority_specs::log_external_progress(format!(
                        "phase=workspace-lock status=acquired topdir={} target_id={} packages={}",
                        topdir.display(),
                        args.effective_target_id(),
                        requested_packages.join(",")
                    ));
                    guard
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
        cli::Command::GeneratePrioritySpecs(mut args) => {
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
    }

    ExitCode::SUCCESS
}
