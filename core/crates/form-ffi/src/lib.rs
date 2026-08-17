//! The C ABI over `form-core`.
//!
//! Nine functions, JSON in and out, events on a callback. See `docs/specs/06-ffi.md`.
//!
//! Three rules govern every function here:
//! 1. **No panic may unwind into Swift** — that is undefined behaviour. Everything is
//!    wrapped in `catch_unwind`.
//! 2. **No pointer into Rust-owned memory is ever returned.** Strings are freshly
//!    allocated and freed only by `form_string_free`.
//! 3. **Events are delivered on one dedicated thread, in order, never concurrently.**

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use form_core::protocol::{CoreConfig, Envelope};
use form_core::{Core, CoreError, ABI_VERSION};

/// Bumped on any breaking protocol change. The client asserts a match at startup.
// Spelled as a literal rather than an alias so cbindgen can emit it as a `#define`; the
// assertion is what actually keeps it in step with the protocol module.
pub const FORM_ABI_VERSION: u32 = 1;
const _: () = assert!(FORM_ABI_VERSION == ABI_VERSION);

/// Receives one serialized `form_core::Event` per call. `json` is valid only for the
/// duration of the call — copy it.
pub type FormEventCallback =
    Option<extern "C" fn(json: *const c_char, len: usize, ctx: *mut c_void)>;

const MAGIC: u64 = 0x666f_726d_0001; // "form" + version — guards a stale or bogus handle.

/// How long `form_core_free` waits for the dispatcher before giving up and detaching it.
/// A wedged dispatcher must not hang the app on quit (spec 06 §1).
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Every handle handed out and not yet freed. Checking membership *before* dereferencing is
/// what makes a stale or bogus pointer an error instead of a use-after-free: the magic field
/// alone cannot be read safely once the box is gone.
///
/// This makes sequential misuse (double free, use after free, a pointer we never minted)
/// safe. It cannot make a free that races a concurrent call on the same handle safe — that
/// stays the caller's contract, as it is in any C API.
static LIVE_HANDLES: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn register_handle(ptr: *mut FormCoreHandle) {
    if let Ok(mut live) = LIVE_HANDLES.lock() {
        live.insert(ptr as usize);
    }
}

/// Returns true if `ptr` was live; removes it. A second call for the same pointer is false.
fn claim_handle(ptr: *mut FormCoreHandle) -> bool {
    LIVE_HANDLES
        .lock()
        .map(|mut live| live.remove(&(ptr as usize)))
        .unwrap_or(false)
}

fn is_live(ptr: *mut FormCoreHandle) -> bool {
    LIVE_HANDLES
        .lock()
        .map(|live| live.contains(&(ptr as usize)))
        .unwrap_or(false)
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(message: impl Into<String>) {
    let cstring = CString::new(message.into()).unwrap_or_else(|_| CString::new("error").unwrap());
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(cstring));
}

fn clear_last_error() {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
}

/// A callback plus its opaque Swift context. `ctx` is only ever passed back verbatim.
struct Subscriber {
    callback: extern "C" fn(*const c_char, usize, *mut c_void),
    ctx: usize,
}

// SAFETY: `ctx` is an opaque token owned by the caller, moved to the dispatcher thread and
// handed back unmodified. Swift keeps the referenced object alive until `unsubscribe`
// returns, which is guaranteed by the lock discipline below.
unsafe impl Send for Subscriber {}

pub struct FormCoreHandle {
    magic: u64,
    core: Arc<Core>,
    subscribers: Arc<Mutex<HashMap<i32, Subscriber>>>,
    /// Monotonic, never reused: a token freed by `unsubscribe` must not name a later
    /// subscriber, or a stale Swift-side unsubscribe would silence the wrong listener.
    next_token: AtomicI32,
    /// Bus tokens, so unsubscribing detaches from the core as well as the dispatcher.
    bus_tokens: Mutex<HashMap<i32, i32>>,
    tx: Option<Sender<(i32, String)>>,
    dispatcher: Option<JoinHandle<()>>,
    /// Checked before every delivery, so a dispatcher we end up detaching still stops
    /// calling into Swift the moment the callback it is stuck in returns.
    shutdown: Arc<AtomicBool>,
    /// Disconnects when the dispatcher thread exits — a `join` with a deadline.
    dispatcher_exit: std::sync::mpsc::Receiver<()>,
}

