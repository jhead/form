//! `form-cli` — drives the core through the C ABI with no Swift involved.
//!
//! This is the end-to-end proof for the boundary and the fastest loop for core work:
//! `chat` renders the live event stream to the terminal, so cadence and ordering are
//! directly observable. See `docs/specs/06-ffi.md` §2.
//!
//! TODO(W6): `stats`, `search`, `protocol-dump`, `bench` subcommands.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

// The `form_ffi::*` items below *are* the `extern "C"` functions — calling them here
// exercises the exact C signatures and the C-string marshalling. That the symbols are
// actually exported from the staticlib is asserted separately by `make check-symbols`.
use form_ffi::{
    form_abi_version, form_core_dispatch, form_core_free, form_core_new, form_core_query,
    form_core_subscribe, form_last_error, form_string_free, FormCoreHandle,
};

struct Sink {
    done: AtomicBool,
}

extern "C" fn on_event(json: *const c_char, _len: usize, ctx: *mut c_void) {
    let text = unsafe { CStr::from_ptr(json) }
        .to_string_lossy()
        .into_owned();
    let value: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return,
    };
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("?");

    match kind {
        // Render deltas inline so the cadence is visible rather than described.
        "message_update" => {
            let event = &value["event"];
            match event.get("type").and_then(Value::as_str).unwrap_or("") {
                "text_delta" | "thinking_delta" => {
                    print!("{}", event["delta"].as_str().unwrap_or(""));
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                "thinking_start" => print!("\n\x1b[2m[thinking] "),
                "thinking_end" => println!("\x1b[0m"),
                "toolcall_end" => println!(
                    "\n\x1b[36m→ tool {}\x1b[0m",
                    event["toolCall"]["name"].as_str().unwrap_or("?")
                ),
                _ => {}
            }
        }
        "tool_execution_end" => println!("\x1b[36m← tool done\x1b[0m"),
        "run_end" => {
            println!(
                "\n\x1b[2m{} · {} tokens · {}ms\x1b[0m",
                value["outcome"].as_str().unwrap_or("?"),
                value["usage"]["totalTokens"].as_u64().unwrap_or(0),
                value["durationMs"].as_u64().unwrap_or(0)
            );
            let sink = unsafe { &*(ctx as *const Sink) };
            sink.done.store(true, Ordering::SeqCst);
        }
        _ => {}
    }
}

fn call(
    f: unsafe extern "C" fn(*mut FormCoreHandle, *const c_char) -> *mut c_char,
    core: *mut FormCoreHandle,
    payload: Value,
) -> Value {
    let input = CString::new(payload.to_string()).expect("payload");
    unsafe {
        let raw = f(core, input.as_ptr());
        let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
        form_string_free(raw);
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("chat");

    let data_dir = std::env::var("FORM_DATA_DIR")
        .unwrap_or_else(|_| format!("{}/.form-cli", std::env::temp_dir().display()));

    println!(
        "\x1b[2mform-cli · abi v{} · {}\x1b[0m",
        form_abi_version(),
        data_dir
    );

    let config = CString::new(
        json!({ "dataDir": data_dir, "seedMockData": true, "harnessSpeed": 1.0 }).to_string(),
    )
    .unwrap();

    let core = unsafe { form_core_new(config.as_ptr()) };
    if core.is_null() {
        let err = unsafe { CStr::from_ptr(form_last_error()) }.to_string_lossy();
        eprintln!("failed to start core: {err}");
        std::process::exit(1);
    }

    let sink = Arc::new(Sink {
        done: AtomicBool::new(false),
    });
    let token =
        unsafe { form_core_subscribe(core, Some(on_event), Arc::as_ptr(&sink) as *mut c_void) };
    assert!(token > 0, "subscribe failed");

    match command {
        "sessions" => {
            let out = call(form_core_query, core, json!({ "type": "listSessions" }));
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
        }
        "chat" => {
            let prompt = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "Add a health check endpoint".to_string());

            let created = call(form_core_dispatch, core, json!({ "type": "createSession" }));
            assert!(
                created["ok"].as_bool().unwrap_or(false),
                "createSession failed: {created}"
            );

            let sessions = call(form_core_query, core, json!({ "type": "listSessions" }));
            let session_id = sessions["data"]["sessions"][0]["id"]
                .as_str()
                .expect("session id")
                .to_string();

            println!("\x1b[1msession {session_id}\x1b[0m");
            println!("\x1b[2m> {prompt}\x1b[0m\n");

            let ack = call(
                form_core_dispatch,
                core,
                json!({ "type": "sendPrompt", "sessionId": session_id, "text": prompt }),
            );
            assert!(
                ack["ok"].as_bool().unwrap_or(false),
                "sendPrompt failed: {ack}"
            );

            // The run is asynchronous; the callback signals completion.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !sink.done.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }

            let usage = call(
                form_core_query,
                core,
                json!({ "type": "getContextUsage", "sessionId": session_id }),
            );
            println!(
                "\x1b[2mcontext {} / {}\x1b[0m",
                usage["data"]["used"].as_u64().unwrap_or(0),
                usage["data"]["total"].as_u64().unwrap_or(0)
            );
        }
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: form-cli [chat <prompt> | sessions]");
        }
    }

    unsafe { form_core_free(core) };
}
