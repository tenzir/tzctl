//! Types and helpers for `tz pipeline insights`: per-operator metrics
//! composed from a bounded, hidden TQL query on a node.
//!
//! The single source is `metrics "operator_profile", live=true`, emitted once
//! per `metrics_interval` (1 s) tick per operator by the execution engine. We
//! stream live emissions over a short [`SAMPLE_WINDOW`] rather than querying a
//! historical window, since operator-profile metrics are only surfaced live,
//! then keep the most recent sample per operator. The bound is wall-clock time
//! (not a row count) so that pipelines with many operators aren't truncated
//! mid-tick:
//!
//! - `cpu` is the percentage of one core busy during the tick (100 = one
//!   full core).
//! - `events_in`, `bytes_in`, and `batches_in` are per-tick deltas over one
//!   second, so the latest tick's values are already per-second rates. We
//!   report the input side (matching the platform frontend), so throughput
//!   describes what flows across each operator's input edge.
//! - `input_bytes` is the current backlog in the operator's input channel
//!   (bytes pushed upstream but not yet pulled). Channels are capped at
//!   100 MiB, so the backlog relative to that cap is the queue fullness.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// How long to stream live `operator_profile` emissions before keeping each
/// operator's most recent sample. A few seconds ensures every operator emits
/// at least one tick (the interval is 1 s) so none are missing.
pub const SAMPLE_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

/// The execution engine's per-channel capacity in bytes (100 MiB).
///
/// Mirrors `bytes_limit`/`events_limit` in the node's executor; queue
/// fullness is the input-channel backlog relative to this cap.
pub const CHANNEL_CAPACITY_BYTES: f64 = 100.0 * 1024.0 * 1024.0;

/// One raw per-tick `operator_profile` row streamed from the node.
///
/// Each tick (1 s) emits one such row per operator. `cpu` is the percentage of
/// one core busy during the tick; the `*_in`/`*_out` fields are per-tick
/// deltas; `input_bytes` is the current input-channel backlog. Deserialization
/// is tolerant: all fields except `operator_id` are optional.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OperatorSampleRaw {
    /// The operator id within the pipeline.
    pub operator_id: String,
    /// The operator name (e.g. `where`).
    #[serde(default)]
    pub name: Option<String>,
    /// Percentage of one core busy during this tick (100 = one full core).
    #[serde(default)]
    pub cpu: Option<f64>,
    /// Input events during this tick.
    #[serde(default)]
    pub events_in: Option<f64>,
    /// Output events during this tick.
    #[serde(default)]
    pub events_out: Option<f64>,
    /// Input bytes during this tick.
    #[serde(default)]
    pub bytes_in: Option<f64>,
    /// Output bytes during this tick.
    #[serde(default)]
    pub bytes_out: Option<f64>,
    /// Input batches during this tick.
    #[serde(default)]
    pub batches_in: Option<f64>,
    /// Output batches during this tick.
    #[serde(default)]
    pub batches_out: Option<f64>,
    /// Current input-channel backlog in bytes.
    #[serde(default)]
    pub input_bytes: Option<f64>,
    /// The input channel's capacity in bytes, when reported by the node.
    #[serde(default)]
    pub input_capacity: Option<f64>,
}

/// Reduce raw per-tick samples to the latest sample per operator.
///
/// Each row already holds one tick's (1 s) worth of deltas and the current CPU
/// and backlog, so the most recent sample per operator is its current
/// per-second snapshot — no averaging or summing needed. Operators keep their
/// first-seen order; the resulting rows have a one-second active duration.
pub fn latest_samples(samples: &[OperatorSampleRaw]) -> Vec<OperatorProfileRaw> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, OperatorProfileRaw> = HashMap::new();
    for s in samples {
        if !by_id.contains_key(&s.operator_id) {
            order.push(s.operator_id.clone());
        }
        let backlog = s.input_bytes;
        by_id.insert(
            s.operator_id.clone(),
            OperatorProfileRaw {
                operator_id: s.operator_id.clone(),
                name: s.name.clone(),
                cpu: s.cpu,
                events_in: s.events_in,
                events_out: s.events_out,
                bytes_in: s.bytes_in,
                bytes_out: s.bytes_out,
                batches_in: s.batches_in,
                batches_out: s.batches_out,
                queued_bytes: backlog,
                peak_queued_bytes: backlog,
                capacity_bytes: s.input_capacity,
                // Each sample spans one metrics tick (1 s).
                seconds: Some(1.0),
            },
        );
    }
    order
        .into_iter()
        .map(|id| by_id.remove(&id).expect("present"))
        .collect()
}

