//! Port of `packages/agent/src/harness/prompt-templates.ts`.
//!
//! Directory inputs load their direct `.md` children, non-recursively; file
//! inputs load one explicit `.md`. Missing paths and non-markdown files are
//! skipped, and read/parse failures become diagnostics rather than errors.

use pi_tools::{ExecutionEnvRef, FileErrorCode, FileInfo, FileKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::frontmatter::parse_frontmatter;
use crate::harness::types::PromptTemplate;

/// How long a first-line fallback description may be before it is elided.
const FALLBACK_DESCRIPTION_LENGTH: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTemplateDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
}

/// Warning produced while loading prompt templates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplateDiagnostic {
    pub code: PromptTemplateDiagnosticCode,
    pub message: String,
    pub path: String,
    /// Provenance tag from [`load_sourced_prompt_templates`], if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}

impl PromptTemplateDiagnostic {
    fn new(
        code: PromptTemplateDiagnosticCode,
        message: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            path: path.into(),
            source: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplateLoadResult {
    pub prompt_templates: Vec<PromptTemplate>,
    pub diagnostics: Vec<PromptTemplateDiagnostic>,
}

/// A template plus the provenance tag of the input it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcedPromptTemplate {
    pub prompt_template: PromptTemplate,
    pub source: Value,
}

/// One `{ path, source }` input to [`load_sourced_prompt_templates`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplateSource {
    pub path: String,
    pub source: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcedPromptTemplateLoadResult {
    pub prompt_templates: Vec<SourcedPromptTemplate>,
    pub diagnostics: Vec<PromptTemplateDiagnostic>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PromptTemplateFrontmatter {
    description: Option<String>,
    #[serde(rename = "argument-hint")]
    argument_hint: Option<String>,
}

/// Load prompt templates from one or more paths.
pub async fn load_prompt_templates(
    env: &ExecutionEnvRef,
    paths: &[String],
) -> PromptTemplateLoadResult {
    let mut result = PromptTemplateLoadResult::default();
    for path in paths {
        let info = match env.file_info(path, None).await {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    result.diagnostics.push(PromptTemplateDiagnostic::new(
                        PromptTemplateDiagnosticCode::FileInfoFailed,
                        error.message.clone(),
                        path,
                    ));
                }
                continue;
            }
        };
        match resolve_kind(env, &info, &mut result.diagnostics).await {
            Some(FileKind::Directory) => {
                let nested = load_templates_from_dir(env, &info.path).await;
                result.prompt_templates.extend(nested.prompt_templates);
                result.diagnostics.extend(nested.diagnostics);
            }
            Some(FileKind::File) if info.name.ends_with(".md") => {
                let loaded = load_template_from_file(env, &info.path, &info.name).await;
                result.prompt_templates.extend(loaded.prompt_templates);
                result.diagnostics.extend(loaded.diagnostics);
            }
            _ => {}
        }
    }
    result
}

/// Load prompt templates from source-tagged paths.
pub async fn load_sourced_prompt_templates(
    env: &ExecutionEnvRef,
    inputs: &[PromptTemplateSource],
) -> SourcedPromptTemplateLoadResult {
    let mut out = SourcedPromptTemplateLoadResult::default();
    for input in inputs {
        let loaded = load_prompt_templates(env, std::slice::from_ref(&input.path)).await;
        out.prompt_templates
            .extend(loaded.prompt_templates.into_iter().map(|prompt_template| {
                SourcedPromptTemplate {
                    prompt_template,
                    source: input.source.clone(),
                }
            }));
        out.diagnostics
            .extend(loaded.diagnostics.into_iter().map(|mut diagnostic| {
                diagnostic.source = Some(input.source.clone());
                diagnostic
            }));
    }
    out
}

