use clap::{Parser, Subcommand, ValueEnum};
use std::env;
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
    /// Run a regression corpus campaign (PR top-N or full nightly).
    Regression(RegressionArgs),
    /// Generate Phoreus payload/meta SPECs for top-priority tools from tools.csv.
    GeneratePrioritySpecs(GeneratePrioritySpecsArgs),
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

fn canonical_arch_name(raw: &str) -> &'static str {
    match raw {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        _ => "x86_64",
    }
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
pub enum MetadataAdapter {
    Auto,
    Conda,
    Native,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum DeploymentProfile {
    Development,
    Production,
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum RegressionMode {
    Pr,
    Nightly,
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

    /// RPM build topdir. Defaults to ~/bioconda2rpm when omitted.
    #[arg(long)]
    pub topdir: Option<PathBuf>,

    /// Quarantine folder for unresolved/non-compliant packages.
    /// Defaults to <topdir>/targets/<target-id>/BAD_SPEC when omitted.
    #[arg(long)]
    pub bad_spec_dir: Option<PathBuf>,

    /// Optional explicit report output directory.
    /// Defaults to <topdir>/targets/<target-id>/reports when omitted.
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

    /// Container image to use for RPM builds (SPEC -> SRPM -> RPM).
    #[arg(long, default_value = "dropworm_dev_almalinux_9_5:0.1.2")]
    pub container_image: String,

    /// Container engine binary. Defaults to docker.
    #[arg(long, default_value = "docker")]
    pub container_engine: String,

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

    /// Metadata ingestion adapter.
    /// `auto` tries conda-build rendering first, then falls back to native parser.
    #[arg(long, value_enum, default_value_t = MetadataAdapter::Auto)]
    pub metadata_adapter: MetadataAdapter,

    /// Deployment profile.
    /// Production profile enforces conda-based metadata rendering.
    #[arg(long, value_enum, default_value_t = DeploymentProfile::Development)]
    pub deployment_profile: DeploymentProfile,

    /// Enforce arch-adjusted first-pass KPI gate for this run.
    #[arg(long)]
    pub kpi_gate: bool,

    /// Minimum arch-adjusted first-pass success rate required when KPI gate is active.
    #[arg(long, default_value_t = 99.0)]
    pub kpi_min_success_rate: f64,

    /// How to handle recipes with outputs: sections.
    #[arg(long, value_enum, default_value_t = OutputSelection::All)]
    pub outputs: OutputSelection,

    /// Requested Bioconda package name.
    pub package: String,

    /// Local Phoreus repository URLs to embed in reserved `phoreus` package config.
    #[arg(long = "phoreus-local-repo")]
    pub phoreus_local_repo: Vec<String>,

    /// Core OS repository URLs to embed in reserved `phoreus` package config.
    #[arg(long = "phoreus-core-repo")]
    pub phoreus_core_repo: Vec<String>,
}

#[derive(Debug, clap::Args)]
pub struct GeneratePrioritySpecsArgs {
    /// Root directory containing Bioconda recipes.
    #[arg(long)]
    pub recipe_root: PathBuf,

    /// CSV file containing priority scores (RPM Priority Score column).
    #[arg(long)]
    pub tools_csv: PathBuf,

    /// Number of highest-priority tools to process.
    #[arg(long, default_value_t = 10)]
    pub top_n: usize,

    /// Number of worker threads for parallel processing.
    #[arg(long)]
    pub workers: Option<usize>,

    /// Container image to use for RPM builds (SPEC -> SRPM -> RPM).
    #[arg(long)]
    pub container_image: String,

    /// Container engine binary. Defaults to docker.
    #[arg(long, default_value = "docker")]
    pub container_engine: String,

    /// RPM build topdir. Defaults to ~/bioconda2rpm when omitted.
    #[arg(long)]
    pub topdir: Option<PathBuf>,

    /// Quarantine folder for unresolved/non-compliant packages.
    /// Defaults to <topdir>/targets/<target-id>/BAD_SPEC when omitted.
    #[arg(long)]
    pub bad_spec_dir: Option<PathBuf>,

    /// Optional explicit report output directory.
    /// Defaults to <topdir>/targets/<target-id>/reports when omitted.
    #[arg(long)]
    pub reports_dir: Option<PathBuf>,

    /// Metadata ingestion adapter.
    /// `auto` tries conda-build rendering first, then falls back to native parser.
    #[arg(long, value_enum, default_value_t = MetadataAdapter::Auto)]
    pub metadata_adapter: MetadataAdapter,
}

#[derive(Debug, clap::Args)]
pub struct RegressionArgs {
    /// Root directory containing Bioconda recipes.
    #[arg(long)]
    pub recipe_root: PathBuf,

    /// CSV file containing priority scores (RPM Priority Score column).
    #[arg(long)]
    pub tools_csv: PathBuf,

    /// Optional newline-delimited software list.
    /// When set, this list defines the corpus and overrides mode/top-n selection.
    #[arg(long)]
    pub software_list: Option<PathBuf>,

    /// Regression campaign mode.
    #[arg(long, value_enum, default_value_t = RegressionMode::Pr)]
    pub mode: RegressionMode,

    /// Number of highest-priority tools for PR mode.
    #[arg(long, default_value_t = 25)]
    pub top_n: usize,

    /// RPM build topdir. Defaults to ~/bioconda2rpm when omitted.
    #[arg(long)]
    pub topdir: Option<PathBuf>,

    /// Quarantine folder for unresolved/non-compliant packages.
    /// Defaults to <topdir>/targets/<target-id>/BAD_SPEC when omitted.
    #[arg(long)]
    pub bad_spec_dir: Option<PathBuf>,

    /// Optional explicit report output directory.
    /// Defaults to <topdir>/targets/<target-id>/reports when omitted.
    #[arg(long)]
    pub reports_dir: Option<PathBuf>,

    /// Container image to use for RPM builds (SPEC -> SRPM -> RPM).
    #[arg(long, default_value = "dropworm_dev_almalinux_9_5:0.1.2")]
    pub container_image: String,

    /// Container engine binary. Defaults to docker.
    #[arg(long, default_value = "docker")]
    pub container_engine: String,

    /// Dependency closure policy for discovered requirements.
    #[arg(long, value_enum, default_value_t = DependencyPolicy::BuildHostRun)]
    pub dependency_policy: DependencyPolicy,

    /// Disable dependency closure and build only the requested package.
    #[arg(long)]
    pub no_deps: bool,

    /// Behavior when dependency recipes cannot be resolved.
    #[arg(long, value_enum, default_value_t = MissingDependencyPolicy::Quarantine)]
    pub missing_dependency: MissingDependencyPolicy,

    /// Target architecture for the campaign.
    #[arg(long, value_enum, default_value_t = BuildArch::X86_64)]
    pub arch: BuildArch,

    /// Metadata ingestion adapter.
    /// `auto` tries conda-build rendering first, then falls back to native parser.
    #[arg(long, value_enum, default_value_t = MetadataAdapter::Auto)]
    pub metadata_adapter: MetadataAdapter,

    /// Deployment profile.
    /// Production profile enforces conda-based metadata rendering.
    #[arg(long, value_enum, default_value_t = DeploymentProfile::Production)]
    pub deployment_profile: DeploymentProfile,

    /// Disable campaign-level arch-adjusted KPI gate.
    #[arg(long)]
    pub no_kpi_gate: bool,

    /// Minimum campaign arch-adjusted first-pass success rate.
    #[arg(long, default_value_t = 99.0)]
    pub kpi_min_success_rate: f64,
}

pub fn default_topdir() -> PathBuf {
    match env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join("bioconda2rpm"),
        None => PathBuf::from("bioconda2rpm"),
    }
}

fn sanitize_target_component(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in raw.chars() {
        let c = if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if c == '-' {
            if !last_dash {
                out.push(c);
            }
            last_dash = true;
        } else {
            out.push(c);
            last_dash = false;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn default_build_target_id(container_image: &str, target_arch: &str) -> String {
    let image = sanitize_target_component(container_image);
    let arch = sanitize_target_component(target_arch);
    let image = if image.is_empty() {
        "container"
    } else {
        &image
    };
    let arch = if arch.is_empty() { "x86_64" } else { &arch };
    format!("{image}-{arch}")
}

impl BuildArgs {
    pub fn with_deps(&self) -> bool {
        !self.no_deps
    }

    pub fn effective_topdir(&self) -> PathBuf {
        self.topdir.clone().unwrap_or_else(default_topdir)
    }

    pub fn effective_target_id(&self) -> String {
        default_build_target_id(&self.container_image, &self.effective_target_arch())
    }

    pub fn effective_target_root(&self) -> PathBuf {
        self.effective_topdir()
            .join("targets")
            .join(self.effective_target_id())
    }

    pub fn effective_bad_spec_dir(&self) -> PathBuf {
        self.bad_spec_dir
            .clone()
            .unwrap_or_else(|| self.effective_target_root().join("BAD_SPEC"))
    }

    pub fn effective_reports_dir(&self) -> PathBuf {
        self.reports_dir
            .clone()
            .unwrap_or_else(|| self.effective_target_root().join("reports"))
    }

    pub fn effective_target_arch(&self) -> String {
        match self.arch {
            BuildArch::Host => canonical_arch_name(std::env::consts::ARCH).to_string(),
            BuildArch::X86_64 => "x86_64".to_string(),
            BuildArch::Aarch64 => "aarch64".to_string(),
        }
    }

    pub fn effective_metadata_adapter(&self) -> MetadataAdapter {
        match self.deployment_profile {
            DeploymentProfile::Development => self.metadata_adapter.clone(),
            DeploymentProfile::Production => MetadataAdapter::Conda,
        }
    }

    pub fn effective_kpi_gate(&self) -> bool {
        self.kpi_gate || self.deployment_profile == DeploymentProfile::Production
    }

    pub fn execution_summary(&self) -> String {
        format!(
            "build package={pkg} stage={stage:?} with_deps={deps} policy={policy:?} recipe_root={recipes} topdir={topdir} target_id={target_id} target_root={target_root} bad_spec_dir={bad_spec} reports_dir={reports} container_mode={container:?} container_image={container_image} container_engine={container_engine} arch={arch:?} target_arch={target_arch} deployment_profile={deployment_profile:?} naming={naming:?} render={render:?} metadata_adapter={metadata_adapter:?} effective_metadata_adapter={effective_metadata_adapter:?} kpi_gate={kpi_gate} kpi_min_success_rate={kpi_min_success_rate:.2} outputs={outputs:?} missing_dependency={missing:?} phoreus_local_repo_count={local_repo_count} phoreus_core_repo_count={core_repo_count}",
            pkg = self.package,
            stage = self.stage,
            deps = self.with_deps(),
            policy = self.dependency_policy,
            recipes = self.recipe_root.display(),
            topdir = self.effective_topdir().display(),
            target_root = self.effective_target_root().display(),
            target_id = self.effective_target_id(),
            bad_spec = self.effective_bad_spec_dir().display(),
            reports = self.effective_reports_dir().display(),
            container = self.container_mode,
            container_image = self.container_image,
            container_engine = self.container_engine,
            arch = self.arch,
            target_arch = self.effective_target_arch(),
            deployment_profile = self.deployment_profile,
            naming = self.naming_profile,
            render = self.render_strategy,
            metadata_adapter = self.metadata_adapter,
            effective_metadata_adapter = self.effective_metadata_adapter(),
            kpi_gate = self.effective_kpi_gate(),
            kpi_min_success_rate = self.kpi_min_success_rate,
            outputs = self.outputs,
            missing = self.missing_dependency,
            local_repo_count = self.phoreus_local_repo.len(),
            core_repo_count = self.phoreus_core_repo.len(),
        )
    }
}

impl GeneratePrioritySpecsArgs {
    pub fn effective_topdir(&self) -> PathBuf {
        self.topdir.clone().unwrap_or_else(default_topdir)
    }

    pub fn effective_target_arch(&self) -> String {
        canonical_arch_name(std::env::consts::ARCH).to_string()
    }

    pub fn effective_target_id(&self) -> String {
        default_build_target_id(&self.container_image, &self.effective_target_arch())
    }

    pub fn effective_target_root(&self) -> PathBuf {
        self.effective_topdir()
            .join("targets")
            .join(self.effective_target_id())
    }

    pub fn effective_bad_spec_dir(&self) -> PathBuf {
        self.bad_spec_dir
            .clone()
            .unwrap_or_else(|| self.effective_target_root().join("BAD_SPEC"))
    }

    pub fn effective_reports_dir(&self) -> PathBuf {
        self.reports_dir
            .clone()
            .unwrap_or_else(|| self.effective_target_root().join("reports"))
    }
}

impl RegressionArgs {
    pub fn effective_topdir(&self) -> PathBuf {
        self.topdir.clone().unwrap_or_else(default_topdir)
    }

    pub fn effective_target_id(&self) -> String {
        default_build_target_id(&self.container_image, &self.effective_target_arch())
    }

    pub fn effective_target_root(&self) -> PathBuf {
        self.effective_topdir()
            .join("targets")
            .join(self.effective_target_id())
    }

    pub fn effective_bad_spec_dir(&self) -> PathBuf {
        self.bad_spec_dir
            .clone()
            .unwrap_or_else(|| self.effective_target_root().join("BAD_SPEC"))
    }

    pub fn effective_reports_dir(&self) -> PathBuf {
        self.reports_dir
            .clone()
            .unwrap_or_else(|| self.effective_target_root().join("reports"))
    }

    pub fn effective_target_arch(&self) -> String {
        match self.arch {
            BuildArch::Host => canonical_arch_name(std::env::consts::ARCH).to_string(),
            BuildArch::X86_64 => "x86_64".to_string(),
            BuildArch::Aarch64 => "aarch64".to_string(),
        }
    }

    pub fn effective_metadata_adapter(&self) -> MetadataAdapter {
        match self.deployment_profile {
            DeploymentProfile::Development => self.metadata_adapter.clone(),
            DeploymentProfile::Production => MetadataAdapter::Conda,
        }
    }

    pub fn effective_kpi_gate(&self) -> bool {
        !self.no_kpi_gate
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
        ])
        .expect("build defaults should parse");

        let Command::Build(args) = cli.command else {
            panic!("expected build command")
        };
        assert_eq!(args.package, "fastp");
        assert_eq!(args.stage, BuildStage::Rpm);
        assert_eq!(args.dependency_policy, DependencyPolicy::BuildHostRun);
        assert!(args.with_deps());
        assert_eq!(args.container_mode, ContainerMode::Ephemeral);
        assert_eq!(args.missing_dependency, MissingDependencyPolicy::Quarantine);
        assert_eq!(args.arch, BuildArch::Host);
        assert_eq!(args.naming_profile, NamingProfile::Phoreus);
        assert_eq!(args.render_strategy, RenderStrategy::JinjaFull);
        assert_eq!(args.metadata_adapter, MetadataAdapter::Auto);
        assert_eq!(args.deployment_profile, DeploymentProfile::Development);
        assert_eq!(args.effective_metadata_adapter(), MetadataAdapter::Auto);
        assert!(!args.effective_kpi_gate());
        assert_eq!(args.kpi_min_success_rate, 99.0);
        assert_eq!(args.outputs, OutputSelection::All);
        assert!(args.effective_topdir().ends_with("bioconda2rpm"));
        assert!(
            args.effective_target_root()
                .starts_with(args.effective_topdir().join("targets"))
        );
        assert!(
            args.effective_bad_spec_dir()
                .starts_with(args.effective_target_root())
        );
        assert!(args.effective_bad_spec_dir().ends_with("BAD_SPEC"));
        assert!(
            args.effective_reports_dir()
                .starts_with(args.effective_target_root())
        );
        assert!(args.effective_reports_dir().ends_with("reports"));
    }

    #[test]
    fn build_command_accepts_topdir_and_bad_spec_overrides() {
        let cli = Cli::try_parse_from([
            "bioconda2rpm",
            "build",
            "samtools",
            "--recipe-root",
            "/recipes",
            "--topdir",
            "/rpmbuild",
            "--bad-spec-dir",
            "/quarantine",
        ])
        .expect("topdir and bad spec overrides should parse");

        let Command::Build(args) = cli.command else {
            panic!("expected build command")
        };
        assert_eq!(args.effective_topdir(), PathBuf::from("/rpmbuild"));
        assert_eq!(args.effective_bad_spec_dir(), PathBuf::from("/quarantine"));
        assert_eq!(
            args.effective_reports_dir(),
            args.effective_target_root().join("reports")
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
            "--metadata-adapter",
            "native",
            "--deployment-profile",
            "production",
            "--kpi-min-success-rate",
            "99.5",
            "--reports-dir",
            "/reports",
        ])
        .expect("build overrides should parse");

        let Command::Build(args) = cli.command else {
            panic!("expected build command")
        };
        assert_eq!(args.stage, BuildStage::Spec);
        assert_eq!(args.dependency_policy, DependencyPolicy::RunOnly);
        assert!(!args.with_deps());
        assert_eq!(args.container_mode, ContainerMode::Auto);
        assert_eq!(args.missing_dependency, MissingDependencyPolicy::Fail);
        assert_eq!(args.arch, BuildArch::Aarch64);
        assert_eq!(args.effective_target_arch(), "aarch64".to_string());
        assert_eq!(args.metadata_adapter, MetadataAdapter::Native);
        assert_eq!(args.deployment_profile, DeploymentProfile::Production);
        assert_eq!(args.effective_metadata_adapter(), MetadataAdapter::Conda);
        assert!(args.effective_kpi_gate());
        assert_eq!(args.kpi_min_success_rate, 99.5);
        assert_eq!(args.effective_topdir(), PathBuf::from("/rpmbuild"));
        assert_eq!(args.effective_reports_dir(), PathBuf::from("/reports"));
    }

    #[test]
    fn build_requires_recipe_root() {
        let parse = Cli::try_parse_from(["bioconda2rpm", "build", "fastqc"]);

        assert!(parse.is_err());
    }

    #[test]
    fn generate_priority_specs_defaults_parse() {
        let cli = Cli::try_parse_from([
            "bioconda2rpm",
            "generate-priority-specs",
            "--recipe-root",
            "/recipes",
            "--tools-csv",
            "/tmp/tools.csv",
            "--container-image",
            "almalinux:9",
        ])
        .expect("generate-priority-specs defaults should parse");

        let Command::GeneratePrioritySpecs(args) = cli.command else {
            panic!("expected generate-priority-specs subcommand");
        };
        assert_eq!(args.top_n, 10);
        assert_eq!(args.container_image, "almalinux:9");
        assert_eq!(args.container_engine, "docker");
        assert_eq!(args.metadata_adapter, MetadataAdapter::Auto);
        assert!(args.effective_topdir().ends_with("bioconda2rpm"));
        assert!(
            args.effective_bad_spec_dir()
                .starts_with(args.effective_target_root())
        );
        assert!(args.effective_bad_spec_dir().ends_with("BAD_SPEC"));
        assert!(
            args.effective_reports_dir()
                .starts_with(args.effective_target_root())
        );
        assert!(args.effective_reports_dir().ends_with("reports"));
    }

    #[test]
    fn regression_defaults_parse() {
        let cli = Cli::try_parse_from([
            "bioconda2rpm",
            "regression",
            "--recipe-root",
            "/recipes",
            "--tools-csv",
            "/tmp/tools.csv",
        ])
        .expect("regression defaults should parse");

        let Command::Regression(args) = cli.command else {
            panic!("expected regression subcommand");
        };
        assert_eq!(args.mode, RegressionMode::Pr);
        assert_eq!(args.top_n, 25);
        assert!(args.software_list.is_none());
        assert_eq!(args.deployment_profile, DeploymentProfile::Production);
        assert_eq!(args.effective_metadata_adapter(), MetadataAdapter::Conda);
        assert_eq!(args.effective_target_arch(), "x86_64".to_string());
        assert!(
            args.effective_bad_spec_dir()
                .starts_with(args.effective_target_root())
        );
        assert!(args.effective_bad_spec_dir().ends_with("BAD_SPEC"));
        assert!(
            args.effective_reports_dir()
                .starts_with(args.effective_target_root())
        );
        assert!(args.effective_reports_dir().ends_with("reports"));
        assert!(args.effective_kpi_gate());
        assert_eq!(args.kpi_min_success_rate, 99.0);
    }

    #[test]
    fn regression_accepts_software_list_override() {
        let cli = Cli::try_parse_from([
            "bioconda2rpm",
            "regression",
            "--recipe-root",
            "/recipes",
            "--tools-csv",
            "/tmp/tools.csv",
            "--software-list",
            "/tmp/essential_100.txt",
            "--mode",
            "nightly",
        ])
        .expect("regression software list should parse");

        let Command::Regression(args) = cli.command else {
            panic!("expected regression subcommand");
        };
        assert_eq!(args.mode, RegressionMode::Nightly);
        assert_eq!(
            args.software_list,
            Some(PathBuf::from("/tmp/essential_100.txt"))
        );
    }

    #[test]
    fn default_build_target_id_is_sanitized_and_stable() {
        let target_id = default_build_target_id("dropworm_dev_almalinux_9_5:0.1.2", "aarch64");
        assert_eq!(target_id, "dropworm_dev_almalinux_9_5-0.1.2-aarch64");
    }
}
