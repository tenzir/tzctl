//! Command-line surface: argument parsing and command dispatch.

mod apply;
mod auth;
mod commands;
mod destroy;
mod insights;
mod lifecycle;
mod list;
mod node;
mod plan;
mod run;
mod status;
mod workspace;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::{self, CliOverrides, ResolvedConfig};
use crate::error::{Error, HintedError, Result};
use crate::output::OutputMode;

/// Manage managed Tenzir pipelines through the Tenzir Platform.
///
/// `tz` treats a local directory of `.tql` files as the single source of truth
/// for the pipelines on a target node.
#[derive(Debug, Parser)]
#[command(name = "tzctl", version, about, long_about = None)]
pub struct Cli {
    /// Project directory to operate in (defaults to the current directory).
    #[arg(long, global = true, value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Path to a `tenzir.toml` file, overriding discovery.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Workspace (tenant) id to target, e.g. `t-abcd1234`.
    #[arg(long, global = true, value_name = "ID")]
    pub workspace: Option<String>,

    /// Node id to target, e.g. `n-w2tjezz3`.
    #[arg(long, global = true, value_name = "ID")]
    pub node: Option<String>,

    /// Assume "yes" to confirmation prompts for destructive actions.
    #[arg(long, global = true)]
    pub yes: bool,

    /// Output format.
    #[arg(long, global = true, value_enum, default_value_t = OutputMode::Text)]
    pub output: OutputMode,

    /// Increase logging verbosity (repeat for more: `-v`, `-vv`).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Override the platform API endpoint base URL.
    #[arg(long, global = true, value_name = "URL")]
    pub api_endpoint: Option<String>,

    /// Use a pre-supplied OIDC id token, bypassing login.
    #[arg(long, global = true, value_name = "TOKEN")]
    pub token: Option<String>,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Authenticate to the Tenzir Platform.
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Act on individual pipelines on the target node.
    #[command(subcommand)]
    Pipeline(PipelineCommand),
    /// Run a `.tql` file on the target node and stream results to stdout.
    Run {
        /// Path to the `.tql` file.
        file: PathBuf,
    },
    /// Sync the local project directory to the platform.
    #[command(subcommand)]
    Project(ProjectCommand),
    /// Manage and select workspaces.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Manage and select nodes.
    #[command(subcommand)]
    Node(NodeCommand),
}

/// `tz auth` subcommands: credential lifecycle (no node/pipeline I/O).
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Authenticate to the Tenzir Platform.
    Login(LoginArgs),
    /// Remove cached credentials.
    Logout,
}

/// `tz pipeline` subcommands: imperative actions on individual pipelines.
#[derive(Debug, Subcommand)]
pub enum PipelineCommand {
    /// List pipelines on the target node.
    List,
    /// Create a pipeline from a `.tql` file.
    Create {
        /// Path to the `.tql` file.
        file: PathBuf,
    },
    /// Update (set) a pipeline's definition in place from a `.tql` file.
    Set {
        /// Path to the `.tql` file.
        file: PathBuf,
    },
    /// Delete a pipeline by name.
    Delete {
        /// The pipeline name.
        name: String,
    },
    /// Start (or resume) a pipeline.
    Start {
        /// The pipeline name.
        name: String,
    },
    /// Stop a pipeline (resets state).
    Stop {
        /// The pipeline name.
        name: String,
    },
    /// Inspect a pipeline's diagnostics and performance insights.
    Status {
        /// The pipeline name.
        name: String,
        /// Time range for diagnostics and activity.
        #[arg(long, value_enum, default_value = "1d")]
        range: crate::status::StatusRange,
        /// Maximum number of recent diagnostics to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Show per-operator metrics: CPU, events/s, bytes/s, batches/s, queue.
    ///
    /// Samples the first 10 live metric emissions of the running pipeline.
    Insights {
        /// The pipeline name.
        name: String,
    },
}

/// `tz project` subcommands: declarative directory-as-source-of-truth sync.
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Reconcile the node's pipelines with the project.
    Apply {
        /// Also delete orphaned (actual-only) pipelines.
        #[arg(long)]
        prune: bool,
        /// Show the plan and exit without applying (like `tz project plan`).
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove all project-defined pipelines from the node.
    Destroy,
}

/// Arguments for `tz auth login`.
#[derive(Debug, clap::Args)]
pub struct LoginArgs {
    /// Force the interactive device-code flow.
    #[arg(long, conflicts_with = "non_interactive")]
    pub interactive: bool,
    /// Force the non-interactive client-credentials flow.
    #[arg(long)]
    pub non_interactive: bool,
}

/// `tz workspace` subcommands.
#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// List available workspaces.
    List,
    /// Select the default workspace and cache its workspace key.
    Select {
        /// Workspace id, name, or 1-based index from `tz workspace list`.
        query: String,
    },
}

/// `tz node` subcommands.
#[derive(Debug, Subcommand)]
pub enum NodeCommand {
    /// List available nodes.
    List,
    /// Select the default node.
    Select,
}

