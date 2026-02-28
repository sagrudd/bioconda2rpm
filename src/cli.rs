use clap::{Parser, Subcommand, ValueEnum};
use std::env;
use std::io::IsTerminal;
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

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum BuildContainerProfile {
    #[value(name = "almalinux-9.7")]
    Almalinux97,
    #[value(name = "almalinux-10.1")]
    Almalinux101,
    #[value(name = "fedora-43")]
    Fedora43,
}

impl BuildContainerProfile {
    pub fn image(self) -> &'static str {
        match self {
            BuildContainerProfile::Almalinux97 => "phoreus/bioconda2rpm-build:almalinux-9.7",
            BuildContainerProfile::Almalinux101 => "phoreus/bioconda2rpm-build:almalinux-10.1",
            BuildContainerProfile::Fedora43 => "phoreus/bioconda2rpm-build:fedora-43",
        }
    }

    pub fn dockerfile_path(self) -> &'static str {
        match self {
            BuildContainerProfile::Almalinux97 => {
                "containers/rpm-build-images/Dockerfile.almalinux-9.7"
            }
            BuildContainerProfile::Almalinux101 => {
                "containers/rpm-build-images/Dockerfile.almalinux-10.1"
            }
            BuildContainerProfile::Fedora43 => "containers/rpm-build-images/Dockerfile.fedora-43",
        }
    }
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum ParallelPolicy {
    Serial,
    Adaptive,
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

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
pub enum UiMode {
    Plain,
    Ratatui,
    Auto,
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

    /// Controlled build container profile used for SPEC -> SRPM -> RPM.
    #[arg(long, value_enum, default_value_t = BuildContainerProfile::Almalinux97)]
    pub container_profile: BuildContainerProfile,

    /// Container engine binary. Defaults to docker.
    #[arg(long, default_value = "docker")]
    pub container_engine: String,

    /// Build parallelism policy.
    /// `adaptive` attempts parallel build first and retries serial when needed.
    #[arg(long, value_enum, default_value_t = ParallelPolicy::Adaptive)]
    pub parallel_policy: ParallelPolicy,

    /// Build job count for parallel mode. Accepts integer or `auto`.
    #[arg(long, default_value = "4")]
    pub build_jobs: String,

    /// Maximum number of queued package builds to run concurrently.
    /// Defaults to floor(host_cores / effective_build_jobs), minimum 1.
    #[arg(long)]
    pub queue_workers: Option<usize>,

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

    /// Console UI mode for build progress.
    #[arg(long, value_enum, default_value_t = UiMode::Auto)]
    pub ui: UiMode,

    /// Optional newline-delimited packages file (supports `#` comments).
    #[arg(long)]
    pub packages_file: Option<PathBuf>,

    /// One or more requested Bioconda package names.
    #[arg(value_name = "PACKAGE")]
    pub packages: Vec<String>,

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

    /// Controlled build container profile used for SPEC -> SRPM -> RPM.
    #[arg(long, value_enum, default_value_t = BuildContainerProfile::Almalinux97)]
    pub container_profile: BuildContainerProfile,

    /// Container engine binary. Defaults to docker.
    #[arg(long, default_value = "docker")]
    pub container_engine: String,

    /// Build parallelism policy.
    /// `adaptive` attempts parallel build first and retries serial when needed.
    #[arg(long, value_enum, default_value_t = ParallelPolicy::Adaptive)]
    pub parallel_policy: ParallelPolicy,

    /// Build job count for parallel mode. Accepts integer or `auto`.
    #[arg(long, default_value = "4")]
    pub build_jobs: String,

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

    /// Controlled build container profile used for SPEC -> SRPM -> RPM.
    #[arg(long, value_enum, default_value_t = BuildContainerProfile::Almalinux97)]
    pub container_profile: BuildContainerProfile,

    /// Container engine binary. Defaults to docker.
    #[arg(long, default_value = "docker")]
    pub container_engine: String,

    /// Build parallelism policy.
    /// `adaptive` attempts parallel build first and retries serial when needed.
    #[arg(long, value_enum, default_value_t = ParallelPolicy::Adaptive)]
    pub parallel_policy: ParallelPolicy,

    /// Build job count for parallel mode. Accepts integer or `auto`.
    #[arg(long, default_value = "4")]
    pub build_jobs: String,

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

fn host_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|v| v.get())
        .unwrap_or(1)
        .max(1)
}

fn parse_build_jobs(raw: &str) -> usize {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("auto") {
        return host_parallelism();
    }
    trimmed
        .parse::<usize>()
        .ok()
        .filter(|v| *v > 0)
        .unwrap_or(1)
}

