//! YAML frontmatter, shared by the skill and prompt-template loaders.
//!
//! Port of the private `parseFrontmatter` in `harness/skills.ts` and
//! `harness/prompt-templates.ts` (upstream duplicates it; the port does not).

use serde::de::DeserializeOwned;

/// A parsed markdown file: its frontmatter, deserialized, plus the body.
#[derive(Debug, Clone, PartialEq)]
pub struct Frontmatter<T> {
    pub frontmatter: T,
    pub body: String,
}

/// Split `---` frontmatter off the front of a markdown document and parse it.
///
/// Upstream's rules, kept exactly:
/// - line endings are normalized to `\n` first,
/// - a document not starting with `---` is all body, with empty frontmatter,
/// - an *unterminated* `---` block is also all body rather than an error,
/// - the body is trimmed, the frontmatter is not,
/// - an empty or `null` YAML document yields the default value.
///
/// Only malformed YAML inside a terminated block is an error; callers turn that
/// into a `parse_failed` diagnostic and skip the file.
pub fn parse_frontmatter<T: DeserializeOwned + Default>(
    content: &str,
) -> Result<Frontmatter<T>, String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");

    let Some(rest) = normalized.strip_prefix("---") else {
        return Ok(Frontmatter {
            frontmatter: T::default(),
            body: normalized,
        });
    };
    // `rest` begins at byte 3; upstream searches for the closing "\n---" from
    // there, so an empty block (`---\n---`) is still terminated.
    let Some(offset) = rest.find("\n---") else {
        return Ok(Frontmatter {
            frontmatter: T::default(),
            body: normalized,
        });
    };
    let end_index = 3 + offset;

    // Byte slicing is safe: every delimiter here is ASCII.
    let yaml = &normalized[4.min(end_index)..end_index];
    let body = normalized[end_index + 4..].trim().to_string();

    if yaml.trim().is_empty() {
        return Ok(Frontmatter {
            frontmatter: T::default(),
            body,
        });
    }

    match serde_norway::from_str::<Option<T>>(yaml) {
        Ok(parsed) => Ok(Frontmatter {
            frontmatter: parsed.unwrap_or_default(),
            body,
        }),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Deserialize)]
    #[serde(default)]
    struct Meta {
        description: String,
        name: Option<String>,
        #[serde(rename = "disable-model-invocation")]
        disable_model_invocation: bool,
    }

    #[test]
    fn parses_a_terminated_block_and_trims_the_body() {
        let parsed: Frontmatter<Meta> =
            parse_frontmatter("---\ndescription: One template\n---\nHello $1\n").unwrap();
        assert_eq!(parsed.frontmatter.description, "One template");
        assert_eq!(parsed.body, "Hello $1");
    }

    #[test]
    fn a_document_without_frontmatter_is_all_body() {
        let parsed: Frontmatter<Meta> = parse_frontmatter("First line description\nBody").unwrap();
        assert_eq!(parsed.frontmatter, Meta::default());
        assert_eq!(parsed.body, "First line description\nBody");
    }

    #[test]
    fn an_unterminated_block_is_all_body_not_an_error() {
        let parsed: Frontmatter<Meta> = parse_frontmatter("---\ndescription: x\nno end").unwrap();
        assert_eq!(parsed.frontmatter, Meta::default());
        assert!(parsed.body.starts_with("---"));
    }

    #[test]
    fn malformed_yaml_in_a_terminated_block_is_an_error() {
        // Upstream's own test case for a `parse_failed` diagnostic.
        let parsed = parse_frontmatter::<Meta>("---\ndescription: [unterminated\n---\nBody");
        assert!(parsed.is_err());
    }

    #[test]
    fn crlf_is_normalized_before_parsing() {
        let parsed: Frontmatter<Meta> =
            parse_frontmatter("---\r\ndescription: X\r\n---\r\nBody\r\n").unwrap();
        assert_eq!(parsed.frontmatter.description, "X");
        assert_eq!(parsed.body, "Body");
    }

    #[test]
    fn structured_frontmatter_deserializes_into_the_target_struct() {
        let parsed: Frontmatter<Meta> = parse_frontmatter(
            "---\nname: my-skill\ndescription: >-\n  a folded\n  description\ndisable-model-invocation: true\n---\nBody",
        )
        .unwrap();
        assert_eq!(parsed.frontmatter.name.as_deref(), Some("my-skill"));
        assert_eq!(parsed.frontmatter.description, "a folded description");
        assert!(parsed.frontmatter.disable_model_invocation);
    }

    #[test]
    fn an_empty_block_yields_the_default() {
        let parsed: Frontmatter<Meta> = parse_frontmatter("---\n---\nBody").unwrap();
        assert_eq!(parsed.frontmatter, Meta::default());
        assert_eq!(parsed.body, "Body");
    }
}