/// Direct `.md` children only — upstream does not recurse here, unlike skills.
async fn load_templates_from_dir(env: &ExecutionEnvRef, dir: &str) -> PromptTemplateLoadResult {
    let mut result = PromptTemplateLoadResult::default();
    let mut entries = match env.list_dir(dir, None).await {
        Ok(entries) => entries,
        Err(error) => {
            result.diagnostics.push(PromptTemplateDiagnostic::new(
                PromptTemplateDiagnosticCode::ListFailed,
                error.message.clone(),
                dir,
            ));
            return result;
        }
    };
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    for entry in &entries {
        if resolve_kind(env, entry, &mut result.diagnostics).await != Some(FileKind::File) {
            continue;
        }
        if !entry.name.ends_with(".md") {
            continue;
        }
        let loaded = load_template_from_file(env, &entry.path, &entry.name).await;
        result.prompt_templates.extend(loaded.prompt_templates);
        result.diagnostics.extend(loaded.diagnostics);
    }
    result
}

async fn load_template_from_file(
    env: &ExecutionEnvRef,
    file_path: &str,
    file_name: &str,
) -> PromptTemplateLoadResult {
    let mut result = PromptTemplateLoadResult::default();

    let raw = match env.read_text_file(file_path, None).await {
        Ok(raw) => raw,
        Err(error) => {
            result.diagnostics.push(PromptTemplateDiagnostic::new(
                PromptTemplateDiagnosticCode::ReadFailed,
                error.message.clone(),
                file_path,
            ));
            return result;
        }
    };

    let parsed = match parse_frontmatter::<PromptTemplateFrontmatter>(&raw) {
        Ok(parsed) => parsed,
        Err(message) => {
            result.diagnostics.push(PromptTemplateDiagnostic::new(
                PromptTemplateDiagnosticCode::ParseFailed,
                message,
                file_path,
            ));
            return result;
        }
    };

    // With no declared description, the first non-blank body line stands in,
    // elided at 60 characters.
    let description = match parsed.frontmatter.description.filter(|d| !d.is_empty()) {
        Some(description) => description,
        None => parsed
            .body
            .split('\n')
            .find(|line| !line.trim().is_empty())
            .map(elide)
            .unwrap_or_default(),
    };

    result.prompt_templates.push(PromptTemplate {
        name: strip_md_suffix(file_name),
        description: Some(description),
        content: parsed.body,
    });
    result
}

fn elide(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() > FALLBACK_DESCRIPTION_LENGTH {
        let head: String = chars[..FALLBACK_DESCRIPTION_LENGTH].iter().collect();
        format!("{head}...")
    } else {
        line.to_string()
    }
}

fn strip_md_suffix(file_name: &str) -> String {
    let lower = file_name.to_ascii_lowercase();
    match lower.strip_suffix(".md") {
        Some(_) => file_name[..file_name.len() - 3].to_string(),
        None => file_name.to_string(),
    }
}

async fn resolve_kind(
    env: &ExecutionEnvRef,
    info: &FileInfo,
    diagnostics: &mut Vec<PromptTemplateDiagnostic>,
) -> Option<FileKind> {
    if matches!(info.kind, FileKind::File | FileKind::Directory) {
        return Some(info.kind);
    }
    let canonical = match env.canonical_path(&info.path, None).await {
        Ok(path) => path,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(PromptTemplateDiagnostic::new(
                    PromptTemplateDiagnosticCode::FileInfoFailed,
                    error.message.clone(),
                    &info.path,
                ));
            }
            return None;
        }
    };
    match env.file_info(&canonical, None).await {
        Ok(target) if matches!(target.kind, FileKind::File | FileKind::Directory) => {
            Some(target.kind)
        }
        Ok(_) => None,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(PromptTemplateDiagnostic::new(
                    PromptTemplateDiagnosticCode::FileInfoFailed,
                    error.message.clone(),
                    &info.path,
                ));
            }
            None
        }
    }
}

