//! Handlers for the `tz node` command group.

use std::io::Write;
use std::time::Instant;

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::cli::NodeConfigFormat;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::model::{Node, TenantId};
use crate::output::{self, OutputMode};
use crate::table::{Align, Table};

/// Resolve the target workspace from config, with a hinted error.
fn workspace(config: &ResolvedConfig) -> Result<TenantId, HintedError> {
    config
        .workspace
        .clone()
        .map(TenantId::from)
        .ok_or_else(|| {
            HintedError::new(Error::Config("no workspace configured".to_string())).with_hint(
                "set `[workspace] id` in tenzir.toml, pass --workspace, or run \
                 `tz workspace select`",
            )
        })
}

/// Handle `tz node list`.
pub async fn list(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
) -> Result<(), HintedError> {
    let tenant = workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let nodes = client.list_nodes(&tenant).await.map_err(HintedError::new)?;
    output::render(output, &nodes, || render_table(&nodes))
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    Ok(())
}

/// Handle `tz node ping <node>` — measure the node's proxy round-trip time.
pub async fn ping(
    config: &ResolvedConfig,
    sources: TokenSources,
    node: &str,
) -> Result<(), HintedError> {
    let tenant = workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let resolved = client
        .resolve_node(&tenant, node)
        .await
        .map_err(super::list::disconnected_hint)?;
    let start = Instant::now();
    let result: Result<serde_json::Value, Error> = client
        .node_proxy(&tenant, &resolved.node_id, "ping", &serde_json::json!({}))
        .await;
    let elapsed = start.elapsed();
    match result {
        Ok(_) => {
            println!(
                "{} {} responded in {:.0?}",
                crate::symbols::OK.green().bold(),
                resolved.name.bold(),
                elapsed
            );
            Ok(())
        }
        Err(Error::NodeDisconnected) => {
            println!("{} {} is disconnected", "✗".red().bold(), resolved.name);
            Ok(())
        }
        Err(e) => Err(super::list::disconnected_hint(e)),
    }
}

/// Handle `tz node create [--name]` — create a new node.
pub async fn create(
    config: &ResolvedConfig,
    sources: TokenSources,
    name: Option<&str>,
) -> Result<(), HintedError> {
    let tenant = workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let node_name = name.map(str::to_string).unwrap_or_else(default_node_name);
    let cfg = client
        .generate_client_config(&tenant, "docker", None, Some(&node_name))
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Created node {} ({})",
        crate::symbols::OK.green().bold(),
        node_name.bold(),
        cfg.node_id
    );
    Ok(())
}

/// Handle `tz node config <node> [-o file] [--format]`.
pub async fn config(
    config: &ResolvedConfig,
    sources: TokenSources,
    node: &str,
    format: NodeConfigFormat,
    output_file: Option<&str>,
) -> Result<(), HintedError> {
    let tenant = workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let resolved = client
        .resolve_node(&tenant, node)
        .await
        .map_err(HintedError::new)?;
    let cfg = client
        .generate_client_config(&tenant, format.as_wire(), Some(&resolved.node_id), None)
        .await
        .map_err(HintedError::new)?;
    // Determine the output destination: the explicit `-o`, else the filename
    // the platform suggests, else stdout.
    let dest = output_file
        .map(str::to_string)
        .or(cfg.filename.clone())
        .unwrap_or_else(|| "-".to_string());
    if dest == "-" {
        print!("{}", cfg.contents);
    } else {
        std::fs::write(&dest, &cfg.contents)
            .map_err(|e| HintedError::new(Error::Other(e.into())))?;
        println!(
            "{} Wrote config for node {} to {}",
            crate::symbols::OK.green().bold(),
            cfg.node_id,
            dest.bold()
        );
    }
    Ok(())
}

/// Handle `tz node delete <node>`.
pub async fn delete(
    config: &ResolvedConfig,
    sources: TokenSources,
    node: &str,
    yes: bool,
) -> Result<(), HintedError> {
    let tenant = workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let resolved = client
        .resolve_node(&tenant, node)
        .await
        .map_err(HintedError::new)?;
    if !yes && !super::confirm(&format!("Delete node {} ({})?", resolved.name, resolved.node_id))? {
        println!("Aborted.");
        return Ok(());
    }
    client
        .delete_node(&tenant, &resolved.node_id)
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Deleted node {} ({})",
        crate::symbols::OK.green().bold(),
        resolved.name.bold(),
        resolved.node_id
    );
    Ok(())
}

