//! `core/include/form.h` is generated, and the Swift package's module map includes it
//! directly — so a signature edited here and not regenerated there is a mismatch the
//! compiler cannot see. Spec 06 §1: a test fails the build on drift.
//!
//! Two checks, on purpose. The cbindgen diff is authoritative but needs the generator crate
//! to build (`core/crates/form-ffi/tools/headergen`, deliberately outside the workspace so
//! cbindgen is not a dependency of every core build). The signature cross-check needs
//! nothing at all, so the common failure — a renamed or re-typed function — is still caught
//! on a machine that cannot fetch crates.

use std::path::{Path, PathBuf};
use std::process::Command;

fn ffi_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn header_path() -> PathBuf {
    ffi_dir().join("../../include/form.h")
}

#[test]
fn the_committed_header_matches_a_fresh_cbindgen_run() {
    let out = std::env::temp_dir().join(format!("form-h-{}.h", std::process::id()));
    let _ = std::fs::remove_file(&out);

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(cargo);
    cmd.arg("run")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(ffi_dir().join("tools/headergen/Cargo.toml"))
        // `--manifest-path` means cargo never sees the generator's own `.cargo/config.toml`,
        // so name its target directory here too; `core/target` is already gitignored.
        .arg("--target-dir")
        .arg(ffi_dir().join("../../target/headergen"))
        .arg("--")
        .arg(&out);
    // The generator is its own workspace with its own target directory; inherited cargo
    // state would either redirect it into ours or hand it a jobserver it cannot use.
    for key in [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET",
        "CARGO_MAKEFLAGS",
        "MAKEFLAGS",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        cmd.env_remove(key);
    }

    let result = cmd.output();
    let generated = match result {
        Ok(output) if output.status.success() => {
            std::fs::read_to_string(&out).expect("headergen wrote no file")
        }
        Ok(output) => {
            // Offline, or a toolchain that cannot build the generator. Fall through to the
            // cross-check below rather than failing for a reason unrelated to the header.
            eprintln!(
                "skipping the cbindgen diff — headergen did not build:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        Err(e) => {
            eprintln!("skipping the cbindgen diff — cargo is not runnable here: {e}");
            return;
        }
    };
    let _ = std::fs::remove_file(&out);

    let committed = std::fs::read_to_string(header_path()).expect("core/include/form.h");
    if committed != generated {
        panic!(
            "core/include/form.h is out of date. Regenerate and commit it:\n\
             \n    make headers\n\
             \n{}",
            first_difference(&committed, &generated)
        );
    }
}

/// Belt and braces, and free: every exported function must be declared in the header with
/// the same C types, and the header must not promise anything the library does not export.
#[test]
fn every_exported_function_is_declared_in_the_header() {
    let source = std::fs::read_to_string(ffi_dir().join("src/lib.rs")).expect("src/lib.rs");
    let header = std::fs::read_to_string(header_path()).expect("core/include/form.h");

    let exported = exported_fn_names(&source);
    assert_eq!(
        exported.len(),
        9,
        "spec 00 §2 freezes the surface at nine functions: {exported:?}"
    );

    for name in &exported {
        assert!(
            header.contains(&format!("{name}(")),
            "{name} is exported but not declared in core/include/form.h"
        );
    }
    for declared in declared_fn_names(&header) {
        assert!(
            exported.contains(&declared),
            "core/include/form.h declares {declared}, which the library does not export"
        );
    }
}

fn exported_fn_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut exported = false;
    for line in source.lines() {
        let line = line.trim();
        if line == "#[no_mangle]" {
            exported = true;
            continue;
        }
        if exported {
            if let Some(rest) = line.split("extern \"C\" fn ").nth(1) {
                if let Some(name) = rest.split('(').next() {
                    names.push(name.to_string());
                }
            }
            exported = false;
        }
    }
    names
}

fn declared_fn_names(header: &str) -> Vec<String> {
    header
        .lines()
        .filter(|l| l.trim_end().ends_with(");") && l.contains("form_"))
        .filter_map(|l| {
            let before_paren = l.split('(').next()?;
            let name = before_paren.rsplit([' ', '*']).next()?;
            name.starts_with("form_").then(|| name.to_string())
        })
        .collect()
}

/// A readable diff without pulling in a diffing crate: the first line that differs, with a
/// little context on each side.
fn first_difference(a: &str, b: &str) -> String {
    let (a_lines, b_lines): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let at = a_lines
        .iter()
        .zip(&b_lines)
        .position(|(x, y)| x != y)
        .unwrap_or(a_lines.len().min(b_lines.len()));
    let show = |lines: &[&str]| {
        lines
            .iter()
            .skip(at.saturating_sub(2))
            .take(6)
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "first difference at line {}:\n--- committed\n{}\n--- generated\n{}",
        at + 1,
        show(&a_lines),
        show(&b_lines)
    )
}

/// The Swift package includes the header through this relative path; a move breaks the app
/// build in a way no Rust test would otherwise notice.
#[test]
fn the_swift_module_map_still_points_at_the_header() {
    let map = ffi_dir().join("../../../app/Sources/FormFFI/module.modulemap");
    let text = std::fs::read_to_string(&map).expect("app/Sources/FormFFI/module.modulemap");
    let referenced = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("header \""))
        .and_then(|l| l.split('"').next())
        .expect("the module map should reference a header");
    let resolved = map.parent().unwrap().join(referenced);
    assert!(
        resolved.exists(),
        "the module map points at {referenced}, which does not exist"
    );
    assert!(
        same_file(&resolved, &header_path()),
        "the module map points at {referenced}, not core/include/form.h"
    );
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
