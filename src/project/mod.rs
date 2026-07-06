//! Project loading: glob discovery and frontmatter parsing of `.tql` files.
//!
//! A project is a directory containing `tenzir.toml`; its `.tql` files (matched
//! by `[pipelines].glob`) are the desired state. Each file becomes a
//! [`DesiredPipeline`]; the pipeline name (its identity key) comes from
//! frontmatter or the file stem and must be unique across the project.

pub mod frontmatter;

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{DesiredPipeline, DesiredState, NodeId};

/// Map a `[defaults].state` string to a [`DesiredState`].
fn parse_default_state(s: &str) -> Result<DesiredState> {
    match s.to_ascii_lowercase().as_str() {
        "running" => Ok(DesiredState::Running),
        "paused" => Ok(DesiredState::Paused),
        "stopped" => Ok(DesiredState::Stopped),
        other => Err(Error::Config(format!(
            "invalid [defaults] state {other:?}: expected running|paused|stopped"
        ))),
    }
}

/// Build a [`DesiredPipeline`] from a single `.tql` file.
///
/// The name comes from frontmatter, else the file stem. The state comes from
/// frontmatter, else `default_state`, else `running`.
pub fn desired_from_file(path: &Path) -> Result<DesiredPipeline> {
    desired_from_file_with_default(path, None)
}

/// Like [`desired_from_file`] but with an explicit project default state.
pub fn desired_from_file_with_default(
    path: &Path,
    default_state: Option<DesiredState>,
) -> Result<DesiredPipeline> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("cannot read {}: {e}", path.display())))?;
    let parsed = frontmatter::parse(&content)
        .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;

    let name = match parsed.frontmatter.name {
        Some(n) => n,
        None => path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                Error::Config(format!(
                    "cannot derive a pipeline name from {}",
                    path.display()
                ))
            })?,
    };

    if parsed.definition.is_empty() {
        return Err(Error::Config(format!(
            "{} contains no pipeline definition",
            path.display()
        )));
    }

    let state = parsed
        .frontmatter
        .state
        .or(default_state)
        .unwrap_or_default();
    let node = parsed.frontmatter.node.map(NodeId::from);

    Ok(DesiredPipeline {
        name,
        definition: parsed.definition,
        state,
        node,
    })
}

/// Load every project pipeline matched by `glob`, relative to `root`.
///
/// Enforces unique pipeline names before any network access; a duplicate name
/// is a hard error naming both files.
pub fn load_project(
    root: &Path,
    glob: &str,
    default_state: Option<&str>,
) -> Result<Vec<DesiredPipeline>> {
    Ok(load_project_with_paths(root, glob, default_state)?
        .into_iter()
        .map(|(_, p)| p)
        .collect())
}

/// Like [`load_project`] but also returns each pipeline's source file path.
///
/// Used by `tz project pull` to decide which local file to overwrite or delete
/// for a given pipeline name.
pub fn load_project_with_paths(
    root: &Path,
    glob: &str,
    default_state: Option<&str>,
) -> Result<Vec<(PathBuf, DesiredPipeline)>> {
    let default = default_state.map(parse_default_state).transpose()?;

    let walker = globwalk::GlobWalkerBuilder::from_patterns(root, &[glob])
        .build()
        .map_err(|e| Error::Config(format!("invalid pipelines glob {glob:?}: {e}")))?;

    // Collect and sort paths for deterministic ordering.
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in walker {
        let entry = entry.map_err(|e| Error::Config(format!("error walking {glob:?}: {e}")))?;
        if entry.file_type().is_file() {
            paths.push(entry.into_path());
        }
    }
    paths.sort();

    let mut pipelines = Vec::with_capacity(paths.len());
    // name -> source path, for duplicate detection.
    let mut seen: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    for path in paths {
        let pipeline = desired_from_file_with_default(&path, default)?;
        if let Some(prev) = seen.get(&pipeline.name) {
            return Err(Error::Config(format!(
                "duplicate pipeline name {:?} in {} and {}",
                pipeline.name,
                prev.display(),
                path.display()
            )));
        }
        seen.insert(pipeline.name.clone(), path.clone());
        pipelines.push((path, pipeline));
    }
    Ok(pipelines)
}

