//! Logging initialization and the text/JSON output abstraction.

use std::io::Write;

use tracing_subscriber::EnvFilter;

/// How command output should be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OutputMode {
    /// Human-readable text (the default).
    #[default]
    Text,
    /// Machine-readable JSON, for CI consumption.
    Json,
}

/// Initialize `tracing` with a verbosity-derived level.
///
/// Verbosity maps to: `0` → `warn`, `1` → `info`, `2+` → `debug`. The
/// `RUST_LOG` environment variable, if set, overrides this default.
pub fn init_tracing(verbose: u8) {
    let default_level = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("tzctl={default_level},warn")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

/// Render a value to stdout according to `mode`.
///
/// In [`OutputMode::Json`] the `value` is serialized as pretty JSON; in
/// [`OutputMode::Text`] the supplied `text` closure produces the human-readable
/// form. The closure is only evaluated in text mode.
// Consumed by command handlers from the read-path stage onward.
pub fn render<T, F>(mode: OutputMode, value: &T, text: F) -> std::io::Result<()>
where
    T: serde::Serialize,
    F: FnOnce() -> String,
{
    let mut out = std::io::stdout().lock();
    match mode {
        OutputMode::Json => {
            let json = serde_json::to_string_pretty(value)
                .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
            writeln!(out, "{json}")
        }
        OutputMode::Text => writeln!(out, "{}", text()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_mode_default_is_text() {
        assert_eq!(OutputMode::default(), OutputMode::Text);
    }
}
