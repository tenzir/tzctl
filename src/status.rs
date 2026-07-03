//! Types and helpers for `tz pipeline status`: diagnostics and performance
//! insights composed from bounded, hidden TQL queries on a node.

use serde::{Deserialize, Serialize};

/// A time range for diagnostics and activity queries.
///
/// Each range maps to a `(range, interval)` pair for `pipeline::activity`,
/// mirroring the platform web app's coarse buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum StatusRange {
    /// The last hour, in 1-minute buckets.
    #[value(name = "1h")]
    Hour,
    /// The last day, in 10-minute buckets.
    #[value(name = "1d")]
    Day,
    /// The last week, in 1-hour buckets.
    #[value(name = "7d")]
    Week,
    /// The last two weeks, in 2-hour buckets.
    #[value(name = "14d")]
    Fortnight,
}

impl StatusRange {
    /// The TQL duration literal for this range (e.g. `1d`).
    pub fn as_tql(self) -> &'static str {
        match self {
            StatusRange::Hour => "1h",
            StatusRange::Day => "1d",
            StatusRange::Week => "7d",
            StatusRange::Fortnight => "14d",
        }
    }

    /// The `pipeline::activity` bucket interval literal for this range.
    pub fn interval(self) -> &'static str {
        match self {
            StatusRange::Hour => "1min",
            StatusRange::Day => "10min",
            StatusRange::Week => "1h",
            StatusRange::Fortnight => "2h",
        }
    }
}

/// The severity of a diagnostic message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// An error; the pipeline may have failed.
    Error,
    /// A warning; the pipeline continues.
    Warning,
    /// An informational note.
    Note,
    /// An unrecognized severity, preserved verbatim.
    #[serde(other)]
    Unknown,
}

impl Severity {
    /// The lowercase label for this severity.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
            Severity::Unknown => "unknown",
        }
    }
}

/// A single diagnostic emitted by a pipeline.
///
/// Deserialization is tolerant: unknown fields are ignored and most fields are
/// optional so node-version drift never breaks parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The event timestamp (ISO-8601 string, as emitted by the node).
    #[serde(default)]
    pub timestamp: Option<String>,
    /// The severity.
    pub severity: Severity,
    /// The short diagnostic message.
    #[serde(default)]
    pub message: Option<String>,
    /// The fully rendered, human-readable diagnostic, if provided.
    #[serde(default)]
    pub rendered: Option<String>,
}

impl Diagnostic {
    /// The best available human-readable text for this diagnostic.
    pub fn text(&self) -> &str {
        self.message
            .as_deref()
            .or(self.rendered.as_deref())
            .unwrap_or("")
    }
}

/// Per-severity diagnostic counts over the queried range.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DiagnosticCounts {
    /// The number of errors.
    pub error: u64,
    /// The number of warnings.
    pub warning: u64,
    /// The number of notes.
    pub note: u64,
}

impl DiagnosticCounts {
    /// Tally counts from a slice of diagnostics.
    pub fn from_diagnostics(diagnostics: &[Diagnostic]) -> Self {
        let mut counts = Self::default();
        for d in diagnostics {
            match d.severity {
                Severity::Error => counts.error += 1,
                Severity::Warning => counts.warning += 1,
                Severity::Note => counts.note += 1,
                Severity::Unknown => {}
            }
        }
        counts
    }
}

/// Ingress or egress rate data for a pipeline, from `pipeline::activity`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Rates {
    /// The total bytes over the range.
    #[serde(default)]
    pub bytes: f64,
    /// Whether this side is node-internal traffic (excluded from totals).
    #[serde(default)]
    pub internal: bool,
    /// The per-bucket byte rates.
    #[serde(default)]
    pub rates: Vec<f64>,
}

/// One pipeline's activity entry from a `pipeline::activity` response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PipelineActivity {
    /// The pipeline id.
    pub id: String,
    /// Ingress rate data.
    pub ingress: Rates,
    /// Egress rate data.
    pub egress: Rates,
}

/// The top-level `pipeline::activity` response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Activity {
    /// The first bucket timestamp.
    #[serde(default)]
    pub first: Option<String>,
    /// The last bucket timestamp.
    #[serde(default)]
    pub last: Option<String>,
    /// The per-pipeline activity entries.
    #[serde(default)]
    pub pipelines: Vec<PipelineActivity>,
}

/// A summary of one side (ingress or egress) of a pipeline's throughput.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FlowSummary {
    /// The total bytes moved over the range.
    pub total_bytes: f64,
    /// The average byte rate across buckets.
    pub avg_rate: f64,
    /// The peak byte rate across buckets.
    pub peak_rate: f64,
    /// The per-bucket rate series.
    pub rates: Vec<f64>,
}

impl FlowSummary {
    /// Summarize a `Rates` series; internal traffic yields zeroed totals.
    pub fn from_rates(rates: &Rates) -> Self {
        if rates.internal || rates.rates.is_empty() {
            return Self {
                total_bytes: if rates.internal { 0.0 } else { rates.bytes },
                avg_rate: 0.0,
                peak_rate: 0.0,
                rates: Vec::new(),
            };
        }
        let sum: f64 = rates.rates.iter().sum();
        let avg = sum / rates.rates.len() as f64;
        let peak = rates.rates.iter().copied().fold(0.0_f64, f64::max);
        Self {
            total_bytes: rates.bytes,
            avg_rate: avg,
            peak_rate: peak,
            rates: rates.rates.clone(),
        }
    }
}

