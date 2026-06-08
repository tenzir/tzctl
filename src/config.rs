//! `tenzir.toml` model, discovery, and precedence resolution.
//!
//! Configuration values are resolved with the precedence:
//! CLI flags → `TENZIR_PLATFORM_CLI_*` env → `tenzir.toml` → built-in defaults.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// The default public platform API endpoint.
pub const DEFAULT_API_ENDPOINT: &str = "https://rest.tenzir.app/production-v1";

/// The default pipeline glob used when none is configured.
pub const DEFAULT_PIPELINES_GLOB: &str = "pipelines/**/*.tql";

/// The configuration file name discovered by walking up the tree.
pub const CONFIG_FILE_NAME: &str = "tenzir.toml";

/// The on-disk `tenzir.toml` model.
///
/// All sections are optional so that a partial file (or none at all) still
/// parses; missing values fall back to defaults during [`resolve`].
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Platform connection settings.
    #[serde(default)]
    pub platform: PlatformConfig,
    /// Target workspace.
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    /// Target node.
    #[serde(default)]
    pub node: NodeConfig,
    /// Pipeline discovery settings.
    #[serde(default)]
    pub pipelines: PipelinesConfig,
    /// Default pipeline settings.
    #[serde(default)]
    pub defaults: DefaultsConfig,
    /// Authentication settings.
    #[serde(default)]
    pub auth: AuthConfig,
}

/// `[platform]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformConfig {
    /// The platform API endpoint base URL.
    pub api_endpoint: Option<String>,
    /// OIDC settings (`[platform.oidc]`).
    #[serde(default)]
    pub oidc: OidcConfig,
}

/// `[platform.oidc]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfig {
    /// The OIDC issuer URL.
    pub issuer: Option<String>,
    /// The OIDC client id used for login.
    pub client_id: Option<String>,
    /// The OIDC client secret for non-interactive (client-credentials) login.
    ///
    /// Storing a secret in plaintext is discouraged; prefer
    /// `client_secret_file`.
    pub client_secret: Option<String>,
    /// A path to a file containing the OIDC client secret.
    pub client_secret_file: Option<String>,
    /// An optional audience override.
    pub audience: Option<String>,
    /// An optional scope override.
    pub scope: Option<String>,
}

/// `[workspace]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// The workspace (tenant) id, e.g. `t-abcd1234`.
    pub id: Option<String>,
}

/// `[node]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    /// The node id, e.g. `n-w2tjezz3`.
    pub id: Option<String>,
}

/// `[pipelines]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipelinesConfig {
    /// The glob used to discover `.tql` files.
    pub glob: Option<String>,
}

/// `[defaults]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    /// The default desired state for pipelines (e.g. `running`).
    pub state: Option<String>,
}

/// `[auth]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// An inline OIDC id token (discouraged; prefer `token_file`).
    pub id_token: Option<String>,
    /// A path to a file containing the OIDC id token.
    pub token_file: Option<String>,
}

/// Overrides supplied on the command line.
///
/// Each field, when `Some`, takes precedence over env and file values.
#[derive(Debug, Default, Clone)]
pub struct CliOverrides {
    /// `--api-endpoint`.
    pub api_endpoint: Option<String>,
    /// `--workspace`.
    pub workspace: Option<String>,
    /// `--node`.
    pub node: Option<String>,
}

/// A fully-resolved configuration used throughout the program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    /// The platform API endpoint base URL.
    pub api_endpoint: String,
    /// The resolved OIDC issuer URL, if configured (otherwise a built-in
    /// default applies during authentication).
    pub oidc_issuer: Option<String>,
    /// The resolved OIDC client id, if configured (otherwise a built-in
    /// default applies during authentication).
    pub client_id: Option<String>,
    /// The resolved OIDC client secret, if configured (inline or via file).
    pub client_secret: Option<String>,
    /// The resolved OIDC audience override, if configured.
    pub oidc_audience: Option<String>,
    /// The resolved OIDC scope override, if configured.
    pub oidc_scope: Option<String>,
    /// The resolved workspace id, if any.
    pub workspace: Option<String>,
    /// The resolved node id, if any.
    pub node: Option<String>,
    /// The pipeline discovery glob.
    pub pipelines_glob: String,
    /// The default desired pipeline state, if configured.
    pub default_state: Option<String>,
    /// An inline id token, if configured.
    pub id_token: Option<String>,
    /// A path to the id token file, if configured.
    pub token_file: Option<String>,
    /// The directory the config file was found in, if any.
    pub config_dir: Option<PathBuf>,
    /// The project root used to resolve the pipelines glob.
    pub project_root: PathBuf,
}

/// Discover the nearest `tenzir.toml` by walking up from `start`.
///
/// Returns the path to the first `tenzir.toml` found in `start` or any of its
/// ancestor directories, or `None` if none exists.
pub fn discover(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Read and parse a `tenzir.toml` from `path`.
pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
    let config: Config = toml::from_str(&text)
        .map_err(|e| Error::Config(format!("cannot parse {}: {e}", path.display())))?;
    Ok(config)
}

