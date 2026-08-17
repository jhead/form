//! The C ABI's contract tests — spec 06 §4.
//!
//! Everything here goes through the `extern "C"` functions with C types, because the point
//! is to exercise what Swift actually calls: raw pointers, C strings, function pointers and
//! an opaque context. A Rust-level test of `form_core::Core` would prove nothing about this
//! layer.
//!
//! Panic containment is unit-tested in `src/lib.rs` rather than here, and deliberately: a
//! panic raised *inside* an `extern "C"` callback aborts at that function's own ABI boundary
//! before any `catch_unwind` of ours can see it, so a panicking callback cannot be simulated
//! from a test. What matters — that a panic in Rust code becomes an error envelope instead of
//! unwinding into Swift — is exercised against the shared `call` helper behind `form_core_query`
//! and `form_core_dispatch`.
//!
//! Run under ASAN when a nightly toolchain is around; nothing here needs it, but the
//! string-ownership tests are written so it is worth doing:
//!
//! ```text
//! RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p form-ffi \
//!     --target aarch64-apple-darwin
//! ```

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use form_ffi::{
    form_abi_version, form_core_dispatch, form_core_free, form_core_new, form_core_query,
    form_core_subscribe, form_core_unsubscribe, form_last_error, form_string_free, FormCoreHandle,
    FORM_ABI_VERSION,
};

// ---------------------------------------------------------------- harness

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

/// A core plus the temp directory it stores into. `free` is explicit in every test — this
/// only cleans up the directory, so a test that forgets to free still fails loudly.
struct Fixture {
    ptr: *mut FormCoreHandle,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn new_core(speed: f64) -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "form-ffi-{}-{}",
        std::process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    let config = CString::new(
        json!({
            "dataDir": dir.to_string_lossy(),
            "seedMockData": false,
            "logLevel": "error",
            "harnessSpeed": speed,
        })
        .to_string(),
    )
    .unwrap();
    let ptr = unsafe { form_core_new(config.as_ptr()) };
    assert!(!ptr.is_null(), "form_core_new failed: {:?}", last_error());
    Fixture { ptr, dir }
}

