//! Parser for the optional leading frontmatter block in `.tql` files.
//!
//! A frontmatter block is a leading region delimited by `// ---` lines, with
//! every line prefixed by `// `. The inner text is YAML; everything after the
//! closing delimiter is the pipeline definition, sent verbatim.
//!
//! ```tql
//! // ---
//! // name: suricata-import
//! // state: running
//! // ---
//! from_file "/tmp/eve.sock"
//! ```

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::DesiredState;

/// Parsed frontmatter metadata. All fields are optional.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frontmatter {
    /// Override the pipeline name (otherwise the file stem is used).
    #[serde(default)]
    pub name: Option<String>,
    /// Free-text description. Parsed for authoring convenience but **not**
    /// part of observed state, so it never participates in drift detection.
    #[serde(default)]
    pub description: Option<String>,
    /// Desired run-state (`running` | `paused` | `stopped`).
    #[serde(default)]
    pub state: Option<DesiredState>,
    /// Optional explicit target node id.
    #[serde(default)]
    pub node: Option<String>,
}

/// The result of splitting a file into optional frontmatter + definition.
#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    /// The parsed metadata, or default when no block is present.
    pub frontmatter: Frontmatter,
    /// The pipeline definition (everything after the block), trimmed.
    pub definition: String,
}

/// The frontmatter delimiter line.
const DELIM: &str = "// ---";

/// Parse a `.tql` file's contents into frontmatter + definition.
///
/// Files without a leading `// ---` block parse with default frontmatter and
/// the whole (trimmed) content as the definition.
pub fn parse(content: &str) -> Result<Parsed> {
    let mut lines = content.lines();

    // Skip leading blank lines to find the opening delimiter.
    let mut leading_blanks = 0;
    let first = loop {
        match lines.clone().next() {
            Some(l) if l.trim().is_empty() => {
                lines.next();
                leading_blanks += 1;
            }
            other => break other,
        }
    };

    if first.map(str::trim) != Some(DELIM) {
        // No frontmatter block; the entire file is the definition.
        return Ok(Parsed {
            frontmatter: Frontmatter::default(),
            definition: content.trim().to_string(),
        });
    }

    // Consume the opening delimiter.
    lines.next();
    let mut yaml_lines = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == DELIM {
            closed = true;
            break;
        }
        yaml_lines.push(strip_comment_prefix(line));
    }
    if !closed {
        return Err(Error::Config(format!(
            "frontmatter block opened at line {} is never closed with `{DELIM}`",
            leading_blanks + 1
        )));
    }

    let yaml = yaml_lines.join("\n");
    let frontmatter: Frontmatter = serde_yaml::from_str(&yaml)
        .map_err(|e| Error::Config(format!("invalid frontmatter: {e}")))?;

    let definition = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Ok(Parsed {
        frontmatter,
        definition,
    })
}

/// Render frontmatter + definition back into `.tql` file contents.
///
/// Emits a `// ---` block containing only the fields that are set, followed by
/// the definition and a trailing newline. When no fields are set, the block is
/// omitted and only the definition is written. This is the inverse of
/// [`parse`]: `parse(render(fm, def))` round-trips the metadata and definition.
pub fn render(frontmatter: &Frontmatter, definition: &str) -> String {
    let mut fields: Vec<(&str, String)> = Vec::new();
    if let Some(name) = &frontmatter.name {
        fields.push(("name", name.clone()));
    }
    if let Some(description) = &frontmatter.description {
        fields.push(("description", description.clone()));
    }
    if let Some(state) = frontmatter.state {
        let wire = match state {
            DesiredState::Running => "running",
            DesiredState::Paused => "paused",
            DesiredState::Stopped => "stopped",
        };
        fields.push(("state", wire.to_string()));
    }
    if let Some(node) = &frontmatter.node {
        fields.push(("node", node.clone()));
    }

    let mut out = String::new();
    if !fields.is_empty() {
        out.push_str(DELIM);
        out.push('\n');
        for (key, value) in fields {
            out.push_str(&format!("// {key}: {value}\n"));
        }
        out.push_str(DELIM);
        out.push('\n');
    }
    out.push_str(definition.trim());
    out.push('\n');
    out
}

/// Strip a leading `// ` (or `//`) comment prefix from a frontmatter line.
fn strip_comment_prefix(line: &str) -> String {
    let trimmed = line.trim_start();
    let body = trimmed
        .strip_prefix("// ")
        .or_else(|| trimmed.strip_prefix("//"))
        .unwrap_or(trimmed);
    body.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_is_all_definition() {
        let p = parse("from_file \"/tmp/x\"\n").unwrap();
        assert_eq!(p.frontmatter, Frontmatter::default());
        assert_eq!(p.definition, "from_file \"/tmp/x\"");
    }

    #[test]
    fn parses_full_block() {
        let src = "// ---\n// name: suricata-import\n// description: EVE JSON\n// state: paused\n// node: n-abcd1234\n// ---\nfrom_file \"/tmp/eve.sock\"\n";
        let p = parse(src).unwrap();
        assert_eq!(p.frontmatter.name.as_deref(), Some("suricata-import"));
        assert_eq!(p.frontmatter.description.as_deref(), Some("EVE JSON"));
        assert_eq!(p.frontmatter.state, Some(DesiredState::Paused));
        assert_eq!(p.frontmatter.node.as_deref(), Some("n-abcd1234"));
        assert_eq!(p.definition, "from_file \"/tmp/eve.sock\"");
    }

    #[test]
    fn tolerates_leading_blank_lines() {
        let src = "\n\n// ---\n// name: p\n// ---\nversion\n";
        let p = parse(src).unwrap();
        assert_eq!(p.frontmatter.name.as_deref(), Some("p"));
        assert_eq!(p.definition, "version");
    }

    #[test]
    fn unclosed_block_errors() {
        let src = "// ---\n// name: p\nversion\n";
        assert!(parse(src).is_err());
    }

    #[test]
    fn invalid_state_errors() {
        let src = "// ---\n// state: galloping\n// ---\nversion\n";
        let err = parse(src).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
    }

    #[test]
    fn unknown_field_errors() {
        let src = "// ---\n// frobnicate: yes\n// ---\nversion\n";
        assert!(parse(src).is_err());
    }

    #[test]
    fn render_round_trips_full_block() {
        let fm = Frontmatter {
            name: Some("my-pipeline".to_string()),
            description: Some("an example".to_string()),
            state: Some(DesiredState::Paused),
            node: Some("n-abcd1234".to_string()),
        };
        let rendered = render(&fm, "from_file \"/tmp/x\"");
        let parsed = parse(&rendered).unwrap();
        assert_eq!(parsed.frontmatter, fm);
        assert_eq!(parsed.definition, "from_file \"/tmp/x\"");
    }

    #[test]
    fn render_omits_empty_block() {
        let rendered = render(&Frontmatter::default(), "version");
        assert_eq!(rendered, "version\n");
        let parsed = parse(&rendered).unwrap();
        assert_eq!(parsed.frontmatter, Frontmatter::default());
        assert_eq!(parsed.definition, "version");
    }

    #[test]
    fn render_round_trips_name_and_state_only() {
        let fm = Frontmatter {
            name: Some("p".to_string()),
            description: None,
            state: Some(DesiredState::Running),
            node: None,
        };
        let parsed = parse(&render(&fm, "version")).unwrap();
        assert_eq!(parsed.frontmatter, fm);
    }
}