/// The static directory prefix of a pipelines glob, relative to the root.
///
/// This is where `tz project pull` writes newly discovered pipelines. It walks
/// the glob's leading components until the first one containing a wildcard
/// (`*`, `?`, `[`, or `{`), e.g. `pipelines/**/*.tql` -> `pipelines`,
/// `*.tql` -> `` (the root itself).
pub fn glob_base_dir(root: &Path, glob: &str) -> PathBuf {
    let mut base = root.to_path_buf();
    for component in Path::new(glob).components() {
        let part = component.as_os_str().to_string_lossy();
        if part.contains(['*', '?', '[', '{']) {
            break;
        }
        base.push(component);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_minimal_pipeline() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("suricata-import.tql");
        std::fs::write(&file, "\nfrom_file \"/tmp/eve.sock\"\n").unwrap();
        let p = desired_from_file(&file).unwrap();
        assert_eq!(p.name, "suricata-import");
        assert_eq!(p.definition, "from_file \"/tmp/eve.sock\"");
        assert_eq!(p.state, DesiredState::Running);
    }

    #[test]
    fn frontmatter_overrides_name_and_state() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file.tql");
        std::fs::write(
            &file,
            "// ---\n// name: renamed\n// state: paused\n// ---\nversion\n",
        )
        .unwrap();
        let p = desired_from_file(&file).unwrap();
        assert_eq!(p.name, "renamed");
        assert_eq!(p.state, DesiredState::Paused);
    }

    #[test]
    fn default_state_applies_without_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("p.tql");
        std::fs::write(&file, "version\n").unwrap();
        let p = desired_from_file_with_default(&file, Some(DesiredState::Stopped)).unwrap();
        assert_eq!(p.state, DesiredState::Stopped);
    }

    #[test]
    fn rejects_empty_definition() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("empty.tql");
        std::fs::write(&file, "   \n").unwrap();
        assert!(desired_from_file(&file).is_err());
    }

    #[test]
    fn loads_project_glob() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pipelines/sub");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(tmp.path().join("pipelines/a.tql"), "version").unwrap();
        std::fs::write(dir.join("b.tql"), "version").unwrap();
        // A non-tql file is ignored.
        std::fs::write(tmp.path().join("pipelines/readme.md"), "x").unwrap();

        let pipelines = load_project(tmp.path(), "pipelines/**/*.tql", None).unwrap();
        let names: Vec<_> = pipelines.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn duplicate_names_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("pipelines")).unwrap();
        // Two files whose frontmatter resolves to the same name.
        std::fs::write(
            tmp.path().join("pipelines/one.tql"),
            "// ---\n// name: dup\n// ---\nversion",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("pipelines/two.tql"),
            "// ---\n// name: dup\n// ---\nversion",
        )
        .unwrap();
        let err = load_project(tmp.path(), "pipelines/**/*.tql", None).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn load_project_with_paths_returns_source_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("pipelines")).unwrap();
        std::fs::write(tmp.path().join("pipelines/a.tql"), "version").unwrap();
        let loaded = load_project_with_paths(tmp.path(), "pipelines/**/*.tql", None).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].1.name, "a");
        assert!(loaded[0].0.ends_with("pipelines/a.tql"));
    }

    #[test]
    fn glob_base_dir_strips_wildcards() {
        let root = Path::new("/proj");
        assert_eq!(
            glob_base_dir(root, "pipelines/**/*.tql"),
            Path::new("/proj/pipelines")
        );
        assert_eq!(glob_base_dir(root, "*.tql"), Path::new("/proj"));
        assert_eq!(glob_base_dir(root, "a/b/c/*.tql"), Path::new("/proj/a/b/c"));
    }
}
