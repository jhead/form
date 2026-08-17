//! Workspace confinement (F4.3).
//!
//! This is the API real tools will call once `pi-rs` lands, so it is written as a rejection
//! matrix rather than a happy path: every way out of the root — `..`, an absolute path, a
//! symlink pointing out, and on a case-insensitive volume a case-variant prefix — has to be
//! closed, and each has a test.

use std::path::{Component, Path, PathBuf};

use crate::error::{CoreError, Result};
use crate::protocol::ResolvedPath;

/// Resolve `candidate` against `root`, refusing anything that lands outside.
///
/// `root == None` is legal and yields `insideRoot: false` — a session with no workspace is
/// explicitly unconfined, and the UI labels it as such (F4.5).
pub fn resolve_in_workspace(root: Option<&Path>, candidate: &str) -> Result<ResolvedPath> {
    let Some(root) = root else {
        return Ok(ResolvedPath {
            resolved: candidate.to_string(),
            inside_root: false,
        });
    };
    if candidate.is_empty() {
        return Err(CoreError::InvalidRequest("empty path".to_string()));
    }
    // An interior NUL would be truncated by any syscall that later receives this path.
    if candidate.contains('\0') {
        return Err(CoreError::PathEscapesRoot(candidate.to_string()));
    }

    let root = root
        .canonicalize()
        .map_err(|e| CoreError::Io(format!("workspace root {}: {e}", root.display())))?;

    let joined = if Path::new(candidate).is_absolute() {
        PathBuf::from(candidate)
    } else {
        root.join(candidate)
    };

    let normalized = lexically_normalize(&joined)
        .ok_or_else(|| CoreError::PathEscapesRoot(candidate.to_string()))?;

    // Resolve symlinks over the part that exists, then re-attach the tail. Canonicalizing
    // the whole path would fail for a file the caller is about to create, and canonicalizing
    // nothing would let `root/link-to-etc/passwd` through.
    let resolved = resolve_existing_prefix(&normalized)?;

    if !contains(&root, &resolved) {
        return Err(CoreError::PathEscapesRoot(candidate.to_string()));
    }

    Ok(ResolvedPath {
        resolved: resolved.to_string_lossy().into_owned(),
        inside_root: true,
    })
}

/// Collapse `.` and `..` without touching the filesystem. Returns `None` if `..` would walk
/// above the filesystem root.
fn lexically_normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                out.pop();
                depth -= 1;
            }
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::Normal(c) => {
                out.push(c);
                depth += 1;
            }
        }
    }
    Some(out)
}

/// Canonicalize the longest existing ancestor and re-append the components below it.
fn resolve_existing_prefix(path: &Path) -> Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();

    loop {
        match existing.canonicalize() {
            Ok(real) => {
                let mut out = real;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return Ok(out);
            }
            Err(_) => {
                let Some(name) = existing.file_name().map(|n| n.to_os_string()) else {
                    // Walked to the root without finding anything that exists.
                    return Err(CoreError::Io(format!(
                        "cannot resolve path: {}",
                        path.display()
                    )));
                };
                tail.push(name);
                if !existing.pop() {
                    return Err(CoreError::Io(format!(
                        "cannot resolve path: {}",
                        path.display()
                    )));
                }
            }
        }
    }
}

/// Component-wise containment. Two checks, and they must agree.
///
/// The exact check alone is a false negative on a case-insensitive volume; a case-folded
/// check alone would accept a path whose real on-disk identity we have not confirmed. When
/// they disagree the path is a case-variant of the root that `canonicalize` could not pin
/// down (the tail does not exist yet) — exactly the collision spec 01 §5 says to reject.
fn contains(root: &Path, candidate: &Path) -> bool {
    let exact = candidate.starts_with(root);
    let folded = folded_starts_with(root, candidate);
    exact && folded
}

