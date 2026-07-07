//! Command handlers.
//!
//! `login`/`logout` are implemented; the remaining handlers are stubs that
//! report "not implemented" until their stage lands.

use super::{AuthCommand, Command, NodeCommand, PipelineCommand, ProjectCommand, WorkspaceCommand};
use crate::auth::TokenSources;
use crate::config::ResolvedConfig;
use crate::error::HintedError;
use crate::output::OutputMode;

/// Dispatch a command to its handler, returning the process exit code.
///
/// Most commands exit `0`; `tz project plan` exits `2` when changes are pending.
pub async fn handle(
    command: &Command,
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    yes: bool,
) -> Result<u8, HintedError> {
    match command {
        Command::Auth(cmd) => match cmd {
            AuthCommand::Login(args) => zero(super::auth::login(config, sources, args).await),
            AuthCommand::Logout => zero(super::auth::logout(config, sources).await),
        },
        Command::Pipeline(cmd) => match cmd {
            PipelineCommand::List => zero(super::list::run(config, sources, output).await),
            PipelineCommand::Create { file } => {
                zero(super::lifecycle::create(config, sources, output, file).await)
            }
            PipelineCommand::Set { file } => {
                zero(super::lifecycle::set(config, sources, output, file).await)
            }
            PipelineCommand::Delete { name } => {
                zero(super::lifecycle::delete(config, sources, output, name, yes).await)
            }
            PipelineCommand::Start { name } => {
                zero(super::lifecycle::start(config, sources, output, name).await)
            }
            PipelineCommand::Stop { name } => {
                zero(super::lifecycle::stop(config, sources, output, name).await)
            }
            PipelineCommand::Status { name, range, limit } => {
                zero(super::status::run(config, sources, output, name, *range, *limit).await)
            }
            PipelineCommand::Insights { name, watch } => {
                zero(super::insights::run(config, sources, output, name, *watch).await)
            }
        },
        Command::Run(args) => zero(super::run::run(config, sources, output, args).await),
        Command::Project(cmd) => match cmd {
            ProjectCommand::Apply { prune, dry_run } => {
                super::apply::run(config, sources, output, *prune, *dry_run, yes).await
            }
            ProjectCommand::Pull { prune, dry_run } => {
                super::pull::run(config, sources, output, *prune, *dry_run, yes).await
            }
            ProjectCommand::Destroy => super::destroy::run(config, sources, output, yes).await,
        },
        Command::Workspace(cmd) => match cmd {
            WorkspaceCommand::List => zero(super::workspace::list(config, sources, output).await),
            WorkspaceCommand::Select { query } => {
                zero(super::workspace::select(config, sources, query).await)
            }
        },
        Command::Node(cmd) => match cmd {
            NodeCommand::List => zero(super::node::list(config, sources, output).await),
            NodeCommand::Ping { node } => {
                zero(super::node::ping(config, sources, node).await)
            }
            NodeCommand::Create { name } => {
                zero(super::node::create(config, sources, name.as_deref()).await)
            }
            NodeCommand::Config { node, file, format } => {
                zero(super::node::config(config, sources, node, *format, file.as_deref()).await)
            }
            NodeCommand::Delete { node } => {
                zero(super::node::delete(config, sources, node, yes).await)
            }
            NodeCommand::Proxy {
                node,
                endpoint,
                body,
            } => zero(super::node::proxy(config, sources, node, endpoint, body.as_deref()).await),
            NodeCommand::Run { name, image } => {
                zero(super::node::run(config, sources, name.as_deref(), image.as_deref()).await)
            }
        },
    }
}

/// Map a unit-returning handler result to exit code `0`.
fn zero(result: Result<(), HintedError>) -> Result<u8, HintedError> {
    result.map(|()| 0)
}
