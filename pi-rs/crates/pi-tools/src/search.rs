//! Glob and content search over the local filesystem.
//!
//! Upstream's `find` and `grep` tools shell out to `fd` and `rg`
//! (`.upstream/packages/coding-agent/src/core/tools/{find,grep}.ts`). This port
//! does the walk in-process with the `ignore` crate, which implements the same
//! `.gitignore` semantics ripgrep uses, so no external binary is downloaded at
//! runtime.
//!
//! Note the same limitation upstream has: the walk runs against the *local*
//! filesystem, not through the [`crate::FileSystem`] trait, because the ignore
//! machinery needs real directory handles. A host that substitutes a remote
//! environment should substitute its own search tools too.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;

/// Where a glob pattern is applied.
#[derive(Debug, Clone)]
pub struct PathMatcher {
    matcher: GlobMatcher,
    /// Patterns containing a separator match the whole path; others match the basename.
    full_path: bool,
}

impl PathMatcher {
    /// Build a matcher with fd's rule: a pattern without a `/` matches the
    /// basename; a pattern with one matches the full path and gets an implicit
    /// `**/` prefix so `src/**/*.rs` matches below the search root.
    pub fn new(pattern: &str) -> Result<Self, String> {
        let full_path = pattern.contains('/');
        let effective = if full_path {
            if pattern.starts_with('/') || pattern.starts_with("**/") || pattern == "**" {
                pattern.to_string()
            } else {
                format!("**/{pattern}")
            }
        } else {
            pattern.to_string()
        };
        let glob = Glob::new(&effective).map_err(|e| format!("Invalid glob pattern: {e}"))?;
        Ok(Self {
            matcher: glob.compile_matcher(),
            full_path,
        })
    }

    pub fn is_match(&self, path: &Path) -> bool {
        if self.full_path {
            self.matcher.is_match(path)
        } else {
            path.file_name()
                .is_some_and(|name| self.matcher.is_match(Path::new(name)))
        }
    }
}

/// `.gitignore` rules only apply inside a repository unless the caller opts out,
/// which is exactly what fd's `--no-require-git` does.
pub fn is_inside_git_repo(start: &Path) -> bool {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return true;
        }
        current = dir.parent();
    }
    false
}

/// Walk `root` respecting `.gitignore`, including hidden files (fd `--hidden`),
/// in a deterministic order.
pub fn walk(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(is_inside_git_repo(root))
        .sort_by_file_path(|a, b| a.cmp(b))
        .build()
}

/// Paths under `root` matching `pattern`, at most `limit` of them.
///
/// Both files and directories are returned, like `fd`. The root itself is never
/// a result.
pub fn glob_paths(root: &Path, pattern: &str, limit: usize) -> Result<Vec<PathBuf>, String> {
    let matcher = PathMatcher::new(pattern)?;
    let mut results = Vec::new();
    for entry in walk(root) {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path == root {
            continue;
        }
        if matcher.is_match(path) {
            results.push(path.to_path_buf());
            if results.len() >= limit {
                break;
            }
        }
    }
    Ok(results)
}

/// Files under `root` (or `root` itself when it is a file), optionally filtered
/// by a glob, in a deterministic order.
pub fn candidate_files(root: &Path, glob: Option<&str>) -> Result<Vec<PathBuf>, String> {
    let matcher = glob.map(PathMatcher::new).transpose()?;
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut files = Vec::new();
    for entry in walk(root) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if matcher.as_ref().is_none_or(|m| m.is_match(path)) {
            files.push(path.to_path_buf());
        }
    }
    Ok(files)
}

/// Relativize a result against the search root and normalize to posix separators.
pub fn relativize_result_path(result_path: &Path, search_path: &Path) -> String {
    let relative = result_path
        .strip_prefix(search_path)
        .unwrap_or(result_path)
        .to_string_lossy()
        .into_owned();
    let relative = if relative.is_empty() {
        result_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(relative)
    } else {
        relative
    };
    relative.replace(std::path::MAIN_SEPARATOR, "/")
}

/// Heuristic used by ripgrep: a NUL byte in the first block means binary.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|byte| *byte == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("src/nested/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(root.join("notes.md"), "# notes\n").unwrap();
        std::fs::write(root.join("target/build.rs"), "ignored\n").unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        dir
    }

    #[test]
    fn matches_basenames_for_patterns_without_a_separator() {
        let matcher = PathMatcher::new("*.rs").unwrap();
        assert!(matcher.is_match(Path::new("/a/b/main.rs")));
        assert!(!matcher.is_match(Path::new("/a/b/main.md")));
    }

    #[test]
    fn matches_full_paths_for_patterns_with_a_separator() {
        let matcher = PathMatcher::new("src/**/*.rs").unwrap();
        assert!(matcher.is_match(Path::new("/repo/src/nested/lib.rs")));
        assert!(!matcher.is_match(Path::new("/repo/tests/lib.rs")));
    }

    #[test]
    fn globs_respect_gitignore() {
        let dir = temp_tree();
        // `.gitignore` only applies inside a repo; make one.
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();

        let found = glob_paths(dir.path(), "*.rs", 100).unwrap();
        let names: Vec<String> = found
            .iter()
            .map(|p| relativize_result_path(p, dir.path()))
            .collect();
        assert!(names.contains(&"src/main.rs".to_string()));
        assert!(names.contains(&"src/nested/lib.rs".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("target/")));
    }

    #[test]
    fn globs_honour_the_limit() {
        let dir = temp_tree();
        let found = glob_paths(dir.path(), "*.rs", 1).unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn candidate_files_filters_by_glob() {
        let dir = temp_tree();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let files = candidate_files(dir.path(), Some("*.md")).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| relativize_result_path(p, dir.path()))
            .collect();
        assert_eq!(names, vec!["notes.md".to_string()]);
    }

    #[test]
    fn detects_binary_content() {
        assert!(looks_binary(b"abc\0def"));
        assert!(!looks_binary(b"plain text"));
    }

    #[test]
    fn rejects_invalid_globs() {
        assert!(PathMatcher::new("[").is_err());
    }
}
