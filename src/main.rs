mod cli;
mod priority_specs;

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
        cli::Command::Build(args) => {
            let topdir = args.effective_topdir();
            let bad_spec = args.effective_bad_spec_dir();
            let reports = args.effective_reports_dir();
            if let Err(err) = ensure_workspace_paths(&topdir, &bad_spec, &reports) {
                eprintln!("failed to prepare workspace directories: {err}");
                return ExitCode::FAILURE;
            }
            println!("{}", args.execution_summary());
            match priority_specs::run_build(&args) {
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
        cli::Command::GeneratePrioritySpecs(args) => {
            let topdir = args.effective_topdir();
            let bad_spec = args.effective_bad_spec_dir();
            let reports = args.effective_reports_dir();
            if let Err(err) = ensure_workspace_paths(&topdir, &bad_spec, &reports) {
                eprintln!("failed to prepare workspace directories: {err}");
                return ExitCode::FAILURE;
            }

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
    }

    ExitCode::SUCCESS
}