impl FormCoreHandle {
    /// Validate an incoming pointer. A bad handle is an error, not a crash.
    ///
    /// # Safety
    /// The returned reference is only valid while the caller holds the C-side contract that
    /// no other thread frees the handle concurrently.
    unsafe fn from_ptr<'a>(ptr: *mut FormCoreHandle) -> Option<&'a FormCoreHandle> {
        if ptr.is_null() {
            set_last_error("null core handle");
            return None;
        }
        if !is_live(ptr) {
            set_last_error("invalid or already-freed core handle");
            return None;
        }
        let handle = &*ptr;
        if handle.magic != MAGIC {
            set_last_error("core handle failed its magic check");
            return None;
        }
        Some(handle)
    }
}

fn to_c_string(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new("{\"ok\":false}").unwrap())
        .into_raw()
}

// ---------------------------------------------------------------- lifecycle

/// The ABI version this library was built against. Swift refuses to run on a mismatch.
#[no_mangle]
pub extern "C" fn form_abi_version() -> u32 {
    FORM_ABI_VERSION
}

/// Create a core. Returns NULL on failure; `form_last_error()` explains why.
///
/// # Safety
/// `config_json` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn form_core_new(config_json: *const c_char) -> *mut FormCoreHandle {
    let result = catch_unwind(AssertUnwindSafe(|| {
        clear_last_error();
        if config_json.is_null() {
            set_last_error("config_json is null");
            return std::ptr::null_mut();
        }
        let raw = match CStr::from_ptr(config_json).to_str() {
            Ok(s) => s,
            Err(e) => {
                set_last_error(format!("config_json is not utf-8: {e}"));
                return std::ptr::null_mut();
            }
        };
        let config: CoreConfig = match serde_json::from_str(raw) {
            Ok(c) => c,
            Err(e) => {
                set_last_error(format!("config_json is invalid: {e}"));
                return std::ptr::null_mut();
            }
        };
        let core = match Core::new(config) {
            Ok(c) => c,
            Err(e) => {
                set_last_error(e.to_string());
                return std::ptr::null_mut();
            }
        };

        // One dispatcher thread drains the queue and invokes callbacks, so delivery is
        // ordered and never concurrent no matter which tokio worker emitted the event.
        let subscribers: Arc<Mutex<HashMap<i32, Subscriber>>> = Arc::default();
        let (tx, rx) = channel::<(i32, String)>();
        let (exit_tx, dispatcher_exit) = channel::<()>();
        let dispatch_subs = subscribers.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let dispatch_shutdown = shutdown.clone();
        let dispatcher = std::thread::Builder::new()
            .name("form-events".to_string())
            .spawn(move || {
                // Owned solely by this thread; the disconnect on exit is the shutdown signal.
                let _exit_tx = exit_tx;
                while let Ok((token, json)) = rx.recv() {
                    if dispatch_shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    // The lock is held across the callback on purpose: it is what makes
                    // `form_core_unsubscribe` able to promise no further delivery once it
                    // returns. The callback must not re-enter the core (spec 00 §7).
                    let Ok(subs) = dispatch_subs.lock() else {
                        break;
                    };
                    let Some(sub) = subs.get(&token) else {
                        continue;
                    };
                    // A NUL cannot appear in serialized JSON, but dropping an event would
                    // silently break the ordering contract — sanitize rather than skip.
                    let cstring = CString::new(json.replace('\0', "\u{fffd}"))
                        .unwrap_or_else(|_| CString::new("{}").unwrap());
                    let bytes = cstring.as_bytes();
                    (sub.callback)(cstring.as_ptr(), bytes.len(), sub.ctx as *mut c_void);
                }
            });

        let dispatcher = match dispatcher {
            Ok(d) => d,
            Err(e) => {
                set_last_error(format!("dispatcher thread: {e}"));
                return std::ptr::null_mut();
            }
        };

        let ptr = Box::into_raw(Box::new(FormCoreHandle {
            magic: MAGIC,
            core,
            subscribers,
            next_token: AtomicI32::new(0),
            bus_tokens: Mutex::new(HashMap::new()),
            tx: Some(tx),
            dispatcher: Some(dispatcher),
            shutdown,
            dispatcher_exit,
        }));
        register_handle(ptr);
        ptr
    }));

    result.unwrap_or_else(|_| {
        set_last_error("panic in form_core_new");
        std::ptr::null_mut()
    })
}

