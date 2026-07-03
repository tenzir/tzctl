//! Handler for `tz pipeline status <name>` — diagnostics and performance
//! insights for a single pipeline.

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::HintedError;
use crate::model::{NodeId, TenantId};
use crate::output::{self, OutputMode};
use crate::status::{
    Activity, Diagnostic, DiagnosticCounts, FlowSummary, PipelineStatus, StatusRange,
    activity_query, diagnostics_query, format_bytes, format_rate, sparkline,
};

/// Handle `tz pipeline status <name>`.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    name: &str,
    range: StatusRange,
    limit: usize,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let client = super::platform_client(config, sources).await?;

    let remote = client
        .resolve_pipeline(&workspace, &node, name)
        .await
        .map_err(super::list::disconnected_hint)?;

    // Fetch diagnostics and activity concurrently; degrade gracefully if the
    // activity query fails (e.g. an older node without `pipeline::activity`).
    let (diagnostics, activity) = tokio::join!(
        fetch_diagnostics(&client, &workspace, &node, remote.id.as_str(), range, limit),
        fetch_activity(&client, &workspace, &node, range),
    );

    let diagnostics = diagnostics.map_err(super::list::disconnected_hint)?;
    let counts = DiagnosticCounts::from_diagnostics(&diagnostics);

    let (ingress, egress) = match &activity {
        Ok(activity) => activity
            .pipelines
            .iter()
            .find(|p| p.id == remote.id.as_str())
            .map(|p| {
                (
                    Some(FlowSummary::from_rates(&p.ingress)),
                    Some(FlowSummary::from_rates(&p.egress)),
                )
            })
            .unwrap_or((None, None)),
        Err(_) => (None, None),
    };

    let status = PipelineStatus {
        name: remote.name.clone(),
        id: remote.id.to_string(),
        state: remote.state.to_string(),
        error: remote.error.clone(),
        diagnostics: counts,
        recent_diagnostics: diagnostics,
        ingress,
        egress,
    };

    output::render(output, &status, || {
        render_text(&status, range, activity.is_err())
    })
    .map_err(|e| HintedError::new(crate::error::Error::Other(e.into())))?;
    Ok(())
}

/// Fetch and parse the recent diagnostics for a pipeline.
async fn fetch_diagnostics(
    client: &impl PlatformClient,
    workspace: &TenantId,
    node: &NodeId,
    pipeline_id: &str,
    range: StatusRange,
    limit: usize,
) -> Result<Vec<Diagnostic>, crate::error::Error> {
    let query = diagnostics_query(pipeline_id, range, limit);
    let events = client.run_query(workspace, node, &query, limit).await?;
    Ok(events
        .into_iter()
        .filter_map(|e| serde_json::from_value(e).ok())
        .collect())
}

/// Fetch and parse the `pipeline::activity` response.
async fn fetch_activity(
    client: &impl PlatformClient,
    workspace: &TenantId,
    node: &NodeId,
    range: StatusRange,
) -> Result<Activity, crate::error::Error> {
    let query = activity_query(range);
    let events = client.run_query(workspace, node, &query, 1).await?;
    let value = events
        .into_iter()
        .next()
        .ok_or_else(|| crate::error::Error::Platform("no activity data returned".to_string()))?;
    serde_json::from_value(value)
        .map_err(|e| crate::error::Error::Platform(format!("cannot parse activity: {e}")))
}

/// Render the status as human-readable text.
fn render_text(status: &PipelineStatus, range: StatusRange, activity_failed: bool) -> String {
    let mut out = String::new();

    // Header.
    out.push_str(&format!(
        "{} ({})\n",
        status.name.bold(),
        status.id.dimmed()
    ));
    out.push_str(&format!("  state:  {}\n", status.state));
    if let Some(error) = &status.error {
        out.push_str(&format!("  error:  {}\n", error.red()));
    }

    // Diagnostics summary.
    let c = &status.diagnostics;
    out.push_str(&format!(
        "\ndiagnostics (last {}): {} errors, {} warnings, {} notes\n",
        range.as_tql(),
        c.error,
        c.warning,
        c.note,
    ));
    if status.recent_diagnostics.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for d in &status.recent_diagnostics {
            let ts = d.timestamp.as_deref().unwrap_or("");
            out.push_str(&format!(
                "  {:<24} {:<8} {}\n",
                ts,
                d.severity.label(),
                d.text(),
            ));
        }
    }

    // Activity.
    out.push_str(&format!("\nactivity (last {}):\n", range.as_tql()));
    if activity_failed {
        out.push_str("  (unavailable; node may not support pipeline::activity)\n");
    } else {
        out.push_str(&format!(
            "  ingress: {}\n",
            render_flow(status.ingress.as_ref())
        ));
        out.push_str(&format!(
            "  egress:  {}\n",
            render_flow(status.egress.as_ref())
        ));
    }

    out.trim_end().to_string()
}

/// Render one flow (ingress/egress) summary line.
fn render_flow(summary: Option<&FlowSummary>) -> String {
    match summary {
        None => "no data".to_string(),
        Some(s) => {
            let spark = sparkline(&s.rates);
            let base = format!(
                "{} total, avg {}, peak {}",
                format_bytes(s.total_bytes),
                format_rate(s.avg_rate),
                format_rate(s.peak_rate),
            );
            if spark.is_empty() {
                base
            } else {
                format!("{base}  {spark}")
            }
        }
    }
}