impl BuildArgs {
    pub fn with_deps(&self) -> bool {
        !self.no_deps
    }

    pub fn effective_topdir(&self) -> PathBuf {
        self.topdir.clone().unwrap_or_else(default_topdir)
    }

    pub fn effective_container_image(&self) -> &'static str {
        self.container_profile.image()
    }

    pub fn effective_target_id(&self) -> String {
        default_build_target_id(
            self.effective_container_image(),
            &self.effective_target_arch(),
        )
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

    pub fn effective_build_jobs(&self) -> usize {
        match self.parallel_policy {
            ParallelPolicy::Serial => 1,
            ParallelPolicy::Adaptive => parse_build_jobs(&self.build_jobs),
        }
    }

    pub fn effective_queue_workers(&self) -> usize {
        if let Some(workers) = self.queue_workers.filter(|v| *v > 0) {
            return workers;
        }
        let host = host_parallelism();
        let per_job = self.effective_build_jobs().max(1);
        (host / per_job).max(1)
    }

    pub fn effective_ui_mode(&self) -> UiMode {
        match self.ui {
            UiMode::Plain => UiMode::Plain,
            UiMode::Ratatui => UiMode::Ratatui,
            UiMode::Auto => {
                if std::io::stdout().is_terminal() {
                    UiMode::Ratatui
                } else {
                    UiMode::Plain
                }
            }
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
            "build requested_packages={requested_packages} stage={stage:?} with_deps={deps} policy={policy:?} recipe_root={recipes} topdir={topdir} target_id={target_id} target_root={target_root} bad_spec_dir={bad_spec} reports_dir={reports} container_mode={container:?} container_profile={container_profile:?} container_image={container_image} container_engine={container_engine} parallel_policy={parallel_policy:?} build_jobs={build_jobs} effective_build_jobs={effective_build_jobs} queue_workers={queue_workers} effective_queue_workers={effective_queue_workers} ui={ui:?} effective_ui={effective_ui:?} arch={arch:?} target_arch={target_arch} deployment_profile={deployment_profile:?} naming={naming:?} render={render:?} metadata_adapter={metadata_adapter:?} effective_metadata_adapter={effective_metadata_adapter:?} kpi_gate={kpi_gate} kpi_min_success_rate={kpi_min_success_rate:.2} outputs={outputs:?} missing_dependency={missing:?} phoreus_local_repo_count={local_repo_count} phoreus_core_repo_count={core_repo_count}",
            requested_packages = self.packages.len(),
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
            container_profile = self.container_profile,
            container_image = self.effective_container_image(),
            container_engine = self.container_engine,
            parallel_policy = self.parallel_policy,
            build_jobs = self.build_jobs,
            effective_build_jobs = self.effective_build_jobs(),
            queue_workers = self
                .queue_workers
                .map(|v| v.to_string())
                .unwrap_or_else(|| "auto".to_string()),
            effective_queue_workers = self.effective_queue_workers(),
            ui = self.ui,
            effective_ui = self.effective_ui_mode(),
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

    pub fn effective_container_image(&self) -> &'static str {
        self.container_profile.image()
    }

    pub fn effective_target_arch(&self) -> String {
        canonical_arch_name(std::env::consts::ARCH).to_string()
    }

    pub fn effective_build_jobs(&self) -> usize {
        match self.parallel_policy {
            ParallelPolicy::Serial => 1,
            ParallelPolicy::Adaptive => parse_build_jobs(&self.build_jobs),
        }
    }

    pub fn effective_target_id(&self) -> String {
        default_build_target_id(
            self.effective_container_image(),
            &self.effective_target_arch(),
        )
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

    pub fn effective_container_image(&self) -> &'static str {
        self.container_profile.image()
    }

    pub fn effective_target_id(&self) -> String {
        default_build_target_id(
            self.effective_container_image(),
            &self.effective_target_arch(),
        )
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

    pub fn effective_build_jobs(&self) -> usize {
        match self.parallel_policy {
            ParallelPolicy::Serial => 1,
            ParallelPolicy::Adaptive => parse_build_jobs(&self.build_jobs),
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
        assert_eq!(args.packages, vec!["fastp".to_string()]);
        assert_eq!(args.stage, BuildStage::Rpm);
        assert_eq!(args.dependency_policy, DependencyPolicy::BuildHostRun);
        assert!(args.with_deps());
        assert_eq!(args.container_mode, ContainerMode::Ephemeral);
        assert_eq!(args.container_profile, BuildContainerProfile::Almalinux97);
        assert_eq!(
            args.effective_container_image(),
            "phoreus/bioconda2rpm-build:almalinux-9.7"
        );
        assert_eq!(args.parallel_policy, ParallelPolicy::Adaptive);
        assert_eq!(args.build_jobs, "4");
        assert_eq!(args.effective_build_jobs(), 4);
        assert!(args.effective_queue_workers() >= 1);
        assert!(args.effective_build_jobs() >= 1);
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
        assert_eq!(args.ui, UiMode::Auto);
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
            "--container-profile",
            "almalinux-9.7",
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
            "--container-profile",
            "fedora-43",
            "--parallel-policy",
            "serial",
            "--build-jobs",
            "12",
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
            "--ui",
            "plain",
        ])
        .expect("build overrides should parse");

        let Command::Build(args) = cli.command else {
            panic!("expected build command")
        };
        assert_eq!(args.stage, BuildStage::Spec);
        assert_eq!(args.dependency_policy, DependencyPolicy::RunOnly);
        assert!(!args.with_deps());
        assert_eq!(args.container_mode, ContainerMode::Auto);
        assert_eq!(args.container_profile, BuildContainerProfile::Fedora43);
        assert_eq!(args.parallel_policy, ParallelPolicy::Serial);
        assert_eq!(args.effective_build_jobs(), 1);
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
        assert_eq!(args.ui, UiMode::Plain);
        assert_eq!(args.effective_ui_mode(), UiMode::Plain);
    }

    #[test]
    fn build_requires_recipe_root() {
        let parse = Cli::try_parse_from(["bioconda2rpm", "build", "fastqc"]);

        assert!(parse.is_err());
    }

    #[test]
    fn build_accepts_multiple_positional_packages() {
        let cli = Cli::try_parse_from([
            "bioconda2rpm",
            "build",
            "fastqc",
            "samtools",
            "bcftools",
            "--recipe-root",
            "/recipes",
            "--container-profile",
            "almalinux-9.7",
        ])
        .expect("multi package build should parse");
        let Command::Build(args) = cli.command else {
            panic!("expected build command")
        };
        assert_eq!(
            args.packages,
            vec![
                "fastqc".to_string(),
                "samtools".to_string(),
                "bcftools".to_string()
            ]
        );
    }

    #[test]
    fn build_accepts_packages_file_without_positionals() {
        let cli = Cli::try_parse_from([
            "bioconda2rpm",
            "build",
            "--recipe-root",
            "/recipes",
            "--container-profile",
            "almalinux-9.7",
            "--packages-file",
            "/tmp/verification_software.txt",
        ])
        .expect("packages-file only build should parse");
        let Command::Build(args) = cli.command else {
            panic!("expected build command")
        };
        assert_eq!(
            args.packages_file,
            Some(PathBuf::from("/tmp/verification_software.txt"))
        );
        assert!(args.packages.is_empty());
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
        ])
        .expect("generate-priority-specs defaults should parse");

        let Command::GeneratePrioritySpecs(args) = cli.command else {
            panic!("expected generate-priority-specs subcommand");
        };
        assert_eq!(args.top_n, 10);
        assert_eq!(args.container_profile, BuildContainerProfile::Almalinux97);
        assert_eq!(
            args.effective_container_image(),
            "phoreus/bioconda2rpm-build:almalinux-9.7"
        );
        assert_eq!(args.container_engine, "docker");
        assert_eq!(args.parallel_policy, ParallelPolicy::Adaptive);
        assert_eq!(args.effective_build_jobs(), 4);
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
        assert_eq!(args.container_profile, BuildContainerProfile::Almalinux97);
        assert!(args.software_list.is_none());
        assert_eq!(args.parallel_policy, ParallelPolicy::Adaptive);
        assert_eq!(args.effective_build_jobs(), 4);
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
            "--container-profile",
            "fedora-43",
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
        assert_eq!(args.container_profile, BuildContainerProfile::Fedora43);
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

    #[test]
    fn parse_build_jobs_supports_auto_and_numeric() {
        assert!(parse_build_jobs("auto") >= 1);
        assert_eq!(parse_build_jobs("8"), 8);
        assert_eq!(parse_build_jobs("0"), 1);
        assert_eq!(parse_build_jobs("invalid"), 1);
    }

    #[test]
    fn container_profile_metadata_is_stable() {
        assert_eq!(
            BuildContainerProfile::Almalinux97.image(),
            "phoreus/bioconda2rpm-build:almalinux-9.7"
        );
        assert_eq!(
            BuildContainerProfile::Almalinux101.dockerfile_path(),
            "containers/rpm-build-images/Dockerfile.almalinux-10.1"
        );
        assert_eq!(
            BuildContainerProfile::Fedora43.image(),
            "phoreus/bioconda2rpm-build:fedora-43"
        );
    }
}
