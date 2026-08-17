//! Port of `packages/agent/src/harness/skills.ts`.
//!
//! Loads `SKILL.md` files from a directory tree over `pi-tools`' `ExecutionEnv`,
//! honouring `.gitignore`-style ignore files, and reports every problem as a
//! diagnostic rather than an error: one malformed skill must not stop the rest
//! from loading.

use std::sync::Arc;

use futures::future::BoxFuture;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use pi_tools::{ExecutionEnvRef, FileErrorCode, FileInfo, FileKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::frontmatter::parse_frontmatter;
use crate::harness::types::Skill;

const MAX_NAME_LENGTH: usize = 64;
const MAX_DESCRIPTION_LENGTH: usize = 1024;
const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
    InvalidMetadata,
}

/// Warning produced while loading skills. Currently only warnings are emitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDiagnostic {
    pub code: SkillDiagnosticCode,
    pub message: String,
    /// Path associated with the diagnostic.
    pub path: String,
    /// Provenance tag from [`load_sourced_skills`], if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}

impl SkillDiagnostic {
    fn new(code: SkillDiagnosticCode, message: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: path.into(),
            source: None,
        }
    }
}

/// Skills loaded from one or more directories, plus everything that went wrong.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillLoadResult {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

/// A skill plus the provenance tag of the input it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcedSkill {
    pub skill: Skill,
    pub source: Value,
}

/// One `{ path, source }` input to [`load_sourced_skills`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSource {
    pub path: String,
    /// Application-defined provenance. Upstream is generic over it; the port
    /// carries JSON, per the no-generics-in-public-API rule.
    pub source: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcedSkillLoadResult {
    pub skills: Vec<SourcedSkill>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "disable-model-invocation")]
    disable_model_invocation: Option<bool>,
}

/// Format a skill invocation prompt, optionally appending user instructions.
pub fn format_skill_invocation(skill: &Skill, additional_instructions: Option<&str>) -> String {
    let block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.file_path,
        dirname_env_path(&skill.file_path),
        skill.content
    );
    match additional_instructions {
        Some(extra) => format!("{block}\n\n{extra}"),
        None => block,
    }
}

/// Load skills from one or more directories.
///
/// Directories are traversed recursively. A directory containing a `SKILL.md`
/// contributes exactly that skill and is not descended into further; otherwise
/// its subdirectories are searched. Direct `.md` children of a *root* input
/// directory also load as skills. Missing inputs are skipped silently.
pub async fn load_skills(env: &ExecutionEnvRef, dirs: &[String]) -> SkillLoadResult {
    let mut result = SkillLoadResult::default();
    for dir in dirs {
        let Some(root_info) = file_info(env, dir, &mut result.diagnostics).await else {
            continue;
        };
        if resolve_kind(env, &root_info, &mut result.diagnostics).await != Some(FileKind::Directory)
        {
            continue;
        }
        let mut patterns: Vec<String> = Vec::new();
        let nested = load_skills_from_dir(
            env.clone(),
            root_info.path.clone(),
            true,
            &mut patterns,
            root_info.path.clone(),
        )
        .await;
        result.skills.extend(nested.skills);
        result.diagnostics.extend(nested.diagnostics);
    }
    result
}

/// Load skills from source-tagged directories.
///
/// Source values are preserved exactly and attached to every skill and
/// diagnostic. This crate never interprets them.
pub async fn load_sourced_skills(
    env: &ExecutionEnvRef,
    inputs: &[SkillSource],
) -> SourcedSkillLoadResult {
    let mut out = SourcedSkillLoadResult::default();
    for input in inputs {
        let loaded = load_skills(env, std::slice::from_ref(&input.path)).await;
        out.skills
            .extend(loaded.skills.into_iter().map(|skill| SourcedSkill {
                skill,
                source: input.source.clone(),
            }));
        out.diagnostics
            .extend(loaded.diagnostics.into_iter().map(|mut diagnostic| {
                diagnostic.source = Some(input.source.clone());
                diagnostic
            }));
    }
    out
}

