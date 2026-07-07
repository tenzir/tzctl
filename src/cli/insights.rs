//! Handler for `tz pipeline insights <name>` — per-operator CPU, throughput,
//! and queue metrics for a single pipeline.

use owo_colors::OwoColorize;

use crate::auth::TokenSources;
use crate::client::PlatformClient;
use crate::config::ResolvedConfig;
use crate::error::HintedError;
use crate::insights::{
    CHANNEL_CAPACITY_BYTES, OperatorInsights, OperatorSampleRaw, PipelineInsights, SAMPLE_WINDOW,
    format_bytes_rate, format_count, latest_samples, operator_depth, operator_id_segments,
    operator_profile_query,
};
use crate::output::{self, OutputMode};
use crate::status::format_bytes;
use crate::table::{Align, Table};

/// Handle `tz pipeline insights <name>`.
pub async fn run(
    config: &ResolvedConfig,
    sources: TokenSources,
    output: OutputMode,
    name: &str,
    watch: bool,
) -> Result<(), HintedError> {
    let (workspace, node) = super::resolve_target(config)?;
    let client = super::platform_client(config, sources).await?;

    let pipeline = client
        .resolve_pipeline(&workspace, &node, name)
        .await
        .map_err(super::list::disconnected_hint)?;

    let query = operator_profile_query(pipeline.id.as_str());

    if watch {
        // Stream live emissions until Ctrl-C, re-rendering the table as each
        // page of new samples arrives.
        let mut samples: Vec<OperatorSampleRaw> = Vec::new();
        client
            .stream_query(&workspace, &node, &query, |events| {
                let new = parse_samples(events.to_vec());
                if new.is_empty() {
                    return Ok(());
                }
                // Keep only the latest sample per operator, preserving
                // first-seen order, so memory stays bounded.
                for s in new {
                    match samples.iter_mut().find(|e| e.operator_id == s.operator_id) {
                        Some(existing) => *existing = s,
                        None => samples.push(s),
                    }
                }
                let insights = build_insights(&pipeline, &samples);
                render_watch_frame(output, &insights)
                    .map_err(|e| crate::error::Error::Other(e.into()))?;
                Ok(())
            })
            .await
            .map_err(super::list::disconnected_hint)?;
        return Ok(());
    }

    let events = client
        .sample_live_query(&workspace, &node, &query, SAMPLE_WINDOW)
        .await
        .map_err(super::list::disconnected_hint)?;
    let insights = build_insights(&pipeline, &parse_samples(events));

    output::render(output, &insights, || render_text(&insights))
        .map_err(|e| HintedError::new(crate::error::Error::Other(e.into())))?;
    Ok(())
}

/// Parse raw serve events into operator-profile samples.
fn parse_samples(events: Vec<serde_json::Value>) -> Vec<OperatorSampleRaw> {
    events
        .into_iter()
        // Serve wraps each result as `{schema_id, data}`; unwrap `data` when
        // present, otherwise use the event verbatim.
        .map(|mut e| e.get_mut("data").map(serde_json::Value::take).unwrap_or(e))
        .filter_map(|e| serde_json::from_value::<OperatorSampleRaw>(e).ok())
        .collect()
}

/// Fold raw samples into the serializable per-pipeline insights.
fn build_insights(
    pipeline: &crate::model::RemotePipeline,
    samples: &[OperatorSampleRaw],
) -> PipelineInsights {
    let mut operators: Vec<OperatorInsights> = latest_samples(samples)
        .iter()
        .map(OperatorInsights::from_raw)
        .collect();
    // Order as a pipeline tree: lexicographically by parsed id segments, so
    // nested sub-pipeline operators follow their parent in execution order.
    operators.sort_by(|a, b| {
        operator_id_segments(&a.operator_id).cmp(&operator_id_segments(&b.operator_id))
    });
    PipelineInsights {
        name: pipeline.name.clone(),
        id: pipeline.id.to_string(),
        state: pipeline.state.to_string(),
        operators,
    }
}

