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
