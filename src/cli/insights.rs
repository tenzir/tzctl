//! Handler for `tz pipeline insights <name>` — per-operator CPU, throughput,
//! and queue metrics for a single pipeline.

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::HintedError;
use crate::insights::{
    OperatorInsights, OperatorSampleRaw, PipelineInsights, SAMPLE_WINDOW, format_count,
    latest_samples, operator_depth, operator_id_segments, operator_profile_query,
};
use crate::output::{self, OutputMode};
use crate::status::{format_bytes, format_rate};
use crate::table::{Align, Table};

/// Handle `tz pipeline insights <name>`.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    name: &str,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let client = super::platform_client(config, sources).await?;

    let pipeline = client
        .resolve_pipeline(&workspace, &node, name)
        .await
        .map_err(super::list::disconnected_hint)?;

    let query = operator_profile_query(pipeline.id.as_str());
    let events = client
        .sample_live_query(&workspace, &node, &query, SAMPLE_WINDOW)
        .await
        .map_err(super::list::disconnected_hint)?;
    let samples: Vec<OperatorSampleRaw> = events
        .into_iter()
        // Serve wraps each result as `{schema_id, data}`; unwrap `data` when
        // present, otherwise use the event verbatim.
        .map(|mut e| e.get_mut("data").map(serde_json::Value::take).unwrap_or(e))
        .filter_map(|e| serde_json::from_value::<OperatorSampleRaw>(e).ok())
        .collect();
    let mut operators: Vec<OperatorInsights> = latest_samples(&samples)
        .iter()
        .map(OperatorInsights::from_raw)
        .collect();
    // Order as a pipeline tree: lexicographically by parsed id segments, so
    // nested sub-pipeline operators follow their parent in execution order.
    operators.sort_by(|a, b| {
        operator_id_segments(&a.operator_id).cmp(&operator_id_segments(&b.operator_id))
    });

    let insights = PipelineInsights {
        name: pipeline.name.clone(),
        id: pipeline.id.to_string(),
        state: pipeline.state.to_string(),
        operators,
    };

    output::render(output, &insights, || render_text(&insights))
        .map_err(|e| HintedError::new(crate::error::Error::Other(e.into())))?;
    Ok(())
}

/// Render the insights as human-readable text.
fn render_text(insights: &PipelineInsights) -> String {
    let mut out = String::new();

    // Header.
    out.push_str(&format!(
        "{} ({})\n",
        insights.name.bold(),
        insights.id.dimmed()
    ));
    out.push_str(&format!("  state:  {}\n", insights.state));

    out.push_str(&format!(
        "\noperator metrics ({}s live sample):\n",
        SAMPLE_WINDOW.as_secs()
    ));
    if insights.operators.is_empty() {
        out.push_str("  (no metrics in range; is the pipeline running?)\n");
        return out.trim_end().to_string();
    }

    let mut table = Table::new(["name", "cpu", "events/s", "bytes/s", "batches/s", "queue"])
        .align(2, Align::Right)
        .align(3, Align::Right)
        .align(4, Align::Right)
        .align(5, Align::Right)
        .align(6, Align::Right);
    for op in &insights.operators {
        // Indent the name by nesting depth to convey the sub-pipeline tree.
        let indent = "  ".repeat(operator_depth(&op.operator_id));
        let name = op.name.as_deref().unwrap_or("-");
        table.row([
            format!("{indent}{name}"),
            op.cpu_percent
                .map(|p| format!("{p:.1}%"))
                .unwrap_or_else(|| "-".to_string()),
            format_count(op.events_per_sec),
            format_rate(op.bytes_per_sec),
            format!("{:.1}", op.batches_per_sec),
            render_queue(op),
        ]);
    }
    out.push_str(&table.render());

    out.trim_end().to_string()
}

/// Render an operator's queue cell as fullness plus current backlog.
fn render_queue(op: &OperatorInsights) -> String {
    let q = &op.queue;
    if q.queued_bytes == 0.0 && q.peak_queued_bytes == 0.0 {
        return "empty".to_string();
    }
    format!(
        "{:.1}% ({})",
        q.fullness_percent,
        format_bytes(q.queued_bytes)
    )
}
