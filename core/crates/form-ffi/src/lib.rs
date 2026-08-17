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
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use form_core::protocol::{CoreConfig, Envelope};
use form_core::{Core, CoreError, ABI_VERSION};

/// Bumped with `form_core::ABI_VERSION`. Swift asserts a match at startup.
pub const FORM_ABI_VERSION: u32 = ABI_VERSION;

/// Receives one serialized [`form_core::Event`] per call. `json` is valid only for the
/// duration of the call — copy it.
pub type FormEventCallback =
    Option<extern "C" fn(json: *const c_char, len: usize, ctx: *mut c_void)>;

const MAGIC: u64 = 0x666f_726d_0001; // "form" + version — guards a stale or bogus handle.

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(message: impl Into<String>) {
    let cstring = CString::new(message.into()).unwrap_or_else(|_| CString::new("error").unwrap());
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(cstring));
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
    /// Bus tokens, so unsubscribing detaches from the core as well as the dispatcher.
    bus_tokens: Mutex<HashMap<i32, i32>>,
    tx: Option<Sender<(i32, String)>>,
    dispatcher: Option<JoinHandle<()>>,
}

impl FormCoreHandle {
    /// Validate an incoming pointer. A bad handle is an error, not a crash.
    unsafe fn from_ptr<'a>(ptr: *mut FormCoreHandle) -> Option<&'a FormCoreHandle> {
        if ptr.is_null() {
            set_last_error("null core handle");
            return None;
        }
        let handle = &*ptr;
        if handle.magic != MAGIC {
            set_last_error("invalid or already-freed core handle");
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

#[no_mangle]
pub extern "C" fn form_abi_version() -> u32 {
    FORM_ABI_VERSION
}

/// Create a core. Returns null on failure; `form_last_error()` explains why.
///
/// # Safety
/// `config_json` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn form_core_new(config_json: *const c_char) -> *mut FormCoreHandle {
    let result = catch_unwind(AssertUnwindSafe(|| {
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
        let dispatch_subs = subscribers.clone();
        let dispatcher = std::thread::Builder::new()
            .name("form-events".to_string())
            .spawn(move || {
                while let Ok((token, json)) = rx.recv() {
                    let subs = dispatch_subs.lock().expect("subscribers poisoned");
                    let Some(sub) = subs.get(&token) else {
                        continue;
                    };
                    let Ok(cstring) = CString::new(json) else {
                        continue;
                    };
                    let bytes = cstring.as_bytes();
                    // The callback must not re-enter the core; Swift's bridge only yields.
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        (sub.callback)(cstring.as_ptr(), bytes.len(), sub.ctx as *mut c_void)
                    }));
                }
            });

        let dispatcher = match dispatcher {
            Ok(d) => d,
            Err(e) => {
                set_last_error(format!("dispatcher thread: {e}"));
                return std::ptr::null_mut();
            }
        };

        Box::into_raw(Box::new(FormCoreHandle {
            magic: MAGIC,
            core,
            subscribers,
            bus_tokens: Mutex::new(HashMap::new()),
            tx: Some(tx),
            dispatcher: Some(dispatcher),
        }))
    }));

    result.unwrap_or_else(|_| {
        set_last_error("panic in form_core_new");
        std::ptr::null_mut()
    })
}

/// Free a core. Safe to call while a run is streaming.
///
/// # Safety
/// `ptr` must come from [`form_core_new`] and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn form_core_free(ptr: *mut FormCoreHandle) {
    if ptr.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let mut handle = Box::from_raw(ptr);
        if handle.magic != MAGIC {
            // Not ours, or already freed. Leak rather than corrupt the allocator.
            std::mem::forget(handle);
            set_last_error("form_core_free on an invalid handle");
            return;
        }
        handle.magic = 0;

        // Detach every bus listener first. Each one holds a clone of the dispatcher's
        // sender, so the loop below would never see a disconnect while they are alive.
        let tokens: Vec<i32> = handle
            .bus_tokens
            .lock()
            .expect("bus tokens poisoned")
            .drain()
            .map(|(_, bus_token)| bus_token)
            .collect();
        for bus_token in tokens {
            handle.core.unsubscribe(bus_token);
        }
        handle
            .subscribers
            .lock()
            .expect("subscribers poisoned")
            .clear();

        // With the last sender gone, `recv` disconnects and the dispatcher exits.
        handle.tx.take();
        if let Some(thread) = handle.dispatcher.take() {
            let _ = thread.join();
        }
    }));
}

/// # Safety
/// `ptr` must be a live handle from [`form_core_new`].
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
            return -1;
        };

        let token = {
            let mut subs = handle.subscribers.lock().expect("subscribers poisoned");
            let token = subs.keys().copied().max().unwrap_or(0) + 1;
            subs.insert(
                token,
                Subscriber {
                    callback,
                    ctx: ctx as usize,
                },
            );
            token
        };

        let bus_token = handle.core.subscribe(Arc::new(move |event| {
            if let Ok(json) = serde_json::to_string(event) {
                let _ = tx.send((token, json));
            }
        }));
        handle
            .bus_tokens
            .lock()
            .expect("bus tokens poisoned")
            .insert(token, bus_token);
        token
    }));
    result.unwrap_or(-1)
}

/// After this returns, the callback is guaranteed not to be invoked again.
///
/// # Safety
/// `ptr` must be a live handle from [`form_core_new`].
#[no_mangle]
pub unsafe extern "C" fn form_core_unsubscribe(ptr: *mut FormCoreHandle, token: i32) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(handle) = FormCoreHandle::from_ptr(ptr) else {
            return;
        };
        if let Some(bus_token) = handle
            .bus_tokens
            .lock()
            .expect("bus tokens poisoned")
            .remove(&token)
        {
            handle.core.unsubscribe(bus_token);
        }
        // Taking this lock waits out any delivery already in flight on the dispatcher.
        handle
            .subscribers
            .lock()
            .expect("subscribers poisoned")
            .remove(&token);
    }));
}

// ---------------------------------------------------------------- calls

/// # Safety
/// `ptr` must be a live handle; `query_json` a valid NUL-terminated UTF-8 string.
/// The returned string must be freed with [`form_string_free`].
#[no_mangle]
pub unsafe extern "C" fn form_core_query(
    ptr: *mut FormCoreHandle,
    query_json: *const c_char,
) -> *mut c_char {
    call(ptr, query_json, |core, json| core.query_json(json))
}

/// # Safety
/// `ptr` must be a live handle; `command_json` a valid NUL-terminated UTF-8 string.
/// The returned string must be freed with [`form_string_free`].
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