/// Handle `tz node proxy <node> <endpoint> [<body>]`.
pub async fn proxy(
    config: &ResolvedConfig,
    sources: TokenSources,
    node: &str,
    endpoint: &str,
    body: Option<&str>,
) -> Result<(), HintedError> {
    let tenant = workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let resolved = client
        .resolve_node(&tenant, node)
        .await
        .map_err(super::list::disconnected_hint)?;
    let json_body: serde_json::Value = serde_json::from_str(body.unwrap_or("{}"))
        .map_err(|e| HintedError::new(Error::Config(format!("invalid JSON body: {e}"))))?;
    let response: serde_json::Value = client
        .node_proxy(
            &tenant,
            &resolved.node_id,
            endpoint.trim_start_matches('/'),
            &json_body,
        )
        .await
        .map_err(super::list::disconnected_hint)?;
    println!("{response}");
    Ok(())
}

/// Handle `tz node run [--name] [--image]` — run a temporary local node.
///
/// Creates a node in the current workspace, writes its `docker compose` config
/// to a temp file, runs `docker compose up`, and deletes the node on exit.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    name: Option<&str>,
    image: Option<&str>,
) -> Result<(), HintedError> {
    let tenant = workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let node_name = name.map(str::to_string).unwrap_or_else(default_node_name);
    let cfg = client
        .generate_client_config(&tenant, "docker", None, Some(&node_name))
        .await
        .map_err(HintedError::new)?;

    // Optionally swap the container image in the generated compose file.
    let mut contents = cfg.contents.clone();
    if let Some(image) = image {
        contents = swap_image(&contents, image);
    }

    // Write the compose file to a temp file.
    let mut temp = tempfile::Builder::new()
        .prefix("tz-node-")
        .suffix(".yaml")
        .tempfile()
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    temp.write_all(contents.as_bytes())
        .and_then(|()| temp.flush())
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    let temp_path = temp.path().to_path_buf();

    println!("Running temporary Tenzir node {}", node_name.bold());
    let status = std::process::Command::new("docker")
        .args(["compose", "-f"])
        .arg(&temp_path)
        .arg("up")
        .status();

    // Always clean up the node, whatever happened to docker compose.
    println!("Removing node and config file");
    let _ = client.delete_node(&tenant, &cfg.node_id.clone().into()).await;

    match status {
        Ok(_) => Ok(()),
        Err(e) => Err(HintedError::new(Error::Other(e.into()))
            .with_hint("`docker compose` must be installed and on your PATH")),
    }
}

/// Build a default node name from the current UTC time.
fn default_node_name() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "node-{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

/// Replace the first `image: tenzir/tenzir...` line with a custom `image`.
fn swap_image(contents: &str, image: &str) -> String {
    let mut replaced = false;
    contents
        .lines()
        .map(|line| {
            if !replaced {
                let trimmed = line.trim_start();
                if let Some(rest) = trimmed.strip_prefix("image:") {
                    if rest.trim_start().starts_with("tenzir/tenzir") {
                        replaced = true;
                        let indent = &line[..line.len() - trimmed.len()];
                        return format!("{indent}image: {image}");
                    }
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render the node list as a numbered table with a connection indicator.
fn render_table(nodes: &[Node]) -> String {
    if nodes.is_empty() {
        return "No nodes in this workspace.".to_string();
    }
    let mut table = Table::new(["#", "NAME", "NODE ID", "STATUS"]).align(0, Align::Right);
    for (i, node) in nodes.iter().enumerate() {
        let status = if node.connected {
            "connected".green().to_string()
        } else {
            "disconnected".red().to_string()
        };
        table.row([
            (i + 1).to_string(),
            node.name.clone(),
            node.node_id.to_string(),
            status,
        ]);
    }
    table.render()
}
