//! Port of `api/openai-prompt-cache.ts`.

pub const OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH: usize = 64;

/// Clamp a prompt cache key to OpenAI's 64-character limit.
///
/// Upstream counts code points (`Array.from(key)`), not UTF-16 units, so this
/// truncates on `chars()`.
pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    if key.chars().count() <= OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH {
        return Some(key.to_string());
    }
    Some(
        key.chars()
            .take(OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_short_keys() {
        assert_eq!(
            clamp_openai_prompt_cache_key(Some("abc")),
            Some("abc".into())
        );
        assert_eq!(clamp_openai_prompt_cache_key(None), None);
    }

    #[test]
    fn truncates_long_keys_by_code_point() {
        let key = "é".repeat(100);
        let clamped = clamp_openai_prompt_cache_key(Some(&key)).unwrap();
        assert_eq!(clamped.chars().count(), 64);
    }
}
