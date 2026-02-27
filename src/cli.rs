use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "bioconda2rpm",
    version,
    about = "Convert Bioconda recipes into Phoreus-style RPM artifacts"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Build RPM artifacts for a package and optionally its dependency closure.
    Build(BuildArgs),
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum BuildStage {
    Spec,
    Srpm,
    Rpm,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum DependencyPolicy {
    RunOnly,
    BuildHostRun,
    RuntimeTransitiveRootBuildHost,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum ContainerMode {
    Ephemeral,
    Running,
    Auto,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum MissingDependencyPolicy {
    Fail,
    Skip,
    Quarantine,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum BuildArch {
    Host,
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum NamingProfile {
    Phoreus,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum RenderStrategy {
    JinjaFull,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum OutputSelection {
    All,
}

#[derive(Debug, clap::Args)]
pub struct BuildArgs {
    /// Root directory containing Bioconda recipes.
    #[arg(long)]
    pub recipe_root: PathBuf,

    /// RPM build topdir. Must be outside this crate workspace.
    #[arg(long)]
    pub topdir: PathBuf,

    /// Optional explicit report output directory.
    #[arg(long)]
    pub reports_dir: Option<PathBuf>,

    /// Packaging stage target.
    #[arg(long, value_enum, default_value_t = BuildStage::Rpm)]
    pub stage: BuildStage,

    /// Dependency closure policy for discovered requirements.
    #[arg(long, value_enum, default_value_t = DependencyPolicy::BuildHostRun)]
    pub dependency_policy: DependencyPolicy,

    /// Disable dependency closure and build only the requested package.
    #[arg(long)]
    pub no_deps: bool,

    /// Container execution model.
    #[arg(long, value_enum, default_value_t = ContainerMode::Ephemeral)]
    pub container_mode: ContainerMode,

    /// Behavior when dependency recipes cannot be resolved.
    #[arg(long, value_enum, default_value_t = MissingDependencyPolicy::Quarantine)]
    pub missing_dependency: MissingDependencyPolicy,

    /// Target architecture for the run.
    #[arg(long, value_enum, default_value_t = BuildArch::Host)]
    pub arch: BuildArch,

    /// RPM naming/layout profile.
    #[arg(long, value_enum, default_value_t = NamingProfile::Phoreus)]
    pub naming_profile: NamingProfile,

    /// Meta.yaml rendering strategy.
    #[arg(long, value_enum, default_value_t = RenderStrategy::JinjaFull)]
    pub render_strategy: RenderStrategy,

    /// How to handle recipes with outputs: sections.
    #[arg(long, value_enum, default_value_t = OutputSelection::All)]
    pub outputs: OutputSelection,

    /// Requested Bioconda package name.
    pub package: String,
}

impl BuildArgs {
    pub fn with_deps(&self) -> bool {
        !self.no_deps
    }

    pub fn effective_reports_dir(&self) -> PathBuf {
        self.reports_dir
            .clone()
            .unwrap_or_else(|| self.topdir.join("reports"))
    }

    pub fn execution_summary(&self) -> String {
        format!(
            "build package={pkg} stage={stage:?} with_deps={deps} policy={policy:?} recipe_root={recipes} topdir={topdir} reports_dir={reports} container_mode={container:?} arch={arch:?} naming={naming:?} render={render:?} outputs={outputs:?} missing_dependency={missing:?}",
            pkg = self.package,
            stage = self.stage,
            deps = self.with_deps(),
            policy = self.dependency_policy,
            recipes = self.recipe_root.display(),
            topdir = self.topdir.display(),
            reports = self.effective_reports_dir().display(),
            container = self.container_mode,
            arch = self.arch,
            naming = self.naming_profile,
            render = self.render_strategy,
            outputs = self.outputs,
            missing = self.missing_dependency,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn build_command_uses_expected_defaults() {
        let cli = Cli::try_parse_from([
            "bioconda2rpm",
            "build",
            "fastp",
            "--recipe-root",
            "/tmp/recipes",
            "--topdir",
            "/tmp/rpmbuild",
        ])
        .expect("build defaults should parse");

        let Command::Build(args) = cli.command;
        assert_eq!(args.package, "fastp");
        assert_eq!(args.stage, BuildStage::Rpm);
        assert_eq!(args.dependency_policy, DependencyPolicy::BuildHostRun);
        assert!(args.with_deps());
        assert_eq!(args.container_mode, ContainerMode::Ephemeral);
        assert_eq!(args.missing_dependency, MissingDependencyPolicy::Quarantine);
        assert_eq!(args.arch, BuildArch::Host);
        assert_eq!(args.naming_profile, NamingProfile::Phoreus);
        assert_eq!(args.render_strategy, RenderStrategy::JinjaFull);
        assert_eq!(args.outputs, OutputSelection::All);
        assert_eq!(
            args.effective_reports_dir(),
            PathBuf::from("/tmp/rpmbuild/reports")
        );
    }

    #[test]
    fn build_command_accepts_overrides() {
        let cli = Cli::try_parse_from([
            "bioconda2rpm",
            "build",
            "samtools",
            "--recipe-root",
            "/recipes",
            "--topdir",
            "/rpmbuild",
            "--stage",
            "spec",
            "--dependency-policy",
            "run-only",
            "--no-deps",
            "--container-mode",
            "auto",
            "--missing-dependency",
            "fail",
            "--arch",
            "aarch64",
            "--reports-dir",
            "/reports",
        ])
        .expect("build overrides should parse");

        let Command::Build(args) = cli.command;
        assert_eq!(args.stage, BuildStage::Spec);
        assert_eq!(args.dependency_policy, DependencyPolicy::RunOnly);
        assert!(!args.with_deps());
        assert_eq!(args.container_mode, ContainerMode::Auto);
        assert_eq!(args.missing_dependency, MissingDependencyPolicy::Fail);
        assert_eq!(args.arch, BuildArch::Aarch64);
        assert_eq!(args.effective_reports_dir(), PathBuf::from("/reports"));
    }

    #[test]
    fn build_requires_topdir() {
        let parse = Cli::try_parse_from([
            "bioconda2rpm",
            "build",
            "fastqc",
            "--recipe-root",
            "/tmp/recipes",
        ]);

        assert!(parse.is_err());
    }
}