/// One aggregated per-operator profile, folded from raw per-tick samples.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct OperatorProfileRaw {
    /// The operator id within the pipeline (e.g. `"1"`, or `"1-2"` for an
    /// operator nested in a sub-pipeline).
    pub operator_id: String,
    /// The operator name (e.g. `where`).
    #[serde(default)]
    pub name: Option<String>,
    /// Mean percentage of one core busy per tick (100 = one full core).
    #[serde(default)]
    pub cpu: Option<f64>,
    /// Total input events over the range.
    #[serde(default)]
    pub events_in: Option<f64>,
    /// Total output events over the range.
    #[serde(default)]
    pub events_out: Option<f64>,
    /// Total input bytes over the range.
    #[serde(default)]
    pub bytes_in: Option<f64>,
    /// Total output bytes over the range.
    #[serde(default)]
    pub bytes_out: Option<f64>,
    /// Total input batches over the range.
    #[serde(default)]
    pub batches_in: Option<f64>,
    /// Total output batches over the range.
    #[serde(default)]
    pub batches_out: Option<f64>,
    /// Most recently observed input-channel backlog in bytes.
    #[serde(default)]
    pub queued_bytes: Option<f64>,
    /// Peak input-channel backlog in bytes over the range.
    #[serde(default)]
    pub peak_queued_bytes: Option<f64>,
    /// The input channel's capacity in bytes, when reported by the node.
    #[serde(default)]
    pub capacity_bytes: Option<f64>,
    /// The number of 1-second metric ticks observed in the range.
    #[serde(default)]
    pub seconds: Option<f64>,
}

/// Queue backlog and fullness of an operator's input channel.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueueFullness {
    /// Currently queued bytes.
    pub queued_bytes: f64,
    /// Peak queued bytes over the range.
    pub peak_queued_bytes: f64,
    /// The channel capacity in bytes used to compute fullness.
    pub capacity_bytes: f64,
    /// Current fullness as a percentage of the channel capacity (0–100).
    pub fullness_percent: f64,
}

/// The per-second metrics of a single operator.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OperatorInsights {
    /// The operator id within the pipeline.
    pub operator_id: String,
    /// The operator name, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Mean percentage of one core busy while the pipeline ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    /// Input events per second (across the operator's input edge).
    pub events_per_sec: f64,
    /// Input bytes per second (across the operator's input edge).
    pub bytes_per_sec: f64,
    /// Input batches per second (across the operator's input edge).
    pub batches_per_sec: f64,
    /// Queue backlog and fullness of the operator's input channel.
    pub queue: QueueFullness,
}

impl OperatorInsights {
    /// Derive an operator's insights from its raw profile row.
    ///
    /// Throughput uses the operator's *input* side (`events_in`, `bytes_in`,
    /// `batches_in`), matching the platform frontend: every column then
    /// describes what flows across the operator's input edge, consistent with
    /// the input-channel backlog reported by the queue. Reporting the output
    /// side here instead would disagree with the frontend for operators whose
    /// input and output rates differ (e.g. `throttle`, `head`, `where`).
    pub fn from_raw(raw: &OperatorProfileRaw) -> Self {
        let v = |x: Option<f64>| x.unwrap_or(0.0);
        let (events, bytes, batches) = (raw.events_in, raw.bytes_in, raw.batches_in);
        let seconds = v(raw.seconds);
        let rate = |total: Option<f64>| {
            if seconds > 0.0 {
                v(total) / seconds
            } else {
                0.0
            }
        };
        let queued_bytes = v(raw.queued_bytes);
        // Prefer the node-reported channel capacity; fall back to the default
        // when it is absent or non-positive.
        let capacity_bytes = match raw.capacity_bytes {
            Some(c) if c > 0.0 => c,
            _ => CHANNEL_CAPACITY_BYTES,
        };
        Self {
            operator_id: raw.operator_id.clone(),
            name: raw.name.clone(),
            cpu_percent: raw.cpu,
            events_per_sec: rate(events),
            bytes_per_sec: rate(bytes),
            batches_per_sec: rate(batches),
            queue: QueueFullness {
                queued_bytes,
                peak_queued_bytes: v(raw.peak_queued_bytes),
                capacity_bytes,
                fullness_percent: queued_bytes / capacity_bytes * 100.0,
            },
        }
    }
}

