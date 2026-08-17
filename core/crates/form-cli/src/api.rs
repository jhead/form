//! A thin, safe wrapper over the C ABI.
//!
//! Everything here goes through the `extern "C"` entry points with C types — that is what
//! makes `form-cli` an FFI test rather than a library test (spec 06 §2). Nothing else in
//! this binary is allowed to touch a raw pointer.

use std::ffi::{c_char, c_void, CStr, CString};

use serde_json::Value;

use form_ffi::{
    form_core_dispatch, form_core_free, form_core_new, form_core_query, form_core_subscribe,
    form_core_unsubscribe, form_last_error, form_string_free, FormCoreHandle,
};

pub struct Core {
    ptr: *mut FormCoreHandle,
}

impl Core {
    pub fn new(config: Value) -> Result<Self, String> {
        let config = CString::new(config.to_string()).map_err(|e| e.to_string())?;
        let ptr = unsafe { form_core_new(config.as_ptr()) };
        if ptr.is_null() {
            return Err(last_error().unwrap_or_else(|| "form_core_new failed".to_string()));
        }
        Ok(Self { ptr })
    }

    pub fn query(&self, payload: Value) -> Value {
        self.call(form_core_query, payload)
    }

    pub fn dispatch(&self, payload: Value) -> Value {
        self.call(form_core_dispatch, payload)
    }

    /// `ctx` must outlive the subscription; the caller keeps it alive.
    pub fn subscribe(
        &self,
        callback: extern "C" fn(*const c_char, usize, *mut c_void),
        ctx: *const (),
    ) -> i32 {
        unsafe { form_core_subscribe(self.ptr, Some(callback), ctx as *mut c_void) }
    }

    pub fn unsubscribe(&self, token: i32) {
        unsafe { form_core_unsubscribe(self.ptr, token) }
    }

    fn call(
        &self,
        f: unsafe extern "C" fn(*mut FormCoreHandle, *const c_char) -> *mut c_char,
        payload: Value,
    ) -> Value {
        let Ok(input) = CString::new(payload.to_string()) else {
            return Value::Null;
        };
        unsafe {
            let raw = f(self.ptr, input.as_ptr());
            if raw.is_null() {
                return Value::Null;
            }
            let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
            form_string_free(raw);
            serde_json::from_str(&text).unwrap_or(Value::Null)
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        unsafe { form_core_free(self.ptr) };
    }
}

pub fn last_error() -> Option<String> {
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

/// `{"ok":true,"data":…}` unwrapped, or the error rendered for a terminal.
pub fn data(envelope: &Value) -> Result<&Value, String> {
    if envelope["ok"].as_bool().unwrap_or(false) {
        Ok(&envelope["data"])
    } else {
        Err(format!(
            "{}: {}",
            envelope["error"]["code"].as_str().unwrap_or("error"),
            envelope["error"]["message"].as_str().unwrap_or("?")
        ))
    }
}