fn last_error() -> Option<String> {
    let raw = form_last_error();
    if raw.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(raw) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Round-trips one call through the C signature, including freeing the returned string.
fn c_call(
    f: unsafe extern "C" fn(*mut FormCoreHandle, *const c_char) -> *mut c_char,
    ptr: *mut FormCoreHandle,
    payload: Value,
) -> Value {
    let input = CString::new(payload.to_string()).unwrap();
    unsafe {
        let raw = f(ptr, input.as_ptr());
        assert!(!raw.is_null(), "query/dispatch must never return NULL");
        let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
        form_string_free(raw);
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

fn query(ptr: *mut FormCoreHandle, payload: Value) -> Value {
    c_call(form_core_query, ptr, payload)
}

fn dispatch(ptr: *mut FormCoreHandle, payload: Value) -> Value {
    c_call(form_core_dispatch, ptr, payload)
}

/// Everything a callback can tell us about how it was invoked.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<Value>>,
    threads: Mutex<HashSet<ThreadId>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    /// Set once `form_core_unsubscribe` has returned. Any delivery after that is a bug.
    sealed: AtomicBool,
    after_seal: AtomicUsize,
    /// Callbacks cannot assert — a panic inside an `extern "C"` fn aborts the process
    /// rather than unwinding — so contract violations are recorded and checked on the
    /// test thread instead.
    bad_len: AtomicUsize,
}

impl Recorder {
    fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }

    fn count_of(&self, kind: &str) -> usize {
        self.events()
            .iter()
            .filter(|e| e["type"] == json!(kind))
            .count()
    }

    fn wait_for(&self, kind: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.count_of(kind) > 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn wait_until(&self, timeout: Duration, f: impl Fn(&Recorder) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if f(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        f(self)
    }
}

extern "C" fn record(json_ptr: *const c_char, len: usize, ctx: *mut c_void) {
    let rec = unsafe { &*(ctx as *const Recorder) };

    let concurrent = rec.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    rec.max_in_flight.fetch_max(concurrent, Ordering::SeqCst);
    rec.threads
        .lock()
        .unwrap()
        .insert(std::thread::current().id());
    if rec.sealed.load(Ordering::SeqCst) {
        rec.after_seal.fetch_add(1, Ordering::SeqCst);
    }

    let bytes = unsafe { CStr::from_ptr(json_ptr) }.to_bytes();
    if bytes.len() != len {
        rec.bad_len.fetch_add(1, Ordering::SeqCst);
    }
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        rec.events.lock().unwrap().push(value);
    }

    rec.in_flight.fetch_sub(1, Ordering::SeqCst);
}

/// A callback that never returns until the test lets it, so shutdown has something to wedge
/// on. It touches only statics, because `form_core_free` may detach the dispatcher thread
/// while this is still running — a `ctx` pointer into the test's stack would then dangle.
static BLOCK_ENTERED: AtomicBool = AtomicBool::new(false);
static BLOCK_RELEASE: AtomicBool = AtomicBool::new(false);

extern "C" fn block_until_released(_json: *const c_char, _len: usize, _ctx: *mut c_void) {
    BLOCK_ENTERED.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !BLOCK_RELEASE.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn subscribe(ptr: *mut FormCoreHandle, rec: &Arc<Recorder>) -> i32 {
    let token = unsafe { form_core_subscribe(ptr, Some(record), Arc::as_ptr(rec) as *mut c_void) };
    assert!(token > 0, "subscribe returned {token}");
    token
}

/// Runs `f` on another thread and fails if it has not returned within `timeout`. Used for
/// the shutdown tests, where the failure mode being guarded against is a hang.
fn with_deadline(timeout: Duration, label: &str, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    assert!(
        rx.recv_timeout(timeout).is_ok(),
        "{label} did not complete within {timeout:?} — deadlock"
    );
    handle.join().unwrap();
}

fn session_ids(ptr: *mut FormCoreHandle) -> HashSet<String> {
    let list = query(ptr, json!({ "type": "listSessions" }));
    list["data"]["sessions"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|s| s["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A live session id, with a run not yet started. Diffed rather than indexed, because the
/// list's sort order belongs to W1 and is not this crate's business.
fn make_session(ptr: *mut FormCoreHandle) -> String {
    let before = session_ids(ptr);
    let ack = dispatch(ptr, json!({ "type": "createSession", "title": "fixture" }));
    assert_eq!(ack["ok"], json!(true), "createSession: {ack}");
    session_ids(ptr)
        .into_iter()
        .find(|id| !before.contains(id))
        .expect("createSession should add exactly one session")
}

// ---------------------------------------------------------------- abi

#[test]
fn abi_version_matches_the_core_and_the_committed_header() {
    assert_eq!(form_abi_version(), form_core::ABI_VERSION);
    assert_eq!(form_abi_version(), FORM_ABI_VERSION);

    let header = std::fs::read_to_string(header_path()).expect("core/include/form.h");
    let defined = header
        .lines()
        .find_map(|l| l.strip_prefix("#define FORM_ABI_VERSION "))
        .expect("the header must #define FORM_ABI_VERSION")
        .trim()
        .parse::<u32>()
        .expect("a numeric ABI version");
    assert_eq!(
        defined,
        form_abi_version(),
        "core/include/form.h is stale — regenerate it"
    );
}

fn header_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../include/form.h")
}

// ---------------------------------------------------------------- lifecycle

#[test]
fn new_and_free_round_trip() {
    let fx = new_core(100.0);
    let settings = query(fx.ptr, json!({ "type": "getSettings" }));
    assert_eq!(settings["ok"], json!(true), "{settings}");
    unsafe { form_core_free(fx.ptr) };
}

#[test]
fn new_reports_failure_through_last_error() {
    let ptr = unsafe { form_core_new(CString::new("not json").unwrap().as_ptr()) };
    assert!(ptr.is_null());
    let err = last_error().expect("last_error must explain the failure");
    assert!(err.contains("invalid"), "{err}");

    let ptr = unsafe { form_core_new(std::ptr::null()) };
    assert!(ptr.is_null());
    assert!(last_error().unwrap().contains("null"));
}

#[test]
fn last_error_is_per_thread() {
    // Fail on this thread…
    assert!(unsafe { form_core_new(std::ptr::null()) }.is_null());
    assert!(last_error().is_some());

    // …and a thread that has not failed still sees nothing.
    std::thread::spawn(|| assert!(form_last_error().is_null()))
        .join()
        .unwrap();
}

#[test]
fn a_bad_handle_is_an_error_not_a_crash() {
    // Null.
    let out = query(std::ptr::null_mut(), json!({ "type": "getSettings" }));
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["error"]["code"], json!("invalid_request"));

    // A pointer this library never minted. It must not be dereferenced.
    let bogus = 0xdead_beef_usize as *mut FormCoreHandle;
    let out = dispatch(bogus, json!({ "type": "createSession" }));
    assert_eq!(out["ok"], json!(false), "{out}");

    assert_eq!(
        unsafe { form_core_subscribe(bogus, Some(record), std::ptr::null_mut()) },
        -1
    );
    unsafe { form_core_unsubscribe(bogus, 1) };
    unsafe { form_core_free(std::ptr::null_mut()) };
}

#[test]
fn subscribe_rejects_a_null_callback() {
    let fx = new_core(100.0);
    assert_eq!(
        unsafe { form_core_subscribe(fx.ptr, None, std::ptr::null_mut()) },
        -1
    );
    assert!(last_error().unwrap().contains("callback"));
    unsafe { form_core_free(fx.ptr) };
}

#[test]
fn a_double_free_is_rejected() {
    let fx = new_core(100.0);
    unsafe { form_core_free(fx.ptr) };

    // The second free must not touch the freed allocation.
    unsafe { form_core_free(fx.ptr) };
    let err = last_error().expect("the second free should record an error");
    assert!(
        err.contains("already-freed") || err.contains("invalid"),
        "{err}"
    );

    // Use-after-free is likewise an envelope, not a crash.
    let out = query(fx.ptr, json!({ "type": "getSettings" }));
    assert_eq!(out["ok"], json!(false));
}

// ---------------------------------------------------------------- shutdown

#[test]
fn free_while_a_run_is_streaming_does_not_deadlock() {
    let fx = new_core(1.0); // real cadence, so the run is certainly still going
    let session = make_session(fx.ptr);
    let ack = dispatch(
        fx.ptr,
        json!({ "type": "sendPrompt", "sessionId": session, "text": "stream something" }),
    );
    assert_eq!(ack["ok"], json!(true), "{ack}");

    let ptr = fx.ptr as usize;
    with_deadline(
        Duration::from_secs(10),
        "form_core_free mid-run",
        move || {
            unsafe { form_core_free(ptr as *mut FormCoreHandle) };
        },
    );
}

/// Regression: the bus listener closure holds a clone of the dispatcher's `Sender`, so
/// dropping the handle's own sender is not enough to disconnect the queue. `form_core_free`
/// must detach the bus listeners *first* or the join never returns.
#[test]
fn free_while_subscribed_and_streaming_does_not_deadlock() {
    let fx = new_core(1.0);
    let rec = Arc::new(Recorder::default());
    subscribe(fx.ptr, &rec);

    let session = make_session(fx.ptr);
    dispatch(
        fx.ptr,
        json!({ "type": "sendPrompt", "sessionId": session, "text": "stream something" }),
    );
    assert!(
        rec.wait_for("run_start", Duration::from_secs(5)),
        "the run should have started before we free"
    );
    assert_eq!(rec.count_of("run_end"), 0, "the run should still be live");

    let ptr = fx.ptr as usize;
    let rec_for_thread = rec.clone();
    with_deadline(
        Duration::from_secs(10),
        "form_core_free with a live subscriber",
        move || {
            unsafe { form_core_free(ptr as *mut FormCoreHandle) };
            drop(rec_for_thread);
        },
    );

    // Nothing may arrive after free returns; the subscriber map is cleared before the wait.
    let before = rec.events().len();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(rec.events().len(), before, "delivery continued after free");
}

// ---------------------------------------------------------------- subscription

#[test]
fn no_event_is_delivered_after_unsubscribe_returns() {
    let fx = new_core(1.0);
    let rec = Arc::new(Recorder::default());
    let token = subscribe(fx.ptr, &rec);

    let session = make_session(fx.ptr);
    dispatch(
        fx.ptr,
        json!({ "type": "sendPrompt", "sessionId": session, "text": "keep streaming" }),
    );
    assert!(
        rec.wait_until(Duration::from_secs(5), |r| r.events().len() > 5),
        "expected a stream to be underway"
    );

    unsafe { form_core_unsubscribe(fx.ptr, token) };
    rec.sealed.store(true, Ordering::SeqCst);

    // The run is still producing events; none of them may reach us.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        rec.after_seal.load(Ordering::SeqCst),
        0,
        "a callback fired after unsubscribe returned"
    );

    unsafe { form_core_free(fx.ptr) };
}

/// A callback that never returns must not hang the app on quit: `form_core_free` waits two
/// seconds for the dispatcher and then detaches it (spec 06 §1).
#[test]
fn a_wedged_dispatcher_is_detached_rather_than_waited_on() {
    let fx = new_core(100.0);
    let token = unsafe { form_core_subscribe(fx.ptr, Some(block_until_released), null_ctx()) };
    assert!(token > 0);

    // `createSession` emits `session_created`, which is enough to wedge the dispatcher.
    dispatch(fx.ptr, json!({ "type": "createSession" }));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !BLOCK_ENTERED.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        BLOCK_ENTERED.load(Ordering::SeqCst),
        "the callback should be running"
    );

    let ptr = fx.ptr as usize;
    let started = Instant::now();
    with_deadline(
        Duration::from_secs(8),
        "form_core_free with a wedged dispatcher",
        move || unsafe { form_core_free(ptr as *mut FormCoreHandle) },
    );
    let elapsed = started.elapsed();

    BLOCK_RELEASE.store(true, Ordering::SeqCst);
    assert!(
        elapsed >= Duration::from_millis(1_800),
        "free returned in {elapsed:?} — it did not wait for the dispatcher at all"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "free took {elapsed:?} — the 2s detach timeout did not fire"
    );
}

fn null_ctx() -> *mut c_void {
    std::ptr::null_mut()
}

// ---------------------------------------------------------------- ordering

#[test]
fn two_concurrent_sessions_keep_their_streams_ordered() {
    let fx = new_core(60.0);
    let rec = Arc::new(Recorder::default());
    subscribe(fx.ptr, &rec);

    let a = make_session(fx.ptr);
    let b = make_session(fx.ptr);
    assert_ne!(a, b);

    for session in [&a, &b] {
        let ack = dispatch(
            fx.ptr,
            json!({ "type": "sendPrompt", "sessionId": session, "text": "concurrent run" }),
        );
        assert_eq!(ack["ok"], json!(true), "{ack}");
    }

    assert!(
        rec.wait_until(Duration::from_secs(30), |r| r.count_of("run_end") == 2),
        "both runs should finish; saw {:?}",
        rec.count_of("run_end")
    );

    // Delivery is single-threaded and never concurrent (spec 00 §7).
    assert_eq!(
        rec.threads.lock().unwrap().len(),
        1,
        "events must arrive on exactly one dispatcher thread"
    );
    assert_eq!(
        rec.max_in_flight.load(Ordering::SeqCst),
        1,
        "callbacks must never overlap"
    );
    assert_eq!(
        rec.bad_len.load(Ordering::SeqCst),
        0,
        "`len` must match the NUL-terminated payload on every delivery"
    );

    let events = rec.events();
    for session in [&a, &b] {
        assert_ordered_run(&events, session);
    }

    // The two streams interleave — that is the point of the test — but each session's own
    // events stay in order, which is what the previous loop asserted.
    unsafe { form_core_free(fx.ptr) };
}

/// Asserts the run-lifecycle contract from spec 00 §5.1 for one session.
fn assert_ordered_run(events: &[Value], session: &str) {
    let mine: Vec<&Value> = events
        .iter()
        .filter(|e| e["sessionId"] == json!(session))
        .collect();
    assert!(!mine.is_empty(), "no events for session {session}");

    let tags: Vec<&str> = mine
        .iter()
        .filter_map(|e| e["type"].as_str())
        .filter(|t| {
            matches!(
                *t,
                "run_start" | "turn_start" | "turn_end" | "run_end" | "message_update"
            )
        })
        .collect();

    let pos = |needle: &str| tags.iter().position(|t| *t == needle);
    assert_eq!(pos("run_start"), Some(0), "run_start must come first");
    assert_eq!(
        tags.iter().filter(|t| **t == "run_end").count(),
        1,
        "exactly one terminal run_end"
    );
    assert_eq!(
        pos("run_end"),
        Some(tags.len() - 1),
        "run_end must be the last event"
    );
    assert!(pos("turn_start") < pos("turn_end"));
    assert!(pos("turn_end") < pos("run_end"));

    // Every message_update sits strictly between its entry's message_start and message_end.
    let mut open: HashMap<String, bool> = HashMap::new();
    for event in &mine {
        let entry_id = |key: &str| event[key]["id"].as_str().map(str::to_string);
        match event["type"].as_str().unwrap_or("") {
            "message_start" => {
                if let Some(id) = entry_id("entry") {
                    open.insert(id, true);
                }
            }
            "message_end" => {
                if let Some(id) = entry_id("entry") {
                    open.insert(id, false);
                }
            }
            "message_update" => {
                let id = event["entryId"].as_str().unwrap_or_default().to_string();
                assert_eq!(
                    open.get(&id),
                    Some(&true),
                    "message_update for {id} outside its message_start/message_end"
                );
            }
            _ => {}
        }
    }
    assert!(
        open.values().all(|open| !open),
        "every message that started must have ended"
    );
}

// ---------------------------------------------------------------- strings

#[test]
fn returned_strings_are_independently_owned() {
    let fx = new_core(100.0);
    let payload = CString::new(json!({ "type": "getCatalog" }).to_string()).unwrap();

    // Hold many live results at once: each must be its own allocation, not a view into a
    // shared buffer that the next call overwrites.
    let mut raws = Vec::new();
    for _ in 0..64 {
        let raw = unsafe { form_core_query(fx.ptr, payload.as_ptr()) };
        assert!(!raw.is_null());
        raws.push(raw);
    }
    let first = unsafe { CStr::from_ptr(raws[0]) }.to_owned();
    for raw in &raws {
        assert_eq!(unsafe { CStr::from_ptr(*raw) }, first.as_c_str());
    }
    let addresses: HashSet<usize> = raws.iter().map(|p| *p as usize).collect();
    assert_eq!(addresses.len(), raws.len(), "results must not alias");

    for raw in raws {
        unsafe { form_string_free(raw) };
    }
    unsafe { form_string_free(std::ptr::null_mut()) }; // freeing NULL is a no-op
    unsafe { form_core_free(fx.ptr) };
}

#[test]
fn a_non_utf8_payload_is_an_error_envelope() {
    let fx = new_core(100.0);
    // 0xFF is not valid UTF-8; the boundary must reject it rather than assume.
    let bytes = CString::new(vec![0xFFu8, 0x7B, 0x7D]).unwrap();
    let raw = unsafe { form_core_query(fx.ptr, bytes.as_ptr()) };
    assert!(!raw.is_null());
    let text = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    unsafe { form_string_free(raw) };
    let out: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(out["ok"], json!(false));
    assert_eq!(out["error"]["code"], json!("invalid_request"));

    // A NULL payload is likewise an envelope, never a crash.
    let raw = unsafe { form_core_dispatch(fx.ptr, std::ptr::null()) };
    assert!(!raw.is_null());
    unsafe { form_string_free(raw) };

    unsafe { form_core_free(fx.ptr) };
}
