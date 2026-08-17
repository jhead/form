//! Regenerates `core/include/form.h` from `form-ffi`'s public `extern "C"` surface.
//!
//! Usage: `form-headergen [output-path]` — defaults to `core/include/form.h`, resolved from
//! this crate's own location rather than the shell's cwd. `make headers` runs it in place;
//! `tests/header_drift.rs` runs it into a temporary file and diffs the two.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    // .../core/crates/form-ffi/tools/headergen
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ffi_dir = crate_dir
        .join("../..")
        .canonicalize()
        .expect("form-ffi crate directory");
    let default_out = ffi_dir.join("../../include/form.h");

    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(default_out);

    let config = cbindgen::Config::from_file(ffi_dir.join("cbindgen.toml"))
        .expect("core/crates/form-ffi/cbindgen.toml");

    match cbindgen::Builder::new()
        .with_crate(&ffi_dir)
        .with_config(config)
        .generate()
    {
        Ok(bindings) => {
            if let Some(parent) = out.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            bindings.write_to_file(&out);
            eprintln!("wrote {}", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cbindgen failed: {e}");
            ExitCode::FAILURE
        }
    }
}
