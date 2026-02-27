mod cli;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();

    match cli.command {
        cli::Command::Build(args) => {
            println!("{}", args.execution_summary());
        }
    }
}