/// Parse an argument string using simple shell-style single and double quotes.
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for ch in args_string.chars() {
        match in_quote {
            Some(q) => {
                if ch == q {
                    in_quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    in_quote = Some(ch);
                } else if ch == ' ' || ch == '\t' {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(ch);
                }
            }
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Substitute `$1`, `$@`, `$ARGUMENTS`, `${@:N}` and `${@:N:L}` placeholders.
///
/// Hand-rolled rather than regex-driven so the three passes keep upstream's
/// order: positional, slice, then whole-argument-list forms.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let after_positional = replace_positional(content, args);
    let after_slice = replace_slices(&after_positional, args);
    let all_args = args.join(" ");
    after_slice
        .replace("$ARGUMENTS", &all_args)
        .replace("$@", &all_args)
}

/// `$1`, `$2`, ... — one-based, missing arguments substitute to the empty string.
fn replace_positional(content: &str, args: &[String]) -> String {
    let bytes: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '$' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            let digits: String = bytes[i + 1..j].iter().collect();
            let index: usize = digits.parse().unwrap_or(0);
            if index >= 1 {
                if let Some(value) = args.get(index - 1) {
                    out.push_str(value);
                }
            }
            i = j;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// `${@:N}` and `${@:N:L}` — one-based start, optional length.
fn replace_slices(content: &str, args: &[String]) -> String {
    let chars: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && chars.get(i + 1) == Some(&'{') && chars.get(i + 2) == Some(&'@') {
            if let Some((replacement, next)) = parse_slice(&chars, i, args) {
                out.push_str(&replacement);
                i = next;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn parse_slice(chars: &[char], start: usize, args: &[String]) -> Option<(String, usize)> {
    // start points at '$'; expect "${@:" then digits [":" digits] "}".
    if chars.get(start + 3) != Some(&':') {
        return None;
    }
    let mut i = start + 4;
    let digits_start = i;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    let start_index: usize = chars[digits_start..i]
        .iter()
        .collect::<String>()
        .parse()
        .ok()?;
    let mut length: Option<usize> = None;
    if chars.get(i) == Some(&':') {
        i += 1;
        let len_start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i == len_start {
            return None;
        }
        length = chars[len_start..i].iter().collect::<String>().parse().ok();
    }
    if chars.get(i) != Some(&'}') {
        return None;
    }
    let from = start_index.saturating_sub(1);
    let slice: &[String] = if from >= args.len() {
        &[]
    } else {
        match length {
            Some(len) => &args[from..(from + len).min(args.len())],
            None => &args[from..],
        }
    };
    Some((slice.join(" "), i + 1))
}

/// Format a prompt template invocation with positional arguments.
pub fn format_prompt_template_invocation(template: &PromptTemplate, args: &[String]) -> String {
    substitute_args(&template.content, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_tools::MemoryExecutionEnv;
    use std::sync::Arc;

    async fn env_with(files: &[(&str, &str)]) -> ExecutionEnvRef {
        let env = MemoryExecutionEnv::new("/work");
        for (path, content) in files {
            env.write_text(path, content).await;
        }
        Arc::new(env)
    }

    async fn load(env: &ExecutionEnvRef, paths: &[&str]) -> PromptTemplateLoadResult {
        let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        load_prompt_templates(env, &paths).await
    }

    #[tokio::test]
    async fn loads_markdown_templates_non_recursively_from_several_dirs() {
        let env = env_with(&[
            (
                "/work/a/one.md",
                "---\ndescription: One template\n---\nHello $1",
            ),
            ("/work/a/nested/ignored.md", "Ignored"),
            ("/work/b/two.md", "First line description\nBody"),
        ])
        .await;

        let result = load(&env, &["/work/a", "/work/b"]).await;
        assert_eq!(result.diagnostics, Vec::new());
        assert_eq!(
            result.prompt_templates,
            vec![
                PromptTemplate {
                    name: "one".into(),
                    description: Some("One template".into()),
                    content: "Hello $1".into(),
                },
                PromptTemplate {
                    name: "two".into(),
                    description: Some("First line description".into()),
                    content: "First line description\nBody".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn loads_an_explicit_markdown_file() {
        let env = env_with(&[(
            "/work/target.md",
            "---\ndescription: Target\n---\nTarget body",
        )])
        .await;

        let result = load(&env, &["/work/target.md"]).await;
        assert_eq!(
            result.prompt_templates,
            vec![PromptTemplate {
                name: "target".into(),
                description: Some("Target".into()),
                content: "Target body".into(),
            }]
        );
    }

    #[tokio::test]
    async fn a_missing_path_is_skipped_silently() {
        let env = env_with(&[]).await;
        let result = load(&env, &["/work/nope", "/work/nope.md"]).await;
        assert!(result.prompt_templates.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[tokio::test]
    async fn non_markdown_files_are_skipped() {
        let env = env_with(&[("/work/a/notes.txt", "text"), ("/work/a/one.md", "Body")]).await;
        let result = load(&env, &["/work/a"]).await;
        assert_eq!(result.prompt_templates.len(), 1);
        assert_eq!(result.prompt_templates[0].name, "one");
    }

    #[tokio::test]
    async fn a_long_first_line_fallback_description_is_elided() {
        let long = "x".repeat(80);
        let env = env_with(&[("/work/a/one.md", &long)]).await;
        let result = load(&env, &["/work/a"]).await;
        let description = result.prompt_templates[0].description.clone().unwrap();
        assert_eq!(description.chars().count(), FALLBACK_DESCRIPTION_LENGTH + 3);
        assert!(description.ends_with("..."));
    }

    #[tokio::test]
    async fn malformed_frontmatter_becomes_a_parse_failed_diagnostic() {
        let env = env_with(&[(
            "/work/broken.md",
            "---\ndescription: [unterminated\n---\nBody",
        )])
        .await;

        let result = load(&env, &["/work/broken.md"]).await;
        assert!(result.prompt_templates.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].code,
            PromptTemplateDiagnosticCode::ParseFailed
        );
        assert_eq!(result.diagnostics[0].path, "/work/broken.md");
    }

    #[tokio::test]
    async fn sourced_loading_tags_templates_and_diagnostics() {
        let env = env_with(&[
            (
                "/work/prompts/example.md",
                "---\ndescription: Example\n---\nExample body",
            ),
            (
                "/work/broken.md",
                "---\ndescription: [unterminated\n---\nBody",
            ),
        ])
        .await;

        let result = load_sourced_prompt_templates(
            &env,
            &[
                PromptTemplateSource {
                    path: "/work/prompts".into(),
                    source: serde_json::json!({ "type": "project" }),
                },
                PromptTemplateSource {
                    path: "/work/broken.md".into(),
                    source: serde_json::json!({ "type": "user" }),
                },
            ],
        )
        .await;

        assert_eq!(result.prompt_templates.len(), 1);
        assert_eq!(result.prompt_templates[0].prompt_template.name, "example");
        assert_eq!(
            result.prompt_templates[0].source,
            serde_json::json!({"type": "project"})
        );
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].source,
            Some(serde_json::json!({"type": "user"}))
        );
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_shell_style_quotes() {
        assert_eq!(
            parse_command_args("a \"b c\" 'd e' f"),
            v(&["a", "b c", "d e", "f"])
        );
        assert_eq!(parse_command_args("   "), Vec::<String>::new());
    }

    #[test]
    fn substitutes_command_arguments() {
        // Upstream test: "$1 ${@:2} $ARGUMENTS" with ["hello world", "test"].
        let content = "$1 ${@:2} $ARGUMENTS";
        let template = PromptTemplate {
            name: "one".into(),
            description: None,
            content: content.into(),
        };
        assert_eq!(
            format_prompt_template_invocation(&template, &v(&["hello world", "test"])),
            "hello world test hello world test"
        );
    }

    #[test]
    fn slice_with_length_and_missing_positional() {
        assert_eq!(substitute_args("${@:1:2}", &v(&["a", "b", "c"])), "a b");
        assert_eq!(substitute_args("[$3]", &v(&["a"])), "[]");
        assert_eq!(substitute_args("$@", &v(&["a", "b"])), "a b");
    }
}