fn folded_starts_with(root: &Path, candidate: &Path) -> bool {
    let mut theirs = candidate.components();
    for ours in root.components() {
        let Some(other) = theirs.next() else {
            return false;
        };
        let a = ours.as_os_str().to_string_lossy().to_lowercase();
        let b = other.as_os_str().to_string_lossy().to_lowercase();
        if a != b {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("form-ws-{name}-{}", uuid::Uuid::new_v4().simple()));
            fs::create_dir_all(dir.join("src")).unwrap();
            fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn assert_escape(err: CoreError, candidate: &str) {
        assert_eq!(err.code(), "path_escapes_root", "for {candidate}");
    }

    #[test]
    fn accepts_paths_inside_the_root() {
        let root = TempRoot::new("inside");
        for candidate in ["src/main.rs", "./src/main.rs", "src/../src/main.rs", "src"] {
            let out = resolve_in_workspace(Some(root.path()), candidate)
                .unwrap_or_else(|e| panic!("{candidate}: {e}"));
            assert!(out.inside_root, "{candidate} should be inside");
            assert!(out.resolved.ends_with("src") || out.resolved.ends_with("main.rs"));
        }
    }

    #[test]
    fn accepts_a_file_that_does_not_exist_yet() {
        let root = TempRoot::new("new-file");
        let out = resolve_in_workspace(Some(root.path()), "src/generated/api.rs").unwrap();
        assert!(out.inside_root);
        assert!(out.resolved.ends_with("src/generated/api.rs"));
    }

    #[test]
    fn rejects_parent_dir_escapes() {
        let root = TempRoot::new("dotdot");
        for candidate in ["../secrets.txt", "src/../../secrets.txt", "../"] {
            let err = resolve_in_workspace(Some(root.path()), candidate).unwrap_err();
            assert_escape(err, candidate);
        }
    }

    #[test]
    fn rejects_walking_above_the_filesystem_root() {
        let root = TempRoot::new("above");
        let err = resolve_in_workspace(Some(root.path()), "/../../../../../../../../etc/passwd")
            .unwrap_err();
        assert_escape(err, "above fs root");
    }

    #[test]
    fn rejects_absolute_paths_outside_the_root() {
        let root = TempRoot::new("absolute");
        let err = resolve_in_workspace(Some(root.path()), "/etc/passwd").unwrap_err();
        assert_escape(err, "/etc/passwd");
    }

    #[test]
    fn accepts_an_absolute_path_inside_the_root() {
        let root = TempRoot::new("abs-inside");
        let inside = root.path().join("src/main.rs");
        let out = resolve_in_workspace(Some(root.path()), &inside.to_string_lossy()).unwrap();
        assert!(out.inside_root);
    }

    #[test]
    fn rejects_a_sibling_directory_sharing_the_root_prefix() {
        // The classic byte-prefix bug: `/tmp/root-evil` starts with `/tmp/root`.
        let root = TempRoot::new("prefix");
        let sibling = PathBuf::from(format!("{}-evil", root.path().display()));
        fs::create_dir_all(&sibling).unwrap();
        let err = resolve_in_workspace(Some(root.path()), &sibling.join("x.txt").to_string_lossy())
            .unwrap_err();
        let _ = fs::remove_dir_all(&sibling);
        assert_escape(err, "sibling prefix");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_pointing_out_of_the_root() {
        let root = TempRoot::new("symlink");
        let outside =
            std::env::temp_dir().join(format!("form-ws-outside-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.txt"), "shh").unwrap();
        std::os::unix::fs::symlink(&outside, root.path().join("escape")).unwrap();

        let err = resolve_in_workspace(Some(root.path()), "escape/secret.txt").unwrap_err();
        // The whole directory, not just the leaf, must be refused.
        let dir_err = resolve_in_workspace(Some(root.path()), "escape").unwrap_err();
        let _ = fs::remove_dir_all(&outside);

        assert_escape(err, "escape/secret.txt");
        assert_escape(dir_err, "escape");
    }

    #[cfg(unix)]
    #[test]
    fn accepts_a_symlink_that_stays_inside_the_root() {
        let root = TempRoot::new("symlink-in");
        std::os::unix::fs::symlink(root.path().join("src"), root.path().join("link")).unwrap();
        let out = resolve_in_workspace(Some(root.path()), "link/main.rs").unwrap();
        assert!(out.inside_root);
        assert!(out.resolved.ends_with("src/main.rs"));
    }

    #[test]
    fn containment_is_component_wise_and_case_agreement_is_required() {
        let root = Path::new("/a/root");
        assert!(contains(root, Path::new("/a/root")));
        assert!(contains(root, Path::new("/a/root/src/main.rs")));
        // Byte-prefix sibling.
        assert!(!contains(root, Path::new("/a/root-evil/x")));
        // Case variant: folded agrees, exact does not — refuse rather than guess.
        assert!(!contains(root, Path::new("/a/ROOT/x")));
        // Unrelated.
        assert!(!contains(root, Path::new("/b/root/x")));
    }

    /// macOS volumes are case-insensitive: `ROOT/x` and `root/x` name the same file, so a
    /// byte-wise prefix check disagrees with the filesystem. Containment is therefore decided
    /// *after* `canonicalize` has pinned every existing component to its real on-disk case,
    /// and any residual disagreement is refused rather than guessed at. Either outcome is
    /// correct; accepting a path that resolves outside the root never is.
    #[test]
    fn a_case_variant_prefix_never_resolves_outside_the_root() {
        let root = TempRoot::new("case");
        let real = root.path().canonicalize().unwrap();
        let variant = root
            .path()
            .to_string_lossy()
            .replace("form-ws-case", "FORM-WS-CASE");

        for tail in ["src/main.rs", "not/created/yet.txt"] {
            let candidate = format!("{variant}/{tail}");
            match resolve_in_workspace(Some(root.path()), &candidate) {
                Ok(out) => {
                    assert!(out.inside_root);
                    assert!(
                        Path::new(&out.resolved).starts_with(&real),
                        "{} escaped {}",
                        out.resolved,
                        real.display()
                    );
                }
                Err(e) => assert_escape(e, &candidate),
            }
        }
    }

    #[test]
    fn rejects_an_interior_nul() {
        let root = TempRoot::new("nul");
        let err = resolve_in_workspace(Some(root.path()), "src/main\0.rs").unwrap_err();
        assert_escape(err, "nul byte");
    }

    #[test]
    fn empty_candidate_is_an_invalid_request_not_an_escape() {
        let root = TempRoot::new("empty");
        let err = resolve_in_workspace(Some(root.path()), "").unwrap_err();
        assert_eq!(err.code(), "invalid_request");
    }

    #[test]
    fn missing_root_is_allowed_but_flagged() {
        let out = resolve_in_workspace(None, "/anywhere/at/all").unwrap();
        assert!(!out.inside_root);
        assert_eq!(out.resolved, "/anywhere/at/all");
    }

    #[test]
    fn a_nonexistent_root_is_an_io_error() {
        let missing = std::env::temp_dir().join("form-ws-does-not-exist-ever");
        let err = resolve_in_workspace(Some(&missing), "x.txt").unwrap_err();
        assert_eq!(err.code(), "io");
    }
}