/// The aggregate, serializable result of `tz pipeline insights`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PipelineInsights {
    /// The pipeline name.
    pub name: String,
    /// The pipeline id.
    pub id: String,
    /// The current lifecycle state.
    pub state: String,
    /// Per-operator metrics, ordered by operator id.
    pub operators: Vec<OperatorInsights>,
}

/// Build the live per-operator profile query for a pipeline id.
///
/// Streams raw `operator_profile` rows (one per operator per 1 s tick). We do
/// not `summarize` here: aggregation on a live, unbounded stream never flushes.
/// Instead the caller bounds the stream by time and folds the rows with
/// [`aggregate_samples`].
pub fn operator_profile_query(pipeline_id: &str) -> String {
    format!(
        "metrics \"operator_profile\", live=true\n\
         where pipeline_id == \"{id}\"",
        id = pipeline_id,
    )
}

/// An instance key within a scatter / gather / broadcast operator.
///
/// Numeric instances sort before named ones (e.g. `"out"` sorts last).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum InstanceKey {
    Numeric(u64),
    Named(String),
}

/// One parsed segment of an operator id.
///
/// Supports two id formats:
/// - `{pipeline}/{operator}` pairs separated by `-` (sub-pipeline nesting)
/// - `{pipeline}/{operator}#{name}/{instance}` (scatter / gather / broadcast)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperatorIdSegment {
    pub pipeline_id: u64,
    pub operator_id: u64,
    /// Present when the id carries a `#name/instance` suffix.
    /// Numeric instances sort before named ones (e.g. `"out"`).
    pub instance: Option<InstanceKey>,
}

/// Parse an operator id into its ordered segments.
///
/// Two id formats are supported:
///
/// - `517/6-0/0-0/3` — dash-separated `pipeline/operator` pairs used for
///   sub-pipeline nesting.  Each pair becomes one segment; depth is
///   `segments.len() - 1`.
/// - `168/4#broadcast/2` — a single `pipeline/operator` base with a
///   `#name/instance` suffix used by scatter / gather / broadcast operators.
///   The suffix becomes the `instance` field; depth counts as 1.
///
/// Unparseable ids yield an empty list (sorts first, depth 0).
pub fn operator_id_segments(id: &str) -> Vec<OperatorIdSegment> {
    id.split('-')
        .filter_map(|seg| {
            // Split off a `#name/instance` suffix if present.
            let (base, instance) = match seg.split_once('#') {
                Some((base, inst_part)) => {
                    let key = inst_part.split_once('/').and_then(|(_name, inst)| {
                        if let Ok(n) = inst.parse::<u64>() {
                            Some(InstanceKey::Numeric(n))
                        } else {
                            Some(InstanceKey::Named(inst.to_string()))
                        }
                    });
                    (base, key)
                }
                None => (seg, None),
            };
            let (pipeline, operator) = base.split_once('/')?;
            Some(OperatorIdSegment {
                pipeline_id: pipeline.parse().ok()?,
                operator_id: operator.parse().ok()?,
                instance,
            })
        })
        .collect()
}

/// The nesting depth of an operator id (0 for top-level operators).
///
/// Sub-pipeline nesting adds one level per extra `pipeline/operator` pair.
/// A `#name/instance` suffix on the last segment also adds one level, since
/// each instance is a child of its parent operator.
pub fn operator_depth(id: &str) -> usize {
    let segs = operator_id_segments(id);
    let base = segs.len().saturating_sub(1);
    let instance_depth = segs
        .last()
        .map(|s| usize::from(s.instance.is_some()))
        .unwrap_or(0);
    base + instance_depth
}

/// Format an event or batch count into a compact SI string (e.g. `1.2M`).
pub fn format_count(count: f64) -> String {
    const UNITS: [&str; 5] = ["", "k", "M", "G", "T"];
    if count < 1000.0 {
        return format!("{}", count.round() as u64);
    }
    let mut value = count;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1}{}", UNITS[unit])
}

