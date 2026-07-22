//! Command-line surface: argument parsing and command dispatch.

mod alert;
mod apply;
mod auth;
mod commands;
mod org;
mod destroy;
mod insights;
mod lifecycle;
mod list;
mod node;
mod plan;
mod pull;
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
    /// Run TQL on the target node and stream results to stdout.
    Run(RunArgs),
    /// Sync the local project directory to the platform.
    #[command(subcommand)]
    Project(ProjectCommand),
    /// Manage and select workspaces.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Manage the current organization.
    #[command(subcommand)]
    Org(OrgCommand),
    /// Manage node-disconnect alerts.
    #[command(subcommand)]
    Alert(AlertCommand),
    /// Manage and select nodes.
    #[command(subcommand)]
    Node(NodeCommand),
}

/// `tz org` subcommands: act on the user's current organization.
#[derive(Debug, Subcommand)]
pub enum OrgCommand {
    /// Show information about the current organization.
    Info,
    /// Create a new organization.
    Create {
        /// The organization name.
        name: String,
    },
    /// Create a new org-owned workspace.
    CreateWorkspace {
        /// The workspace name (defaults to a timestamped name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Delete the current organization.
    Delete,
    /// Create an invitation for the current organization.
    Invite {
        /// The role granted to the invitee: `member` (default) or `admin`.
        #[arg(long, default_value = "member")]
        role: String,
        /// An optional label to attach to the invitation.
        #[arg(long)]
        label: Option<String>,
    },
    /// Leave the current organization.
    Leave,
    /// List invitations for the current organization.
    ListInvitations,
    /// Revoke an invitation by id.
    RevokeInvitation {
        /// The invitation id to revoke.
        invitation_id: String,
    },
    /// Redeem an invitation token to join an organization.
    RedeemInvitation {
        /// The invitation token.
        token: String,
    },
    /// Remove a member from the current organization.
    RemoveMember {
        /// The user id to remove.
        user_id: String,
    },
}

/// `tz alert` subcommands: node-disconnect alerts for the current workspace.
#[derive(Debug, Subcommand)]
pub enum AlertCommand {
    /// Add a new alert.
    Add {
        /// The node id or name to monitor.
        node: String,
        /// The idle duration before triggering, e.g. `30s`, `5m`, `1h`.
        duration: String,
        /// The webhook URL to call when the alert triggers.
        webhook_url: String,
        /// The JSON body to send with the webhook (defaults to a message).
        webhook_body: Option<String>,
    },
    /// Delete an alert by id.
    Delete {
        /// The alert id to delete.
        alert_id: String,
    },
    /// List all configured alerts.
    List,
}

/// `tz auth` subcommands: credential lifecycle (no node/pipeline I/O).
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Authenticate to the Tenzir Platform.
    Login(LoginArgs),
    /// Remove cached credentials.
    Logout,
    /// Print the current auth token.
    Token,
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
        /// Render live insights as new metrics arrive, until Ctrl-C.
        #[arg(long)]
        watch: bool,
        /// Show additional columns (operator id and input capacity).
        #[arg(long)]
        full: bool,
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
    /// Fetch pipeline definitions from the platform into local files.
    Pull {
        /// Also delete local files with no matching pipeline on the platform.
        #[arg(long)]
        prune: bool,
        /// Show what would change and exit without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove all project-defined pipelines from the node.
    Destroy,
}

/// Arguments for `tz run`: exactly one of `--file`/`--code` is required.
#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("source").required(true).args(["file", "code"])))]
pub struct RunArgs {
    /// Path to a `.tql` file to run.
    #[arg(long, short = 'f', value_name = "FILE")]
    pub file: Option<PathBuf>,
    /// TQL code to run directly, instead of a file.
    #[arg(long, short = 'c', value_name = "TQL")]
    pub code: Option<String>,
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
    /// Create an invitation for the current workspace.
    Invite {
        /// The role granted to the invitee: `member` (default) or `admin`.
        #[arg(long, default_value = "member")]
        role: String,
        /// An optional label to attach to the invitation.
        #[arg(long)]
        label: Option<String>,
    },
    /// List invitations for the current workspace.
    ListInvitations,
    /// Revoke an invitation for the current workspace by id.
    RevokeInvitation {
        /// The invitation id to revoke.
        invitation_id: String,
    },
    /// Redeem an invitation token to join a workspace.
    RedeemInvitation {
        /// The invitation token.
        token: String,
    },
    /// Rename the current workspace.
    Rename {
        /// The new workspace name.
        name: String,
    },
}

/// `tz node` subcommands.
#[derive(Debug, Subcommand)]
pub enum NodeCommand {
    /// List available nodes.
    List,
    /// Send a ping request to a node and measure the response time.
    Ping {
        /// The node id or name.
        #[arg(id = "ping_node", value_name = "NODE")]
        node: String,
    },
    /// Create a new node.
    Create {
        /// The name of the newly created node.
        #[arg(long)]
        name: Option<String>,
    },
    /// Download the configuration for an existing node.
    Config {
        /// The node id or name.
        #[arg(id = "config_node", value_name = "NODE")]
        node: String,
        /// Where to write the config file. Use `-` for stdout.
        #[arg(long = "file", short = 'o', value_name = "FILE")]
        file: Option<String>,
        /// The format of the downloaded config file.
        #[arg(long, value_enum, default_value_t = NodeConfigFormat::Docker)]
        format: NodeConfigFormat,
    },
    /// Delete a node.
    Delete {
        /// The node id or name.
        #[arg(id = "delete_node", value_name = "NODE")]
        node: String,
    },
    /// Perform an HTTP request against a node's REST API.
    Proxy {
        /// The node id or name.
        #[arg(id = "proxy_node", value_name = "NODE")]
        node: String,
        /// The node REST API endpoint, e.g. `pipeline/list`.
        endpoint: String,
        /// An optional JSON request body.
        body: Option<String>,
    },
    /// Create a temporary node and run it in a local `docker compose` stack.
    Run {
        /// The name of the newly created node.
        #[arg(long)]
        name: Option<String>,
        /// The docker image to use for the newly created node.
        #[arg(long)]
        image: Option<String>,
    },
}

/// The configuration format for `tz node config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum NodeConfigFormat {
    /// A `docker compose` configuration.
    Docker,
    /// A `tenzir` client configuration.
    Tenzir,
    /// A `tenzir-node` configuration.
    TenzirNode,
}