/// Parse arguments, initialize logging, resolve configuration, and dispatch.
///
/// Returns the process exit code on success.
pub async fn run() -> std::result::Result<u8, HintedError> {
    let cli = Cli::parse();
    crate::output::init_tracing(cli.verbose);
    let resolved = resolve_config(&cli)?;
    dispatch(&cli, &resolved).await
}

/// Resolve effective configuration from the parsed CLI, env, and config file.
fn resolve_config(cli: &Cli) -> Result<ResolvedConfig> {
    let start = match &cli.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()
            .map_err(|e| Error::Config(format!("cannot determine current directory: {e}")))?,
    };
    let config_path = match &cli.config {
        Some(path) => Some(path.clone()),
        None => config::discover(&start),
    };
    let (config, config_dir) = match &config_path {
        Some(path) => {
            let config = config::load(path)?;
            let dir = path.parent().map(|p| p.to_path_buf());
            (config, dir)
        }
        None => (config::Config::default(), None),
    };
    let overrides = CliOverrides {
        api_endpoint: cli.api_endpoint.clone(),
        workspace: cli.workspace.clone(),
        node: cli.node.clone(),
    };
    let mut resolved = config::resolve(&overrides, &config, config_dir)?;
    // When no config file was found, the project root is the start directory
    // (`--dir` or the current directory) rather than ".".
    if resolved.config_dir.is_none() {
        resolved.project_root = start;
    }
    Ok(resolved)
}

/// Dispatch a parsed command to its handler, returning the exit code.
async fn dispatch(cli: &Cli, config: &ResolvedConfig) -> std::result::Result<u8, HintedError> {
    let token_sources = crate::auth::TokenSources::from_config(config, cli.token.clone());
    commands::handle(&cli.command, config, token_sources, cli.output, cli.yes).await
}

/// Resolve the `(workspace, node)` target from config, with hinted errors.
fn resolve_target(
    config: &ResolvedConfig,
) -> std::result::Result<(crate::model::TenantId, crate::model::NodeId), HintedError> {
    let workspace = config
        .workspace
        .clone()
        .map(crate::model::TenantId::from)
        .ok_or_else(|| {
            HintedError::new(Error::Config("no workspace configured".to_string())).with_hint(
                "set `[workspace] id` in tenzir.toml, pass --workspace, or run \
                 `tz workspace select`",
            )
        })?;
    let node = config
        .node
        .clone()
        .map(crate::model::NodeId::from)
        .ok_or_else(|| {
            HintedError::new(Error::Config("no node configured".to_string()))
                .with_hint("set `[node] id` in tenzir.toml, pass --node, or run `tz node list`")
        })?;
    Ok((workspace, node))
}

/// Build an authenticated platform client, resolving the `id_token`.
async fn platform_client(
    config: &ResolvedConfig,
    sources: crate::auth::TokenSources,
) -> std::result::Result<crate::client::PlatformApi, HintedError> {
    let authenticator = crate::auth::Authenticator::new(config, sources)?;
    let id_token = authenticator.load_id_token().await.map_err(|e| {
        HintedError::new(e).with_hint("run `tz auth login` to authenticate to the platform")
    })?;
    let client = crate::client::PlatformApi::new(config, id_token)?;
    Ok(client)
}

/// Prompt the user for a yes/no confirmation on stderr (default: no).
///
/// Returns `true` only on an explicit `y`/`yes`.
pub(super) fn confirm(prompt: &str) -> std::result::Result<bool, HintedError> {
    use std::io::{self, Write};
    eprint!("{prompt} [y/N] ");
    io::stderr()
        .flush()
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn verify_cli() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_auth_subcommands() {
        let cli = Cli::try_parse_from(["tz", "auth", "login"]).unwrap();
        assert!(matches!(cli.command, Command::Auth(AuthCommand::Login(_))));
        let cli = Cli::try_parse_from(["tz", "auth", "logout"]).unwrap();
        assert!(matches!(cli.command, Command::Auth(AuthCommand::Logout)));
    }

    #[test]
    fn parses_pipeline_subcommands() {
        let cli = Cli::try_parse_from(["tz", "pipeline", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Pipeline(PipelineCommand::List)
        ));
        let cli = Cli::try_parse_from(["tz", "pipeline", "create", "p.tql"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Pipeline(PipelineCommand::Create { .. })
        ));
        let cli = Cli::try_parse_from(["tz", "pipeline", "stop", "p"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Pipeline(PipelineCommand::Stop { name }) if name == "p"
        ));
    }

    #[test]
    fn parses_project_subcommands() {
        let cli = Cli::try_parse_from(["tz", "project", "apply", "--prune"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Project(ProjectCommand::Apply { prune: true, .. })
        ));
    }

    #[test]
    fn rejects_old_flat_forms() {
        assert!(Cli::try_parse_from(["tz", "login"]).is_err());
        assert!(Cli::try_parse_from(["tz", "list"]).is_err());
        assert!(Cli::try_parse_from(["tz", "apply"]).is_err());
    }
}