/// Format a bytes-per-second rate using only the `k`/`M` SI prefixes
/// (e.g. `850`, `1.2k`, `3.4M`; values beyond mega stay in `M`).
pub fn format_bytes_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec < 1000.0 {
        format!("{}", bytes_per_sec.round() as u64)
    } else if bytes_per_sec < 1_000_000.0 {
        format!("{:.1}k", bytes_per_sec / 1_000.0)
    } else {
        format!("{:.1}M", bytes_per_sec / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> OperatorProfileRaw {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn throughput_uses_input_side() {
        // Matches the platform frontend: throughput is the input side, even
        // when it differs from the output side (as for `throttle`, `head`,
        // `where`). Output fields are ignored.
        let op = raw(r#"{
                "operator_id": "1", "name": "where", "cpu": 25.0,
                "events_in": 8000, "events_out": 6000,
                "bytes_in": 16000, "bytes_out": 12000,
                "batches_in": 80, "batches_out": 60,
                "queued_bytes": 0, "peak_queued_bytes": 0,
                "seconds": 60
            }"#);
        let insights = OperatorInsights::from_raw(&op);
        assert_eq!(insights.name.as_deref(), Some("where"));
        assert_eq!(insights.cpu_percent, Some(25.0));
        // Input side: 8000/60, 16000/60, 80/60 — not the output values.
        assert_eq!(insights.events_per_sec, 8000.0 / 60.0);
        assert_eq!(insights.bytes_per_sec, 16000.0 / 60.0);
        assert_eq!(insights.batches_per_sec, 80.0 / 60.0);
    }

    #[test]
    fn sink_uses_input_side() {
        let op = raw(r#"{
                "operator_id": "2", "name": "import",
                "events_in": 500, "bytes_in": 1000, "batches_in": 5,
                "events_out": 0, "bytes_out": 0, "batches_out": 0,
                "seconds": 10
            }"#);
        let insights = OperatorInsights::from_raw(&op);
        assert_eq!(insights.events_per_sec, 50.0);
        assert_eq!(insights.bytes_per_sec, 100.0);
        assert_eq!(insights.batches_per_sec, 0.5);
    }

    #[test]
    fn zero_seconds_yields_zero_rates() {
        let op = raw(r#"{"operator_id": "0", "bytes_in": 1000}"#);
        let insights = OperatorInsights::from_raw(&op);
        assert_eq!(insights.cpu_percent, None);
        assert_eq!(insights.bytes_per_sec, 0.0);
        assert_eq!(insights.queue.fullness_percent, 0.0);
    }

    #[test]
    fn queue_fullness_relative_to_channel_capacity() {
        let op = raw(r#"{
                "operator_id": "1", "seconds": 1,
                "queued_bytes": 52428800, "peak_queued_bytes": 104857600
            }"#);
        let insights = OperatorInsights::from_raw(&op);
        assert_eq!(insights.queue.fullness_percent, 50.0);
        assert_eq!(insights.queue.peak_queued_bytes, 104857600.0);
        assert_eq!(insights.queue.capacity_bytes, CHANNEL_CAPACITY_BYTES);
    }

    #[test]
    fn queue_fullness_uses_reported_capacity() {
        // A reported `capacity_bytes` (from the node's `input_capacity`)
        // overrides the default channel capacity.
        let op = OperatorProfileRaw {
            operator_id: "1".to_string(),
            queued_bytes: Some(25_000_000.0),
            peak_queued_bytes: Some(25_000_000.0),
            capacity_bytes: Some(50_000_000.0),
            seconds: Some(1.0),
            ..Default::default()
        };
        let insights = OperatorInsights::from_raw(&op);
        assert_eq!(insights.queue.capacity_bytes, 50_000_000.0);
        assert_eq!(insights.queue.fullness_percent, 50.0);
    }

    #[test]
    fn input_capacity_propagates_from_sample() {
        let samples: Vec<OperatorSampleRaw> = serde_json::from_str(
            r#"[{"operator_id": "1", "input_bytes": 10, "input_capacity": 40}]"#,
        )
        .unwrap();
        let folded = latest_samples(&samples);
        assert_eq!(folded[0].capacity_bytes, Some(40.0));
        let insights = OperatorInsights::from_raw(&folded[0]);
        assert_eq!(insights.queue.capacity_bytes, 40.0);
        assert_eq!(insights.queue.fullness_percent, 25.0);
    }

    #[test]
    fn query_is_live_raw_stream() {
        let q = operator_profile_query("pid-1");
        assert!(q.contains("metrics \"operator_profile\", live=true"));
        assert!(q.contains("pipeline_id == \"pid-1\""));
        // Raw stream, bounded by time on the client: no row cap, no summarize
        // (which would never flush on a live stream), no retro window.
        assert!(!q.contains("head"));
        assert!(!q.contains("summarize"));
        assert!(!q.contains("now()"));
        assert!(!q.contains("remote"));
    }

    #[test]
    fn latest_keeps_most_recent_sample_per_operator() {
        let samples: Vec<OperatorSampleRaw> = serde_json::from_str(
            r#"[
                {"operator_id": "1", "name": "where", "cpu": 20.0,
                 "events_in": 100, "bytes_in": 200, "batches_in": 1,
                 "input_bytes": 500},
                {"operator_id": "0", "name": "src", "cpu": 10.0,
                 "events_in": 50, "bytes_in": 100, "batches_in": 1},
                {"operator_id": "1", "name": "where", "cpu": 40.0,
                 "events_in": 300, "bytes_in": 600, "batches_in": 3,
                 "input_bytes": 900}
            ]"#,
        )
        .unwrap();
        let latest = latest_samples(&samples);
        // First-seen order preserved: operator "1" before "0".
        assert_eq!(latest[0].operator_id, "1");
        assert_eq!(latest[1].operator_id, "0");
        // Operator "1": only the last tick's values are kept, over 1 s.
        assert_eq!(latest[0].seconds, Some(1.0));
        assert_eq!(latest[0].cpu, Some(40.0));
        assert_eq!(latest[0].events_in, Some(300.0));
        assert_eq!(latest[0].queued_bytes, Some(900.0));
        // The last tick's delta is already the per-second rate.
        let insights = OperatorInsights::from_raw(&latest[0]);
        assert_eq!(insights.cpu_percent, Some(40.0));
        assert_eq!(insights.events_per_sec, 300.0);
    }

    #[test]
    fn id_segments_parse_and_order() {
        // Plain `pipeline/operator` format.
        assert_eq!(
            operator_id_segments("517/4"),
            vec![OperatorIdSegment { pipeline_id: 517, operator_id: 4, instance: None }]
        );
        // Dash-separated sub-pipeline nesting.
        assert_eq!(
            operator_id_segments("517/6-0/0-0/3"),
            vec![
                OperatorIdSegment { pipeline_id: 517, operator_id: 6, instance: None },
                OperatorIdSegment { pipeline_id: 0, operator_id: 0, instance: None },
                OperatorIdSegment { pipeline_id: 0, operator_id: 3, instance: None },
            ]
        );
        // Scatter / gather / broadcast instance format.
        assert_eq!(
            operator_id_segments("168/4#broadcast/2"),
            vec![OperatorIdSegment {
                pipeline_id: 168,
                operator_id: 4,
                instance: Some(InstanceKey::Numeric(2)),
            }]
        );
        assert_eq!(
            operator_id_segments("168/25#gather/out"),
            vec![OperatorIdSegment {
                pipeline_id: 168,
                operator_id: 25,
                instance: Some(InstanceKey::Named("out".to_string())),
            }]
        );
        // Depth is the number of nested sub-pipelines.
        assert_eq!(operator_depth("517/4"), 0);
        assert_eq!(operator_depth("517/6-0/0-0/3"), 2);
        // A #name/instance suffix adds one depth level.
        assert_eq!(operator_depth("168/4#broadcast/2"), 1);
        assert_eq!(operator_depth("168/25#gather/out"), 1);
        // Unparseable ids degrade gracefully.
        assert_eq!(operator_id_segments("bogus"), vec![]);
        assert_eq!(operator_depth("bogus"), 0);
        // Lexicographic segment order matches numeric, not string, sorting:
        // operator 10 comes after operator 2.
        let mut ids = ["517/10", "517/2"];
        ids.sort_by_key(|id| operator_id_segments(id));
        assert_eq!(ids, ["517/2", "517/10"]);
        // Numeric instances sort before named ones; instances of the same
        // operator sort together and in numeric order.
        let mut ids = [
            "168/25#gather/out",
            "168/25#gather/31",
            "168/4#broadcast/2",
            "168/25#gather/2",
            "168/20#scatter/1",
        ];
        ids.sort_by_key(|id| operator_id_segments(id));
        assert_eq!(
            ids,
            [
                "168/4#broadcast/2",
                "168/20#scatter/1",
                "168/25#gather/2",
                "168/25#gather/31",
                "168/25#gather/out",
            ]
        );
    }

    #[test]
    fn format_bytes_rate_caps_at_mega() {
        assert_eq!(format_bytes_rate(0.0), "0");
        assert_eq!(format_bytes_rate(999.0), "999");
        assert_eq!(format_bytes_rate(1500.0), "1.5k");
        assert_eq!(format_bytes_rate(2_500_000.0), "2.5M");
        // No G/T prefixes: large rates stay in M.
        assert_eq!(format_bytes_rate(4_200_000_000.0), "4200.0M");
    }

    #[test]
    fn format_count_scales() {
        assert_eq!(format_count(0.0), "0");
        assert_eq!(format_count(999.0), "999");
        assert_eq!(format_count(1200.0), "1.2k");
        assert_eq!(format_count(3_400_000.0), "3.4M");
    }
}
