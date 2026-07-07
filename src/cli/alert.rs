//! Handlers for the `tz alert` command group.

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::{Error, HintedError};
use crate::output::{self, OutputMode};
use crate::table::Table;

/// Parse a human duration like `30s`, `5m`, `1h`, `2d`, or plain seconds.
///
/// Returns the duration in whole seconds.
fn parse_duration(input: &str) -> Result<u64, HintedError> {
    let s = input.trim();
    let invalid = || {
        HintedError::new(Error::Config(format!("invalid duration: {input:?}")))
            .with_hint("use a value like 30s, 5m, 1h, or 2d")
    };
    let (value, unit_secs) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1u64),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86400),
        Some(c) if c.is_ascii_digit() => (s, 1),
        _ => return Err(invalid()),
    };
    let n: u64 = value.trim().parse().map_err(|_| invalid())?;
    if n == 0 {
        return Err(invalid());
    }
    Ok(n * unit_secs)
}

/// Handle `tz alert add`.
pub async fn add(
    config: &ResolvedConfig,
    sources: TokenSources,
    node: &str,
    duration: &str,
    webhook_url: &str,
    webhook_body: Option<&str>,
) -> Result<(), HintedError> {
    let tenant = super::workspace::current_workspace(config)?;
    let seconds = parse_duration(duration)?;
    let default_body =
        format!("{{\"text\": \"Node $NODE_NAME disconnected for more than {duration}\"}}");
    let body = webhook_body.unwrap_or(&default_body);
    // Validate the webhook body is valid JSON before sending.
    serde_json::from_str::<serde_json::Value>(body).map_err(|e| {
        HintedError::new(Error::Config(format!("webhook body must be valid JSON: {e}")))
    })?;

    let client = super::platform_client(config, sources).await?;
    let resolved = client
        .resolve_node(&tenant, node)
        .await
        .map_err(HintedError::new)?;
    client
        .session()
        .alert_add(
            &tenant,
            resolved.node_id.as_str(),
            seconds,
            webhook_url,
            body,
        )
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Added alert for node {}",
        crate::symbols::OK.green().bold(),
        resolved.name.bold()
    );
    Ok(())
}

/// Handle `tz alert delete <alert_id>`.
pub async fn delete(
    config: &ResolvedConfig,
    sources: TokenSources,
    alert_id: &str,
    yes: bool,
) -> Result<(), HintedError> {
    if !yes && !super::confirm(&format!("Delete alert {alert_id}?"))? {
        println!("Aborted.");
        return Ok(());
    }
    let tenant = super::workspace::current_workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    client
        .session()
        .alert_delete(&tenant, alert_id)
        .await
        .map_err(HintedError::new)?;
    println!(
        "{} Deleted alert {}",
        crate::symbols::OK.green().bold(),
        alert_id.bold()
    );
    Ok(())
}

/// Handle `tz alert list`.
pub async fn list(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
) -> Result<(), HintedError> {
    let tenant = super::workspace::current_workspace(config)?;
    let client = super::platform_client(config, sources).await?;
    let alerts = client
        .session()
        .alert_list(&tenant)
        .await
        .map_err(HintedError::new)?;
    output::render(output, &alerts, || render_table(&alerts))
        .map_err(|e| HintedError::new(Error::Other(e.into())))?;
    Ok(())
}

/// Render the alert list as a table.
fn render_table(alerts: &[crate::client::Alert]) -> String {
    if alerts.is_empty() {
        return "No alerts configured.".to_string();
    }
    let mut table = Table::new(["ALERT ID", "NODE", "DURATION", "WEBHOOK URL"]);
    for a in alerts {
        let duration = a
            .duration
            .map(|d| format!("{}s", d as u64))
            .unwrap_or_default();
        table.row([
            a.id.clone(),
            a.node_id.clone().unwrap_or_default(),
            duration,
            a.webhook_url.clone().unwrap_or_default(),
        ]);
    }
    table.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("30s").unwrap(), 30);
        assert_eq!(parse_duration("5m").unwrap(), 300);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
        assert_eq!(parse_duration("2d").unwrap(), 172800);
        assert_eq!(parse_duration("45").unwrap(), 45);
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("").is_err());
    }
}