/// Render one `--watch` update.
///
/// Text mode clears the screen and redraws the table in place (like
/// `watch(1)`); JSON mode emits one document per update so the output remains
/// machine-consumable as a stream.
fn render_watch_frame(output: OutputMode, insights: &PipelineInsights) -> std::io::Result<()> {
    if output == OutputMode::Text {
        // Clear the screen and move the cursor home before redrawing.
        print!("\x1b[2J\x1b[H");
    }
    output::render(output, insights, || {
        format!(
            "{}\n\n{}",
            render_text_with_label(insights, "live"),
            "(watching; Ctrl-C to exit)".dimmed()
        )
    })
}

/// Render the insights as human-readable text.
fn render_text(insights: &PipelineInsights) -> String {
    render_text_with_label(
        insights,
        &format!("{}s live sample", SAMPLE_WINDOW.as_secs()),
    )
}

/// Render the insights as human-readable text with a custom sample label.
fn render_text_with_label(insights: &PipelineInsights, label: &str) -> String {
    let mut out = String::new();

    // Header.
    out.push_str(&format!(
        "{} ({})\n",
        insights.name.bold(),
        insights.id.dimmed()
    ));
    out.push_str(&format!("  state:  {}\n", insights.state));

    out.push_str(&format!("\noperator metrics ({label}):\n"));
    if insights.operators.is_empty() {
        out.push_str("  (no metrics in range; is the pipeline running?)\n");
        return out.trim_end().to_string();
    }

    // The cpu header is padded to 6 chars so the column fits values like
    // `123.4%` without resizing between watch frames.
    let mut table = Table::new([
        "name",
        "cpu   ",
        "events/s",
        "bytes/s",
        "batches/s",
        "queue",
    ])
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
            render_cpu(op.cpu_percent),
            dim_if_zero(format_count(op.events_per_sec), op.events_per_sec),
            dim_if_zero(format_bytes_rate(op.bytes_per_sec), op.bytes_per_sec),
            dim_if_zero(format!("{:.1}", op.batches_per_sec), op.batches_per_sec),
            render_queue(op),
        ]);
    }
    out.push_str(&table.render());

    out.trim_end().to_string()
}

/// Dim a numeric cell when its value is zero.
///
/// Checks the *displayed* value rather than the raw one, so tiny values that
/// round to `0` or `0.0` (e.g. 0.04 batches/s) are dimmed as well.
fn dim_if_zero(cell: String, value: f64) -> String {
    if value == 0.0 || cell == "0" || cell == "0.0" {
        cell.dimmed().to_string()
    } else {
        cell
    }
}

/// Render an operator's CPU cell, colored by load.
///
/// Zero is dimmed; above 50% (half a core) is yellow; above 100% (more than
/// one full core) is red.
fn render_cpu(cpu_percent: Option<f64>) -> String {
    let Some(p) = cpu_percent else {
        return "-".to_string();
    };
    let cell = format!("{p:.1}%");
    if p > 100.0 {
        cell.red().to_string()
    } else if p > 50.0 {
        cell.yellow().to_string()
    } else if cell == "0.0%" {
        // Dim by the displayed value: tiny loads (e.g. 0.04%) render as 0.0%.
        cell.dimmed().to_string()
    } else {
        cell
    }
}

/// Render an operator's queue cell as its current backlog in bytes.
///
/// An empty queue renders as a dimmed `0`; a backlog beyond the 100 MiB
/// channel capacity is red.
fn render_queue(op: &OperatorInsights) -> String {
    let q = &op.queue;
    if q.queued_bytes == 0.0 && q.peak_queued_bytes == 0.0 {
        return "0".dimmed().to_string();
    }
    let cell = format_bytes(q.queued_bytes);
    if q.queued_bytes > CHANNEL_CAPACITY_BYTES {
        cell.red().to_string()
    } else {
        cell
    }
}