/// Recursive descent. Boxed because an `async fn` cannot recurse directly.
///
/// `patterns` accumulates root-relative gitignore patterns as directories are
/// entered, matching upstream's single shared `ignore()` matcher.
fn load_skills_from_dir(
    env: ExecutionEnvRef,
    dir: String,
    include_root_files: bool,
    patterns: &mut Vec<String>,
    root_dir: String,
) -> BoxFuture<'_, SkillLoadResult> {
    Box::pin(async move {
        let mut result = SkillLoadResult::default();

        let Some(dir_info) = file_info(&env, &dir, &mut result.diagnostics).await else {
            return result;
        };
        if resolve_kind(&env, &dir_info, &mut result.diagnostics).await != Some(FileKind::Directory)
        {
            return result;
        }

        add_ignore_rules(&env, patterns, &dir, &root_dir, &mut result.diagnostics).await;
        let matcher = build_matcher(patterns);

        let entries = match env.list_dir(&dir, None).await {
            Ok(entries) => entries,
            Err(error) => {
                result.diagnostics.push(SkillDiagnostic::new(
                    SkillDiagnosticCode::ListFailed,
                    error.message.clone(),
                    &dir,
                ));
                return result;
            }
        };

        // A directory with a SKILL.md *is* the skill; do not descend further.
        for entry in &entries {
            if entry.name != "SKILL.md" {
                continue;
            }
            if resolve_kind(&env, entry, &mut result.diagnostics).await != Some(FileKind::File) {
                continue;
            }
            let rel = relative_env_path(&root_dir, &entry.path);
            if is_ignored(&matcher, &rel, false) {
                continue;
            }
            let loaded = load_skill_from_file(&env, &entry.path, &dir_info.name).await;
            result.skills.extend(loaded.skills);
            result.diagnostics.extend(loaded.diagnostics);
            return result;
        }

        let mut sorted = entries;
        sorted.sort_by(|a, b| a.name.cmp(&b.name));

        for entry in &sorted {
            if entry.name.starts_with('.') || entry.name == "node_modules" {
                continue;
            }
            let Some(kind) = resolve_kind(&env, entry, &mut result.diagnostics).await else {
                continue;
            };
            let rel = relative_env_path(&root_dir, &entry.path);
            let is_dir = kind == FileKind::Directory;
            if is_ignored(&matcher, &rel, is_dir) {
                continue;
            }

            if is_dir {
                let nested = load_skills_from_dir(
                    env.clone(),
                    entry.path.clone(),
                    false,
                    patterns,
                    root_dir.clone(),
                )
                .await;
                result.skills.extend(nested.skills);
                result.diagnostics.extend(nested.diagnostics);
                continue;
            }

            if !include_root_files || !entry.name.ends_with(".md") {
                continue;
            }
            let loaded = load_skill_from_file(&env, &entry.path, &dir_info.name).await;
            result.skills.extend(loaded.skills);
            result.diagnostics.extend(loaded.diagnostics);
        }

        result
    })
}

async fn add_ignore_rules(
    env: &ExecutionEnvRef,
    patterns: &mut Vec<String>,
    dir: &str,
    root_dir: &str,
    diagnostics: &mut Vec<SkillDiagnostic>,
) {
    let relative_dir = relative_env_path(root_dir, dir);
    let prefix = if relative_dir.is_empty() {
        String::new()
    } else {
        format!("{relative_dir}/")
    };

    for filename in IGNORE_FILE_NAMES {
        let ignore_path = match env
            .join_path(&[dir.to_string(), filename.to_string()], None)
            .await
        {
            Ok(path) => path,
            Err(error) => {
                diagnostics.push(SkillDiagnostic::new(
                    SkillDiagnosticCode::FileInfoFailed,
                    error.message.clone(),
                    dir,
                ));
                continue;
            }
        };
        let info = match env.file_info(&ignore_path, None).await {
            Ok(info) => info,
            Err(error) => {
                if error.code != FileErrorCode::NotFound {
                    diagnostics.push(SkillDiagnostic::new(
                        SkillDiagnosticCode::FileInfoFailed,
                        error.message.clone(),
                        &ignore_path,
                    ));
                }
                continue;
            }
        };
        if info.kind != FileKind::File {
            continue;
        }
        let content = match env.read_text_file(&ignore_path, None).await {
            Ok(content) => content,
            Err(error) => {
                diagnostics.push(SkillDiagnostic::new(
                    SkillDiagnosticCode::ReadFailed,
                    error.message.clone(),
                    &ignore_path,
                ));
                continue;
            }
        };
        patterns.extend(
            content
                .split('\n')
                .filter_map(|line| prefix_ignore_pattern(line.trim_end_matches('\r'), &prefix)),
        );
    }
}

/// Rewrite one gitignore line so it matches relative to the *root* directory.
fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = line;
    let mut negated = false;
    if let Some(rest) = pattern.strip_prefix('!') {
        negated = true;
        pattern = rest;
    } else if let Some(rest) = pattern.strip_prefix("\\!") {
        pattern = rest;
    }
    pattern = pattern.strip_prefix('/').unwrap_or(pattern);

    let prefixed = format!("{prefix}{pattern}");
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

