//! `form-cli` — drives the core through the C ABI with no Swift involved.
//!
//! This is the end-to-end proof for the boundary and the fastest loop for core work:
//! `chat` renders the live event stream to the terminal, so cadence and ordering are
//! directly observable. See `docs/specs/06-ffi.md` §2.
//!
//! Note that this binary links `form-ffi` and calls its `extern "C"` functions with C types
//! rather than using `form-core` directly — that is what makes it an FFI test rather than a
//! library test. `form-core` is imported only for the protocol *types* that `protocol-dump`
//! instantiates.

mod api;
mod bench;
mod dump;
mod render;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use api::{data, Core};
use form_ffi::form_abi_version;
use render::{BOLD, DIM, RED, RESET};

const USAGE: &str = "\
usage: form-cli <command> [args]

  seed                          create a store and populate it with mock data
  sessions                      list sessions and groups
  chat [session-id] <prompt>    dispatch a prompt and render the stream live
  stats [--range d7|d30|all]    pretty-print UsageStats
  search <query> [--session id] search sessions, or one session's transcript
  protocol-dump [dir]           write one JSON fixture per protocol variant
  bench                         markdown and stats latency budgets

  FORM_DATA_DIR   store location (default: a per-user directory under $TMPDIR)
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("chat");

    // Needs no core, and deliberately so: the fixtures describe the wire shape, not a
    // running store, and dumping them must work before any handler is implemented.
    if command == "protocol-dump" {
        std::process::exit(protocol_dump(args.get(1)));
    }
    if matches!(command, "help" | "--help" | "-h") {
        println!("{USAGE}");
        return;
    }

    let data_dir = std::env::var("FORM_DATA_DIR")
        .unwrap_or_else(|_| format!("{}/form-cli", std::env::temp_dir().display()));
    println!(
        "{DIM}form-cli · abi v{} · {data_dir}{RESET}",
        form_abi_version()
    );

    let core = match Core::new(json!({
        "dataDir": data_dir,
        "seedMockData": true,
        "logLevel": "info",
        "harnessSpeed": 1.0,
    })) {
        Ok(core) => core,
        Err(e) => {
            eprintln!("{RED}failed to start core: {e}{RESET}");
            std::process::exit(1);
        }
    };

    let code = match command {
        "seed" => seed(&core),
        "sessions" => sessions(&core),
        "chat" => chat(&core, &args[1..]),
        "stats" => stats(&core, &args[1..]),
        "search" => search(&core, &args[1..]),
        "bench" => {
            if bench::run(&core) {
                0
            } else {
                1
            }
        }
        other => {
            eprintln!("{RED}unknown command: {other}{RESET}\n\n{USAGE}");
            2
        }
    };
    // `core` drops here, which is `form_core_free` — including mid-stream, if `chat` timed
    // out. That path is the one spec 06 §1 requires not to deadlock.
    drop(core);
    std::process::exit(code);
}

// ---------------------------------------------------------------- commands

fn seed(core: &Core) -> i32 {
    // `seedMockData` is set for every invocation; the store decides whether it has work to
    // do. TODO(W1): the mock corpus itself is spec 01 §6 and is not populated yet.
    match data(&core.query(json!({ "type": "listSessions" }))) {
        Ok(list) => {
            let count = list["sessions"].as_array().map_or(0, Vec::len);
            println!("{count} session(s) in the store");
            if count == 0 {
                println!("{DIM}the mock corpus lands with W1 (spec 01 §6){RESET}");
            }
            0
        }
        Err(e) => fail(&e),
    }
}

fn sessions(core: &Core) -> i32 {
    match data(&core.query(json!({ "type": "listSessions", "includeArchived": true }))) {
        Ok(list) => {
            render::print_sessions(list);
            0
        }
        Err(e) => fail(&e),
    }
}