/// Read an environment variable, returning `None` if unset or empty.
fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

/// Validate a workspace id against the `t-[a-z0-9]{8}` shape.
fn validate_workspace(id: &str) -> Result<()> {
    validate_id(id, 't', "workspace")
}

/// Validate a node id against the `n-[a-z0-9]{8}` shape.
fn validate_node(id: &str) -> Result<()> {
    validate_id(id, 'n', "node")
}

/// Validate an id of the form `{prefix}-[a-z0-9]{8}`.
fn validate_id(id: &str, prefix: char, kind: &str) -> Result<()> {
    let valid = id
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|body| {
            body.len() == 8
                && body
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if valid {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "invalid {kind} id {id:?}: expected the form \"{prefix}-xxxxxxxx\" \
             (8 lowercase alphanumerics)"
        )))
    }
}

/// Resolve effective configuration from CLI overrides, env, and a config file.
///
/// Applies the precedence CLI → env → file → defaults. The `config` and
/// `config_dir` arguments are the parsed file and its containing directory (if
/// a file was found).
pub fn resolve(
    cli: &CliOverrides,
    config: &Config,
    config_dir: Option<PathBuf>,
) -> Result<ResolvedConfig> {
    let api_endpoint = cli
        .api_endpoint
        .clone()
        .or_else(|| env_var("TENZIR_PLATFORM_CLI_API_ENDPOINT"))
        .or_else(|| config.platform.api_endpoint.clone())
        .unwrap_or_else(|| DEFAULT_API_ENDPOINT.to_string());

    let oidc = &config.platform.oidc;
    let oidc_issuer = oidc.issuer.clone();
    let client_id = oidc.client_id.clone();
    let oidc_audience = oidc.audience.clone();
    let oidc_scope = oidc.scope.clone();

    // The inline client secret takes precedence over the file-based source.
    let client_secret = match &oidc.client_secret {
        Some(secret) => Some(secret.clone()),
        None => match &oidc.client_secret_file {
            Some(path) => {
                let secret = std::fs::read_to_string(path).map_err(|e| {
                    Error::Config(format!("cannot read client secret file {path:?}: {e}"))
                })?;
                Some(secret.trim().to_string())
            }
            None => None,
        },
    };

    let workspace = cli
        .workspace
        .clone()
        .or_else(|| env_var("TENZIR_PLATFORM_CLI_WORKSPACE"))
        .or_else(|| config.workspace.id.clone());
    if let Some(ws) = &workspace {
        validate_workspace(ws)?;
    }

    let node = cli
        .node
        .clone()
        .or_else(|| env_var("TENZIR_PLATFORM_CLI_NODE"))
        .or_else(|| config.node.id.clone());
    if let Some(n) = &node {
        validate_node(n)?;
    }

    let pipelines_glob = config
        .pipelines
        .glob
        .clone()
        .unwrap_or_else(|| DEFAULT_PIPELINES_GLOB.to_string());

    let default_state = config.defaults.state.clone();

    let id_token = env_var("TENZIR_PLATFORM_CLI_ID_TOKEN").or_else(|| config.auth.id_token.clone());
    let token_file = config.auth.token_file.clone();

    Ok(ResolvedConfig {
        api_endpoint,
        oidc_issuer,
        client_id,
        client_secret,
        oidc_audience,
        oidc_scope,
        workspace,
        node,
        pipelines_glob,
        default_state,
        id_token,
        token_file,
        project_root: config_dir.clone().unwrap_or_else(|| PathBuf::from(".")),
        config_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    /// Serializes tests that read or mutate process-wide environment variables,
    /// which would otherwise race under the parallel test runner.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn discover_finds_nearest_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let nested = root.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join(CONFIG_FILE_NAME), "").unwrap();

        let found = discover(&nested).unwrap();
        assert_eq!(found, root.join(CONFIG_FILE_NAME));
    }

    #[test]
    fn discover_prefers_closest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let nested = root.join("a/b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join(CONFIG_FILE_NAME), "").unwrap();
        fs::write(nested.join(CONFIG_FILE_NAME), "").unwrap();

        let found = discover(&nested).unwrap();
        assert_eq!(found, nested.join(CONFIG_FILE_NAME));
    }

    #[test]
    fn discover_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover(tmp.path()).is_none());
    }

    #[test]
    fn defaults_when_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = Config::default();
        let resolved = resolve(&CliOverrides::default(), &config, None).unwrap();
        assert_eq!(resolved.api_endpoint, DEFAULT_API_ENDPOINT);
        assert_eq!(resolved.pipelines_glob, DEFAULT_PIPELINES_GLOB);
        assert!(resolved.workspace.is_none());
        assert!(resolved.node.is_none());
        assert!(resolved.client_id.is_none());
        assert!(resolved.client_secret.is_none());
    }

    #[test]
    fn file_values_apply() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config: Config = toml::from_str(
            r#"
            [platform]
            api_endpoint = "https://example.test/api"
            [platform.oidc]
            client_id = "my-client-id"
            [workspace]
            id = "t-abcd1234"
            [node]
            id = "n-w2tjezz3"
            "#,
        )
        .unwrap();
        let resolved = resolve(&CliOverrides::default(), &config, None).unwrap();
        assert_eq!(resolved.api_endpoint, "https://example.test/api");
        assert_eq!(resolved.client_id.as_deref(), Some("my-client-id"));
        assert_eq!(resolved.workspace.as_deref(), Some("t-abcd1234"));
        assert_eq!(resolved.node.as_deref(), Some("n-w2tjezz3"));
    }

    #[test]
    fn client_secret_from_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let secret_path = tmp.path().join("secret.txt");
        fs::write(&secret_path, "  s3cr3t-value\n").unwrap();
        let config: Config = toml::from_str(&format!(
            r#"
            [platform.oidc]
            client_secret_file = {:?}
            "#,
            secret_path.display().to_string()
        ))
        .unwrap();
        let resolved = resolve(&CliOverrides::default(), &config, None).unwrap();
        // Inline value is absent; the file is read and trimmed.
        assert_eq!(resolved.client_secret.as_deref(), Some("s3cr3t-value"));
    }

    #[test]
    fn client_secret_inline_beats_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config: Config = toml::from_str(
            r#"
            [platform.oidc]
            client_secret = "inline"
            client_secret_file = "/nonexistent/path"
            "#,
        )
        .unwrap();
        let resolved = resolve(&CliOverrides::default(), &config, None).unwrap();
        assert_eq!(resolved.client_secret.as_deref(), Some("inline"));
    }

    #[test]
    fn oidc_settings_from_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config: Config = toml::from_str(
            r#"
            [platform.oidc]
            issuer = "https://issuer.test/"
            client_id = "file-client-id"
            audience = "my-audience"
            scope = "openid profile"
            "#,
        )
        .unwrap();
        let resolved = resolve(&CliOverrides::default(), &config, None).unwrap();
        assert_eq!(
            resolved.oidc_issuer.as_deref(),
            Some("https://issuer.test/")
        );
        assert_eq!(resolved.client_id.as_deref(), Some("file-client-id"));
        assert_eq!(resolved.oidc_audience.as_deref(), Some("my-audience"));
        assert_eq!(resolved.oidc_scope.as_deref(), Some("openid profile"));
    }

    #[test]
    fn flag_overrides_env_and_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: guarded by ENV_LOCK; restore after.
        unsafe {
            std::env::set_var("TENZIR_PLATFORM_CLI_WORKSPACE", "t-eeeeeeee");
        }
        let config: Config = toml::from_str(
            r#"
            [workspace]
            id = "t-ffffffff"
            "#,
        )
        .unwrap();
        let cli = CliOverrides {
            workspace: Some("t-aaaa1111".to_string()),
            ..Default::default()
        };
        let resolved = resolve(&cli, &config, None).unwrap();
        assert_eq!(resolved.workspace.as_deref(), Some("t-aaaa1111"));
        unsafe {
            std::env::remove_var("TENZIR_PLATFORM_CLI_WORKSPACE");
        }
    }

    #[test]
    fn env_overrides_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: guarded by ENV_LOCK; restore after.
        unsafe {
            std::env::set_var("TENZIR_PLATFORM_CLI_NODE", "n-aaaa1111");
        }
        let config: Config = toml::from_str(
            r#"
            [node]
            id = "n-ffffffff"
            "#,
        )
        .unwrap();
        let resolved = resolve(&CliOverrides::default(), &config, None).unwrap();
        assert_eq!(resolved.node.as_deref(), Some("n-aaaa1111"));
        unsafe {
            std::env::remove_var("TENZIR_PLATFORM_CLI_NODE");
        }
    }

    #[test]
    fn rejects_malformed_workspace() {
        let cli = CliOverrides {
            workspace: Some("nope".to_string()),
            ..Default::default()
        };
        let err = resolve(&cli, &Config::default(), None).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn rejects_malformed_node() {
        let cli = CliOverrides {
            node: Some("n-TOOLONG9".to_string()),
            ..Default::default()
        };
        let err = resolve(&cli, &Config::default(), None).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn accepts_valid_ids() {
        assert!(validate_workspace("t-abcd1234").is_ok());
        assert!(validate_node("n-w2tjezz3").is_ok());
        assert!(validate_workspace("t-abcd123").is_err());
        assert!(validate_node("x-abcd1234").is_err());
        assert!(validate_workspace("t-ABCD1234").is_err());
    }
}
