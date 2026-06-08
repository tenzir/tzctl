//! Typed error and result types used across the crate.
//!
//! Library-style code returns [`Error`]; the CLI edge converts these into
//! `anyhow` reports and renders any attached [hints](ErrorExt::with_hint).

/// The crate-wide error type.
// Some variants are constructed only by later stages (auth, client); they are
// part of the established contract for this stage.
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A configuration value was missing or invalid.
    #[error("configuration error: {0}")]
    Config(String),
    /// Authentication or token exchange failed.
    #[error("authentication error: {0}")]
    Auth(String),
    /// The platform returned an error response.
    #[error("platform error: {0}")]
    Platform(String),
    /// The target node is not connected to the platform.
    #[error("the node is disconnected")]
    NodeDisconnected,
    /// Any other error, typically wrapped from `anyhow`.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The crate-wide result type.
pub type Result<T> = std::result::Result<T, Error>;

/// An error together with optional actionable hints.
///
/// Mirrors the Python CLI's `add_hint` pattern: hints are rendered after the
/// error message at the CLI edge to guide the user toward a fix.
#[derive(Debug)]
pub struct HintedError {
    /// The underlying error.
    pub error: Error,
    /// Hints rendered, in order, after the error message.
    pub hints: Vec<String>,
}

impl HintedError {
    /// Wrap an [`Error`] with no hints.
    pub fn new(error: Error) -> Self {
        Self {
            error,
            hints: Vec::new(),
        }
    }

    /// Attach a hint, returning `self` for chaining.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hints.push(hint.into());
        self
    }
}

impl std::fmt::Display for HintedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)?;
        for hint in &self.hints {
            write!(f, "\nhint: {hint}")?;
        }
        Ok(())
    }
}

impl std::error::Error for HintedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl From<Error> for HintedError {
    fn from(error: Error) -> Self {
        Self::new(error)
    }
}