fn chat(core: &Core, args: &[String]) -> i32 {
    // `chat <prompt>` and `chat <session-id> <prompt>` both work; ids are recognizable, and
    // `make cli` uses the one-argument form.
    let (session_arg, prompt) = match args {
        [id, prompt, ..] if id.starts_with("ses_") => (Some(id.clone()), prompt.clone()),
        [prompt, ..] if !prompt.starts_with("ses_") => (None, prompt.clone()),
        [id] => (Some(id.clone()), "Add a health check endpoint".to_string()),
        [] => (None, "Add a health check endpoint".to_string()),
        _ => (None, "Add a health check endpoint".to_string()),
    };

    let stream = Arc::new(render::Stream::default());
    let token = core.subscribe(render::on_event, Arc::as_ptr(&stream) as *const ());
    if token <= 0 {
        return fail("subscribe failed");
    }

    let session_id = match session_arg {
        Some(id) => id,
        None => match create_session(core) {
            Ok(id) => id,
            Err(e) => return fail(&e),
        },
    };

    println!("{BOLD}session {session_id}{RESET}");
    println!("{DIM}> {prompt}{RESET}");

    let ack = core.dispatch(json!({
        "type": "sendPrompt",
        "sessionId": session_id,
        "text": prompt,
    }));
    if let Err(e) = data(&ack) {
        return fail(&e);
    }

    // The run is asynchronous; the callback signals completion.
    if !stream.wait(Duration::from_secs(120)) {
        eprintln!("{RED}timed out waiting for run_end{RESET}");
        core.unsubscribe(token);
        return 1;
    }

    match data(&core.query(json!({ "type": "getContextUsage", "sessionId": session_id }))) {
        Ok(usage) => println!(
            "{DIM}context {} / {} tokens{RESET}",
            render::num(&usage["used"]),
            render::num(&usage["total"])
        ),
        Err(e) => println!("{DIM}context usage unavailable: {e}{RESET}"),
    }

    core.unsubscribe(token);
    0
}

fn stats(core: &Core, args: &[String]) -> i32 {
    let range = flag(args, "--range").unwrap_or_else(|| "d30".to_string());
    let tz = flag(args, "--tz").unwrap_or_else(|| "UTC".to_string());
    match data(&core.query(json!({ "type": "getStats", "range": range, "tz": tz }))) {
        Ok(stats) => {
            render::print_stats(stats);
            0
        }
        Err(e) => fail(&e),
    }
}

fn search(core: &Core, args: &[String]) -> i32 {
    let Some(q) = args.iter().find(|a| !a.starts_with("--")).cloned() else {
        eprintln!("{RED}search needs a query{RESET}\n\n{USAGE}");
        return 2;
    };
    let query = match flag(args, "--session") {
        Some(session_id) => json!({ "type": "searchInSession", "sessionId": session_id, "q": q }),
        None => {
            let limit: usize = flag(args, "--limit")
                .and_then(|l| l.parse().ok())
                .unwrap_or(25);
            json!({ "type": "searchSessions", "q": q, "limit": limit })
        }
    };
    match data(&core.query(query)) {
        Ok(hits) => {
            render::print_hits(hits);
            0
        }
        Err(e) => fail(&e),
    }
}

fn protocol_dump(dir: Option<&String>) -> i32 {
    let root = dir.map(PathBuf::from).unwrap_or_else(default_fixture_dir);

    // Refuse to write a partial set: a fixture directory missing a variant reads, on the
    // Swift side, exactly like a protocol that does not have that variant.
    let missing = dump::missing_variants();
    if !missing.is_empty() {
        return fail(&format!(
            "no sample for {} — add one to dump.rs before dumping",
            missing.join(", ")
        ));
    }

    match dump::write_all(&root) {
        Ok(count) => {
            println!(
                "{count} fixtures written\n{DIM}{}{RESET}",
                dump::summary(&root)
            );
            0
        }
        Err(e) => fail(&format!("{}: {e}", root.display())),
    }
}

/// `core/tests/fixtures/protocol`, resolved from this crate rather than the shell's cwd —
/// the fixtures belong to the repository, not to wherever the tool was run from.
fn default_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/protocol")
}

// ---------------------------------------------------------------- helpers

fn create_session(core: &Core) -> Result<String, String> {
    let before = session_ids(core);
    data(&core.dispatch(json!({ "type": "createSession" })))?;
    session_ids(core)
        .into_iter()
        .find(|id| !before.contains(id))
        .ok_or_else(|| "createSession produced no session".to_string())
}

fn session_ids(core: &Core) -> Vec<String> {
    let list = core.query(json!({ "type": "listSessions", "includeArchived": true }));
    list["data"]["sessions"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|s| s["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `--name value`, the only option syntax this tool needs.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn fail(message: &str) -> i32 {
    eprintln!("{RED}{message}{RESET}");
    1
}