/// Upstream uses the `ignore` npm package over root-relative paths. The `ignore`
/// crate's matcher is pure path matching (no filesystem access), so it works the
/// same way over `ExecutionEnv` paths, but it has no incremental `add`; rebuild
/// per directory instead.
fn build_matcher(patterns: &[String]) -> Option<Gitignore> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GitignoreBuilder::new("");
    for pattern in patterns {
        let _ = builder.add_line(None, pattern);
    }
    builder.build().ok()
}

fn is_ignored(matcher: &Option<Gitignore>, relative_path: &str, is_dir: bool) -> bool {
    matcher
        .as_ref()
        .map(|m| m.matched(relative_path, is_dir).is_ignore())
        .unwrap_or(false)
}

async fn load_skill_from_file(
    env: &ExecutionEnvRef,
    file_path: &str,
    parent_dir_name: &str,
) -> SkillLoadResult {
    let mut result = SkillLoadResult::default();

    let raw = match env.read_text_file(file_path, None).await {
        Ok(raw) => raw,
        Err(error) => {
            result.diagnostics.push(SkillDiagnostic::new(
                SkillDiagnosticCode::ReadFailed,
                error.message.clone(),
                file_path,
            ));
            return result;
        }
    };

    let parsed = match parse_frontmatter::<SkillFrontmatter>(&raw) {
        Ok(parsed) => parsed,
        Err(message) => {
            result.diagnostics.push(SkillDiagnostic::new(
                SkillDiagnosticCode::ParseFailed,
                message,
                file_path,
            ));
            return result;
        }
    };

    let description = parsed.frontmatter.description.clone();
    for error in validate_description(description.as_deref()) {
        result.diagnostics.push(SkillDiagnostic::new(
            SkillDiagnosticCode::InvalidMetadata,
            error,
            file_path,
        ));
    }

    let name = parsed
        .frontmatter
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| parent_dir_name.to_string());
    for error in validate_name(&name, parent_dir_name) {
        result.diagnostics.push(SkillDiagnostic::new(
            SkillDiagnosticCode::InvalidMetadata,
            error,
            file_path,
        ));
    }

    // A skill with no description is unusable by the model; the diagnostics
    // above already explain why it was skipped.
    let Some(description) = description.filter(|d| !d.trim().is_empty()) else {
        return result;
    };

    result.skills.push(Skill {
        name,
        description,
        content: parsed.body,
        file_path: file_path.to_string(),
        disable_model_invocation: Some(parsed.frontmatter.disable_model_invocation == Some(true)),
    });
    result
}

fn validate_name(name: &str, parent_dir_name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if name != parent_dir_name {
        errors.push(format!(
            "name \"{name}\" does not match parent directory \"{parent_dir_name}\""
        ));
    }
    if name.chars().count() > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({})",
            name.chars().count()
        ));
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"
                .to_string(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".to_string());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".to_string());
    }
    errors
}

fn validate_description(description: Option<&str>) -> Vec<String> {
    match description {
        None => vec!["description is required".to_string()],
        Some(d) if d.trim().is_empty() => vec!["description is required".to_string()],
        Some(d) if d.chars().count() > MAX_DESCRIPTION_LENGTH => vec![format!(
            "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({})",
            d.chars().count()
        )],
        Some(_) => Vec::new(),
    }
}

async fn file_info(
    env: &ExecutionEnvRef,
    path: &str,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<FileInfo> {
    match env.file_info(path, None).await {
        Ok(info) => Some(info),
        Err(error) => {
            // A missing input directory is not a problem worth reporting.
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(SkillDiagnostic::new(
                    SkillDiagnosticCode::FileInfoFailed,
                    error.message.clone(),
                    path,
                ));
            }
            None
        }
    }
}