/// Free a core. Safe to call while a run is streaming.
///
/// # Safety
/// `ptr` must come from `form_core_new` and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn form_core_free(ptr: *mut FormCoreHandle) {
    if ptr.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // Claiming the pointer is the double-free guard: the second call finds nothing and
        // never touches the freed allocation.
        if !claim_handle(ptr) {
            set_last_error("form_core_free on an invalid or already-freed handle");
            return;
        }
        let mut handle = Box::from_raw(ptr);
        handle.magic = 0;

        // Set first: it is what stops a *detached* dispatcher from calling back into Swift
        // once whatever it is stuck in finally returns.
        handle.shutdown.store(true, Ordering::SeqCst);

        // Detach every bus listener next. Each one holds a clone of the dispatcher's
        // sender, so the wait below would never see a disconnect while they are alive.
        // (Regression: this once deadlocked `free` during a live run.)
        let tokens: Vec<i32> = handle
            .bus_tokens
            .lock()
            .map(|mut t| t.drain().map(|(_, bus_token)| bus_token).collect())
            .unwrap_or_default();
        for bus_token in tokens {
            handle.core.unsubscribe(bus_token);
        }
        // Best-effort, and deliberately `try_lock`: the dispatcher holds this lock across a
        // delivery, so blocking here would reintroduce exactly the hang the timeout exists
        // to prevent. The `shutdown` flag is the guarantee; this is just tidiness.
        if let Ok(mut subs) = handle.subscribers.try_lock() {
            subs.clear();
        }

        // With the last sender gone, `recv` disconnects and the dispatcher exits.
        handle.tx.take();
        if let Some(thread) = handle.dispatcher.take() {
            match handle.dispatcher_exit.recv_timeout(SHUTDOWN_TIMEOUT) {
                // The thread dropped its end, so it has left the loop: joining is immediate.
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = thread.join();
                }
                // Wedged in a callback that never returns. Detach rather than hang the app;
                // `shutdown` guarantees no further delivery once it unwedges.
                _ => {
                    set_last_error("dispatcher did not stop within 2s; detached");
                    drop(thread);
                }
            }
        }
    }));
}

/// Register an event callback. Returns a positive token, or -1 on failure.
///
/// # Safety
/// `ptr` must be a live handle from `form_core_new`.
#[no_mangle]
pub unsafe extern "C" fn form_core_subscribe(
    ptr: *mut FormCoreHandle,
    callback: FormEventCallback,
    ctx: *mut c_void,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(handle) = FormCoreHandle::from_ptr(ptr) else {
            return -1;
        };
        let Some(callback) = callback else {
            set_last_error("callback is null");
            return -1;
        };
        let Some(tx) = handle.tx.clone() else {
            set_last_error("core is shutting down");
            return -1;
        };

        let token = handle.next_token.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let Ok(mut subs) = handle.subscribers.lock() else {
                set_last_error("subscriber registry poisoned");
                return -1;
            };
            subs.insert(
                token,
                Subscriber {
                    callback,
                    ctx: ctx as usize,
                },
            );
        }

        let bus_token = handle.core.subscribe(Arc::new(move |event| {
            if let Ok(json) = serde_json::to_string(event) {
                let _ = tx.send((token, json));
            }
        }));
        if let Ok(mut bus) = handle.bus_tokens.lock() {
            bus.insert(token, bus_token);
        }
        token
    }));
    result.unwrap_or(-1)
}

