mod cli;

use clap::Parser;
use std::fs;
use std::process::ExitCode;

fn ensure_workspace_paths(args: &cli::BuildArgs) -> std::io::Result<()> {
    fs::create_dir_all(args.effective_topdir())?;
    fs::create_dir_all(args.effective_bad_spec_dir())?;
    fs::create_dir_all(args.effective_reports_dir())?;
    Ok(())
}

fn main() -> ExitCode {
    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Build(args) => {
            if let Err(err) = ensure_workspace_paths(&args) {
                eprintln!("failed to prepare workspace directories: {err}");
                return ExitCode::FAILURE;
            }
            println!("{}", args.execution_summary());
        }
    }

    ExitCode::SUCCESS
}