/// Resolve a symlink to the kind of thing it points at. `FileSystem` never
/// follows symlinks implicitly, so this is explicit.
async fn resolve_kind(
    env: &ExecutionEnvRef,
    info: &FileInfo,
    diagnostics: &mut Vec<SkillDiagnostic>,
) -> Option<FileKind> {
    if matches!(info.kind, FileKind::File | FileKind::Directory) {
        return Some(info.kind);
    }
    let canonical = match env.canonical_path(&info.path, None).await {
        Ok(path) => path,
        Err(error) => {
            if error.code != FileErrorCode::NotFound {
                diagnostics.push(SkillDiagnostic::new(
                    SkillDiagnosticCode::FileInfoFailed,
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
                diagnostics.push(SkillDiagnostic::new(
                    SkillDiagnosticCode::FileInfoFailed,
                    error.message.clone(),
                    &info.path,
                ));
            }
            None
        }
    }
}

pub(crate) fn dirname_env_path(path: &str) -> String {
    let normalized = path.trim_end_matches(['/', '\\']);
    let separator = normalized
        .rfind(['/', '\\'])
        .map(|i| i as isize)
        .unwrap_or(-1);
    // Windows drive root, e.g. `C:\` — keep the drive.
    if separator == 2 && normalized.as_bytes().get(1) == Some(&b':') {
        return normalized[..3].to_string();
    }
    if separator <= 0 {
        return "/".to_string();
    }
    normalized[..separator as usize].to_string()
}

pub(crate) fn relative_env_path(root: &str, path: &str) -> String {
    let normalized_root = root.replace('\\', "/");
    let normalized_root = normalized_root.trim_end_matches('/');
    let normalized_path = path.replace('\\', "/");
    let normalized_path = normalized_path.trim_end_matches('/');
    if normalized_path == normalized_root {
        return String::new();
    }
    match normalized_path.strip_prefix(&format!("{normalized_root}/")) {
        Some(rest) => rest.to_string(),
        None => normalized_path.trim_start_matches('/').to_string(),
    }
}

/// The [`Skill`] list as an `Arc` for sharing with a system-prompt builder.
pub type SkillsRef = Arc<Vec<Skill>>;

#[cfg(test)]
mod tests {
    use super::*;
    use pi_tools::MemoryExecutionEnv;

    async fn env_with(files: &[(&str, &str)]) -> ExecutionEnvRef {
        let env = MemoryExecutionEnv::new("/work");
        for (path, content) in files {
            env.write_text(path, content).await;
        }
        Arc::new(env)
    }

    async fn load(env: &ExecutionEnvRef, dirs: &[&str]) -> SkillLoadResult {
        let dirs: Vec<String> = dirs.iter().map(|d| d.to_string()).collect();
        load_skills(env, &dirs).await
    }

    #[tokio::test]
    async fn loads_skill_md_files_recursively() {
        let env = env_with(&[
            (
                "/work/skills/alpha/SKILL.md",
                "---\nname: alpha\ndescription: Does alpha things\n---\nAlpha body",
            ),
            (
                "/work/skills/beta/SKILL.md",
                "---\nname: beta\ndescription: Does beta things\n---\nBeta body",
            ),
        ])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        assert_eq!(result.diagnostics, Vec::new());
        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert_eq!(result.skills[0].content, "Alpha body");
        assert_eq!(result.skills[0].file_path, "/work/skills/alpha/SKILL.md");
    }

    #[tokio::test]
    async fn a_directory_with_skill_md_is_not_descended_into() {
        let env = env_with(&[
            (
                "/work/skills/alpha/SKILL.md",
                "---\nname: alpha\ndescription: Alpha\n---\nBody",
            ),
            (
                "/work/skills/alpha/nested/SKILL.md",
                "---\nname: nested\ndescription: Nested\n---\nBody",
            ),
        ])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha"]);
    }

    #[tokio::test]
    async fn root_level_markdown_files_load_but_nested_ones_do_not() {
        let env = env_with(&[
            (
                "/work/skills/root-skill.md",
                "---\nname: skills\ndescription: A root skill\n---\nBody",
            ),
            (
                "/work/skills/nested/other.md",
                "---\nname: nested\ndescription: Not a SKILL.md\n---\nBody",
            ),
        ])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["skills"]);
    }

    #[tokio::test]
    async fn the_directory_name_is_the_default_skill_name() {
        let env = env_with(&[(
            "/work/skills/my-skill/SKILL.md",
            "---\ndescription: No explicit name\n---\nBody",
        )])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        assert_eq!(result.skills[0].name, "my-skill");
        assert_eq!(result.diagnostics, Vec::new());
    }

    #[tokio::test]
    async fn a_skill_without_a_description_is_skipped_with_a_diagnostic() {
        let env = env_with(&[(
            "/work/skills/alpha/SKILL.md",
            "---\nname: alpha\n---\nBody with no description",
        )])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        assert!(result.skills.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].code,
            SkillDiagnosticCode::InvalidMetadata
        );
        assert_eq!(result.diagnostics[0].message, "description is required");
    }

    #[tokio::test]
    async fn a_name_that_disagrees_with_its_directory_still_loads_but_warns() {
        let env = env_with(&[(
            "/work/skills/alpha/SKILL.md",
            "---\nname: Not_Valid\ndescription: Still loads\n---\nBody",
        )])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        assert_eq!(result.skills.len(), 1);
        let messages: Vec<&str> = result
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert!(messages
            .iter()
            .any(|m| m.contains("does not match parent directory")));
        assert!(messages.iter().any(|m| m.contains("invalid characters")));
    }

    #[tokio::test]
    async fn malformed_frontmatter_becomes_a_parse_failed_diagnostic() {
        let env = env_with(&[(
            "/work/skills/alpha/SKILL.md",
            "---\ndescription: [unterminated\n---\nBody",
        )])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        assert!(result.skills.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, SkillDiagnosticCode::ParseFailed);
        assert_eq!(result.diagnostics[0].path, "/work/skills/alpha/SKILL.md");
    }

    #[tokio::test]
    async fn disable_model_invocation_round_trips() {
        let env = env_with(&[(
            "/work/skills/alpha/SKILL.md",
            "---\nname: alpha\ndescription: Hidden\ndisable-model-invocation: true\n---\nBody",
        )])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        assert_eq!(
            result.skills[0].disable_model_invocation,
            Some(true),
            "must survive into the system-prompt filter"
        );
    }

    #[tokio::test]
    async fn ignore_files_exclude_matching_directories() {
        let env = env_with(&[
            ("/work/skills/.gitignore", "hidden/\n"),
            (
                "/work/skills/visible/SKILL.md",
                "---\nname: visible\ndescription: Visible\n---\nBody",
            ),
            (
                "/work/skills/hidden/SKILL.md",
                "---\nname: hidden\ndescription: Hidden\n---\nBody",
            ),
        ])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["visible"]);
    }

    #[tokio::test]
    async fn dotfiles_and_node_modules_are_skipped() {
        let env = env_with(&[
            (
                "/work/skills/.hidden/SKILL.md",
                "---\nname: hidden\ndescription: Hidden\n---\nBody",
            ),
            (
                "/work/skills/node_modules/pkg/SKILL.md",
                "---\nname: pkg\ndescription: Package\n---\nBody",
            ),
            (
                "/work/skills/real/SKILL.md",
                "---\nname: real\ndescription: Real\n---\nBody",
            ),
        ])
        .await;

        let result = load(&env, &["/work/skills"]).await;
        let names: Vec<&str> = result.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[tokio::test]
    async fn a_missing_input_directory_is_skipped_silently() {
        let env = env_with(&[]).await;
        let result = load(&env, &["/work/does-not-exist"]).await;
        assert!(result.skills.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[tokio::test]
    async fn sourced_loading_tags_skills_and_diagnostics() {
        let env = env_with(&[
            (
                "/work/project/alpha/SKILL.md",
                "---\nname: alpha\ndescription: Alpha\n---\nBody",
            ),
            (
                "/work/user/broken/SKILL.md",
                "---\ndescription: [unterminated\n---\nBody",
            ),
        ])
        .await;

        let result = load_sourced_skills(
            &env,
            &[
                SkillSource {
                    path: "/work/project".into(),
                    source: serde_json::json!({ "type": "project" }),
                },
                SkillSource {
                    path: "/work/user".into(),
                    source: serde_json::json!({ "type": "user" }),
                },
            ],
        )
        .await;

        assert_eq!(result.skills.len(), 1);
        assert_eq!(result.skills[0].skill.name, "alpha");
        assert_eq!(
            result.skills[0].source,
            serde_json::json!({"type": "project"})
        );
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].source,
            Some(serde_json::json!({"type": "user"}))
        );
    }

    #[test]
    fn formats_a_skill_invocation_block() {
        let skill = Skill {
            name: "alpha".into(),
            description: "Alpha".into(),
            content: "Do the thing".into(),
            file_path: "/skills/alpha/SKILL.md".into(),
            disable_model_invocation: None,
        };
        assert_eq!(
            format_skill_invocation(&skill, None),
            "<skill name=\"alpha\" location=\"/skills/alpha/SKILL.md\">\nReferences are relative to /skills/alpha.\n\nDo the thing\n</skill>"
        );
        assert!(
            format_skill_invocation(&skill, Some("Also do X")).ends_with("</skill>\n\nAlso do X")
        );
    }

    #[test]
    fn path_helpers_match_upstream() {
        assert_eq!(dirname_env_path("/a/b/c.md"), "/a/b");
        assert_eq!(dirname_env_path("/a"), "/");
        assert_eq!(relative_env_path("/root", "/root/a/b"), "a/b");
        assert_eq!(relative_env_path("/root", "/root"), "");
        assert_eq!(relative_env_path("/root", "/other/a"), "other/a");
    }
}
