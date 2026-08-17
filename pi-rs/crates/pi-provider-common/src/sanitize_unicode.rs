//! Port of `packages/ai/src/utils/sanitize-unicode.ts` for `&str` inputs.
//!
//! Upstream strips unpaired UTF-16 surrogates on the way to the wire, because a
//! JavaScript string can hold one and most providers reject the resulting JSON.
//! A Rust `str` is guaranteed well-formed UTF-8, so the value this guards
//! against cannot exist and the function is the identity.
//!
//! [`pi_http::sanitize_unicode`] deliberately does **not** provide this: its two
//! functions handle the boundaries where an unpaired surrogate genuinely can
//! appear (UTF-16 from the Swift/FFI side, and lone `\uXXXX` escapes inside
//! provider JSON *text*). This one exists only so the adapter call sites keep
//! lining up with upstream's, and so the invariant is stated once instead of
//! once per adapter — the OpenAI and Mistral ports had a copy each.

/// Identity. See the module docs for why this is not a no-op worth deleting.
#[inline]
pub fn sanitize_surrogates(text: &str) -> &str {
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_rust_strings_pass_through_untouched() {
        for text in [
            "",
            "plain",
            "🙈 astral",
            "日本語",
            "lone-looking \\ud83d escape",
        ] {
            assert_eq!(sanitize_surrogates(text), text);
        }
    }
}