/// After this returns, the callback is guaranteed not to be invoked again.
///
/// # Safety
/// `ptr` must be a live handle from `form_core_new`.
#[no_mangle]
pub unsafe extern "C" fn form_core_unsubscribe(ptr: *mut FormCoreHandle, token: i32) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(handle) = FormCoreHandle::from_ptr(ptr) else {
            return;
        };
        let bus_token = handle
            .bus_tokens
            .lock()
            .ok()
            .and_then(|mut t| t.remove(&token));
        if let Some(bus_token) = bus_token {
            // Stops new events reaching the queue; the bus lock waits out an emit in flight.
            handle.core.unsubscribe(bus_token);
        }
        // Taking this lock waits out any delivery already in flight on the dispatcher, so
        // the "no callback after unsubscribe returns" promise covers events already queued.
        if let Ok(mut subs) = handle.subscribers.lock() {
            subs.remove(&token);
        }
    }));
}

// ---------------------------------------------------------------- calls

/// Synchronous read. Never returns NULL; failures come back as an error envelope.
///
/// # Safety
/// `ptr` must be a live handle; `query_json` a valid NUL-terminated UTF-8 string.
/// The returned string must be freed with `form_string_free`.
#[no_mangle]
pub unsafe extern "C" fn form_core_query(
    ptr: *mut FormCoreHandle,
    query_json: *const c_char,
) -> *mut c_char {
    call(ptr, query_json, |core, json| core.query_json(json))
}

/// Asynchronous command. Returns an ack envelope; outcomes arrive as events.
///
/// # Safety
/// `ptr` must be a live handle; `command_json` a valid NUL-terminated UTF-8 string.
/// The returned string must be freed with `form_string_free`.
#[no_mangle]
pub unsafe extern "C" fn form_core_dispatch(
    ptr: *mut FormCoreHandle,
    command_json: *const c_char,
) -> *mut c_char {
    call(ptr, command_json, |core, json| core.dispatch_json(json))
}

unsafe fn call(
    ptr: *mut FormCoreHandle,
    input: *const c_char,
    f: impl Fn(&Core, &str) -> String,
) -> *mut c_char {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(handle) = FormCoreHandle::from_ptr(ptr) else {
            return to_c_string(
                Envelope::from_error(&CoreError::InvalidRequest("invalid handle".into())).to_json(),
            );
        };
        if input.is_null() {
            return to_c_string(
                Envelope::from_error(&CoreError::InvalidRequest("null payload".into())).to_json(),
            );
        }
        match CStr::from_ptr(input).to_str() {
            Ok(json) => to_c_string(f(&handle.core, json)),
            Err(e) => to_c_string(
                Envelope::from_error(&CoreError::InvalidRequest(format!("not utf-8: {e}")))
                    .to_json(),
            ),
        }
    }));

    result.unwrap_or_else(|_| {
        to_c_string(
            Envelope::from_error(&CoreError::Internal(
                "panic crossing the ffi boundary".into(),
            ))
            .to_json(),
        )
    })
}

/// Release a string returned by `form_core_query` or `form_core_dispatch`.
///
/// # Safety
/// `s` must have come from this library and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn form_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// Last error on the *calling thread*, valid until the next failing call on that thread.
#[no_mangle]
pub extern "C" fn form_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `catch_unwind` in `call` is the only thing between a `form-core` panic and
    /// undefined behaviour in Swift, and no public entry point can be made to panic on
    /// demand — so drive the private helper directly.
    #[test]
    fn a_panic_inside_a_call_becomes_an_error_envelope() {
        let dir = std::env::temp_dir().join(format!("form-ffi-panic-{}", std::process::id()));
        let config = CString::new(
            serde_json::json!({ "dataDir": dir.to_string_lossy(), "harnessSpeed": 100.0 })
                .to_string(),
        )
        .unwrap();
        let core = unsafe { form_core_new(config.as_ptr()) };
        assert!(!core.is_null());

        let input = CString::new("{}").unwrap();
        let raw = unsafe { call(core, input.as_ptr(), |_, _| panic!("boom")) };
        assert!(!raw.is_null(), "a panic must still produce a JSON reply");
        let text = unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned();
        unsafe { form_string_free(raw) };
        unsafe { form_core_free(core) };
        let _ = std::fs::remove_dir_all(&dir);

        let envelope: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(envelope["ok"], serde_json::json!(false));
        assert_eq!(envelope["error"]["code"], serde_json::json!("internal"));
    }
}