impl NodeConfigFormat {
    /// The wire string expected by `generate-client-config`.
    pub fn as_wire(self) -> &'static str {
        match self {
            NodeConfigFormat::Docker => "docker",
            NodeConfigFormat::Tenzir => "tenzir",
            NodeConfigFormat::TenzirNode => "tenzir-node",
        }
    }
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
        let cli = Cli::try_parse_from(["tz", "auth", "token"]).unwrap();
        assert!(matches!(cli.command, Command::Auth(AuthCommand::Token)));
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
        let cli = Cli::try_parse_from(["tz", "pipeline", "insights", "p", "--watch"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Pipeline(PipelineCommand::Insights { watch: true, .. })
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
        let cli = Cli::try_parse_from(["tz", "project", "pull", "--prune", "--dry-run"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Project(ProjectCommand::Pull {
                prune: true,
                dry_run: true
            })
        ));
    }

    #[test]
    fn parses_node_subcommands() {
        let cli = Cli::try_parse_from(["tz", "node", "list"]).unwrap();
        assert!(matches!(cli.command, Command::Node(NodeCommand::List)));
        let cli = Cli::try_parse_from(["tz", "node", "ping", "edge"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Node(NodeCommand::Ping { node }) if node == "edge"
        ));
        let cli = Cli::try_parse_from(["tz", "node", "create", "--name", "n1"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Node(NodeCommand::Create { name: Some(n) }) if n == "n1"
        ));
        let cli =
            Cli::try_parse_from(["tz", "node", "config", "edge", "--format", "tenzir-node"])
                .unwrap();
        assert!(matches!(
            cli.command,
            Command::Node(NodeCommand::Config {
                format: NodeConfigFormat::TenzirNode,
                ..
            })
        ));
        let cli = Cli::try_parse_from(["tz", "node", "delete", "edge"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Node(NodeCommand::Delete { node }) if node == "edge"
        ));
        let cli =
            Cli::try_parse_from(["tz", "node", "proxy", "edge", "pipeline/list", "{}"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Node(NodeCommand::Proxy { endpoint, body: Some(b), .. })
                if endpoint == "pipeline/list" && b == "{}"
        ));
        let cli = Cli::try_parse_from(["tz", "node", "run", "--image", "tenzir/tenzir:latest"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Command::Node(NodeCommand::Run { image: Some(i), .. }) if i == "tenzir/tenzir:latest"
        ));
    }

    #[test]
    fn parses_workspace_subcommands() {
        let cli = Cli::try_parse_from(["tz", "workspace", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Workspace(WorkspaceCommand::List)
        ));
        let cli =
            Cli::try_parse_from(["tz", "workspace", "invite", "--role", "admin"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Workspace(WorkspaceCommand::Invite { role, .. }) if role == "admin"
        ));
        let cli = Cli::try_parse_from(["tz", "workspace", "list-invitations"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Workspace(WorkspaceCommand::ListInvitations)
        ));
        let cli =
            Cli::try_parse_from(["tz", "workspace", "revoke-invitation", "i-1"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Workspace(WorkspaceCommand::RevokeInvitation { invitation_id }) if invitation_id == "i-1"
        ));
        let cli = Cli::try_parse_from(["tz", "workspace", "redeem-invitation", "tok"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Workspace(WorkspaceCommand::RedeemInvitation { token }) if token == "tok"
        ));
        let cli = Cli::try_parse_from(["tz", "workspace", "rename", "prod"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Workspace(WorkspaceCommand::Rename { name }) if name == "prod"
        ));
    }

    #[test]
    fn parses_org_subcommands() {
        let cli = Cli::try_parse_from(["tz", "org", "info"]).unwrap();
        assert!(matches!(cli.command, Command::Org(OrgCommand::Info)));
        let cli = Cli::try_parse_from(["tz", "org", "create", "acme"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Org(OrgCommand::Create { name }) if name == "acme"
        ));
        let cli =
            Cli::try_parse_from(["tz", "org", "create-workspace", "--name", "w"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Org(OrgCommand::CreateWorkspace { name: Some(n) }) if n == "w"
        ));
        let cli = Cli::try_parse_from(["tz", "org", "remove-member", "u-1"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Org(OrgCommand::RemoveMember { user_id }) if user_id == "u-1"
        ));
        let cli = Cli::try_parse_from(["tz", "org", "invite", "--role", "admin"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Org(OrgCommand::Invite { role, .. }) if role == "admin"
        ));
    }

    #[test]
    fn parses_alert_subcommands() {
        let cli = Cli::try_parse_from(["tz", "alert", "list"]).unwrap();
        assert!(matches!(cli.command, Command::Alert(AlertCommand::List)));
        let cli = Cli::try_parse_from([
            "tz", "alert", "add", "edge", "30s", "https://hook",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Alert(AlertCommand::Add { node, duration, webhook_url, webhook_body: None })
                if node == "edge" && duration == "30s" && webhook_url == "https://hook"
        ));
        let cli = Cli::try_parse_from(["tz", "alert", "delete", "a-1"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Alert(AlertCommand::Delete { alert_id }) if alert_id == "a-1"
        ));
    }

    #[test]
    fn rejects_old_flat_forms() {
        assert!(Cli::try_parse_from(["tz", "login"]).is_err());
        assert!(Cli::try_parse_from(["tz", "list"]).is_err());
        assert!(Cli::try_parse_from(["tz", "apply"]).is_err());
    }
}