/// The aggregate, serializable result of `tz pipeline status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PipelineStatus {
    /// The pipeline name.
    pub name: String,
    /// The pipeline id.
    pub id: String,
    /// The current lifecycle state.
    pub state: String,
    /// The last error, if the pipeline failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Per-severity diagnostic counts over the range.
    pub diagnostics: DiagnosticCounts,
    /// The most recent diagnostics (newest first, capped by `--limit`).
    pub recent_diagnostics: Vec<Diagnostic>,
    /// Ingress throughput summary, when activity data is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingress: Option<FlowSummary>,
    /// Egress throughput summary, when activity data is available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egress: Option<FlowSummary>,
}

/// Format a byte count into a human-readable IEC string (e.g. `1.2 GiB`).
pub fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1.0 {
        return "0 B".to_string();
    }
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", value.round() as u64, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Format a byte-per-second rate into a human-readable string (e.g. `14.5 KiB/s`).
pub fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

/// Render a rate series as a compact unicode sparkline.
pub fn sparkline(rates: &[f64]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if rates.is_empty() {
        return String::new();
    }
    let max = rates.iter().copied().fold(0.0_f64, f64::max);
    if max <= 0.0 {
        return BARS[0].to_string().repeat(rates.len());
    }
    rates
        .iter()
        .map(|&r| {
            let idx = ((r / max) * (BARS.len() - 1) as f64).round() as usize;
            BARS[idx.min(BARS.len() - 1)]
        })
        .collect()
}

/// Build the diagnostics query for a pipeline id and range.
pub fn diagnostics_query(pipeline_id: &str, range: StatusRange, limit: usize) -> String {
    format!(
        "remote {{\n  \
           diagnostics\n  \
           where pipeline_id == \"{id}\"\n  \
           where timestamp > now() - {range}\n  \
           where not hidden\n  \
           sort -timestamp\n  \
           head {limit}\n\
         }}",
        id = pipeline_id,
        range = range.as_tql(),
        limit = limit,
    )
}

/// Build the `pipeline::activity` query for a range.
pub fn activity_query(range: StatusRange) -> String {
    format!(
        "remote {{ pipeline::activity range={range}, interval={interval} }}",
        range = range.as_tql(),
        interval = range.interval(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_counts_tally() {
        let diags = vec![
            Diagnostic {
                timestamp: None,
                severity: Severity::Error,
                message: None,
                rendered: None,
            },
            Diagnostic {
                timestamp: None,
                severity: Severity::Warning,
                message: None,
                rendered: None,
            },
            Diagnostic {
                timestamp: None,
                severity: Severity::Error,
                message: None,
                rendered: None,
            },
        ];
        let counts = DiagnosticCounts::from_diagnostics(&diags);
        assert_eq!(counts.error, 2);
        assert_eq!(counts.warning, 1);
        assert_eq!(counts.note, 0);
    }

    #[test]
    fn diagnostic_parses_fixture() {
        let raw = include_str!("../tests/fixtures/diagnostics_events.json");
        let diags: Vec<Diagnostic> = serde_json::from_str(raw).unwrap();
        assert_eq!(diags.len(), 3);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].text(), "connection refused");
        let counts = DiagnosticCounts::from_diagnostics(&diags);
        assert_eq!(counts.error, 1);
        assert_eq!(counts.warning, 1);
        assert_eq!(counts.note, 1);
    }

    #[test]
    fn unknown_severity_is_tolerated() {
        let d: Diagnostic = serde_json::from_str(r#"{"severity":"fatal"}"#).unwrap();
        assert_eq!(d.severity, Severity::Unknown);
    }

    #[test]
    fn activity_parses_fixture_and_summarizes() {
        let raw = include_str!("../tests/fixtures/pipeline_activity.json");
        let activity: Activity = serde_json::from_str(raw).unwrap();
        let entry = activity
            .pipelines
            .iter()
            .find(|p| p.id == "4c7f2b11-6169-4d1b-89b4-4fc0a68b3d4a")
            .unwrap();
        let ingress = FlowSummary::from_rates(&entry.ingress);
        assert_eq!(ingress.rates.len(), 4);
        assert_eq!(ingress.peak_rate, 400.0);
        assert_eq!(ingress.avg_rate, 250.0);
        // Egress is internal → zeroed.
        let egress = FlowSummary::from_rates(&entry.egress);
        assert_eq!(egress.total_bytes, 0.0);
        assert!(egress.rates.is_empty());
    }

    #[test]
    fn format_bytes_scales() {
        assert_eq!(format_bytes(0.0), "0 B");
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1024.0), "1.0 KiB");
        assert_eq!(format_bytes(1536.0), "1.5 KiB");
        assert_eq!(format_bytes(1024.0 * 1024.0), "1.0 MiB");
    }

    #[test]
    fn sparkline_scales_to_max() {
        assert_eq!(sparkline(&[]), "");
        assert_eq!(sparkline(&[0.0, 0.0]), "▁▁");
        let line = sparkline(&[0.0, 5.0, 10.0]);
        assert_eq!(line.chars().count(), 3);
        assert!(line.ends_with('█'));
    }

    #[test]
    fn queries_embed_range_and_id() {
        let q = diagnostics_query("pid-1", StatusRange::Day, 20);
        assert!(q.contains("pipeline_id == \"pid-1\""));
        assert!(q.contains("now() - 1d"));
        assert!(q.contains("head 20"));
        let a = activity_query(StatusRange::Week);
        assert_eq!(a, "remote { pipeline::activity range=7d, interval=1h }");
    }
}
