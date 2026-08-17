//! Context-overflow detection. Port of `packages/ai/src/utils/overflow.ts`.
//!
//! Every provider words "your prompt is too long" differently and none of them
//! use a distinguishable status code, so this is a pattern match over the error
//! text. The pattern list is the accumulated result of hitting each provider in
//! production; the per-pattern comments name the provider so the list stays
//! auditable. Keep it in sync with upstream.
//!
//! Two providers do not error at all, and get structural detection instead:
//! z.ai silently accepts an oversized prompt (caught via `usage.input`), and
//! Xiaomi MiMo truncates the input to exactly fill the window and then stops
//! with `length` and zero output tokens.

use once_cell::sync::Lazy;
use pi_core::message::{AssistantMessage, StopReason};
use regex::Regex;

/// Source patterns, kept as text so [`overflow_patterns`] can expose them.
const OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"prompt is too long",                    // Anthropic token overflow
    r"request_too_large",                     // Anthropic request byte-size overflow (HTTP 413)
    r"input is too long for requested model", // Amazon Bedrock
    r"exceeds the context window",            // OpenAI (Completions & Responses)
    r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))", // LiteLLM & OpenAI-compatible proxies
    r"input token count.*exceeds the maximum", // Google (Gemini)
    r"maximum prompt length is \d+",           // xAI (Grok)
    r"reduce the length of the messages",      // Groq
    r"maximum context length is \d+ tokens",   // OpenRouter (most backends)
    r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?", // OpenRouter / Poolside
    r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)", // Together AI
    r"exceeds the limit of \d+",           // GitHub Copilot
    r"exceeds the available context size", // llama.cpp server
    r"greater than the context length",    // LM Studio
    r"context window exceeds limit",       // MiniMax
    r"exceeded model token limit",         // Kimi For Coding
    r"too large for model with \d+ maximum context length", // Mistral
    r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?", // DS4 server
    r"model_context_window_exceeded",      // z.ai finish_reason surfaced as error text
    r"prompt too long; exceeded (?:max )?context length", // Ollama explicit overflow
    r"range of input length should be",    // DashScope / Qwen Token Plan
    r"context[_ ]length[_ ]exceeded",      // generic fallback
    r"too many tokens",                    // generic fallback
    r"token limit exceeded",               // generic fallback
    r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)", // Cerebras: 400/413 with no body
];

/// Errors that match an overflow pattern but are not overflow. Bedrock's
/// throttling text ("Too many tokens, please wait…") is the motivating case.
const NON_OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"^(Throttling error|Service unavailable):", // AWS Bedrock human-readable prefixes
    r"rate limit",                               // generic rate limiting
    r"too many requests",                        // generic HTTP 429 wording
];

fn compile(sources: &[&str]) -> Vec<Regex> {
    sources
        .iter()
        .map(|source| {
            Regex::new(&format!("(?i){source}"))
                .unwrap_or_else(|e| panic!("overflow pattern {source:?}: {e}"))
        })
        .collect()
}

static OVERFLOW_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| compile(OVERFLOW_PATTERN_SOURCES));
static NON_OVERFLOW_PATTERNS: Lazy<Vec<Regex>> =
    Lazy::new(|| compile(NON_OVERFLOW_PATTERN_SOURCES));

/// The overflow patterns, for tests and for callers extending the list.
pub fn overflow_patterns() -> &'static [&'static str] {
    OVERFLOW_PATTERN_SOURCES
}

/// Whether an error message looks like a context-overflow error.
pub fn is_context_overflow_message(error_message: &str) -> bool {
    if NON_OVERFLOW_PATTERNS
        .iter()
        .any(|p| p.is_match(error_message))
    {
        return false;
    }
    OVERFLOW_PATTERNS.iter().any(|p| p.is_match(error_message))
}

/// Whether an assistant message represents a context overflow.
///
/// `context_window` enables the two structural checks; without it only the
/// error-text path runs.
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<i64>) -> bool {
    // Case 1: an explicit provider error.
    if message.stop_reason == StopReason::Error {
        if let Some(error_message) = message.error_message.as_deref() {
            if is_context_overflow_message(error_message) {
                return true;
            }
        }
    }

    let Some(context_window) = context_window.filter(|w| *w > 0) else {
        return false;
    };
    let input_tokens = message.usage.input + message.usage.cache_read;

    // Case 2: silent overflow — the request succeeded but used more than the window.
    if message.stop_reason == StopReason::Stop && input_tokens > context_window {
        return true;
    }

    // Case 3: the server truncated an oversized input to fill the window exactly,
    // leaving no room to generate.
    if message.stop_reason == StopReason::Length && message.usage.output == 0 {
        // 99% rather than 100%: providers round their own accounting.
        if (input_tokens as f64) >= (context_window as f64) * 0.99 {
            return true;
        }
    }

    false
}

/// Whether a `length` stop ended below the caller's intended output limit,
/// which makes one bounded compact-and-retry worthwhile.
///
/// `desired_max_output` must be the limit *before* any context-based clamping.
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: i64) -> bool {
    message.stop_reason == StopReason::Length
        && desired_max_output > 0
        && message.usage.output < desired_max_output
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::message::Usage;

    fn error_message(text: &str) -> AssistantMessage {
        let mut message = AssistantMessage::pending("openai-completions", "ollama", "qwen3.5:35b");
        message.stop_reason = StopReason::Error;
        message.error_message = Some(text.to_string());
        message
    }

    fn length_stop(input: i64, cache_read: i64, output: i64) -> AssistantMessage {
        let mut message =
            AssistantMessage::pending("openai-completions", "test-provider", "test-model");
        message.stop_reason = StopReason::Length;
        message.usage = Usage {
            input,
            cache_read,
            output,
            total_tokens: input + cache_read + output,
            ..Default::default()
        };
        message
    }

    #[test]
    fn detects_per_provider_overflow_wording() {
        let cases: &[(&str, i64)] = &[
            // Ollama
            ("400 `prompt too long; exceeded max context length by 100918 tokens`", 32768),
            // Together AI
            (
                "400 The input (516368 tokens) is longer than the model's context length (262144 tokens).",
                262144,
            ),
            // LiteLLM-wrapped OpenAI
            (
                "Error: 503 litellm.ServiceUnavailableError: litellm.MidStreamFallbackError: litellm.APIConnectionError: APIConnectionError: OpenAIException - Requested token count exceeds the model's maximum context length of 131072 tokens.",
                131072,
            ),
            // OpenAI-compatible, parenthesized form
            ("Error: 400 Input length (265330) exceeds model's maximum context length (262144).", 262144),
            // OpenRouter / Poolside
            (
                "Provider returned error: Input length 131393 exceeds the maximum allowed input length of 131040 tokens.",
                131072,
            ),
            // DS4
            ("400 Prompt has 256468 tokens, but the configured context size is 256000 tokens", 256000),
            ("Prompt has 5,958,968 tokens, but the configured context size is 256,000 tokens", 256000),
            // Anthropic
            ("prompt is too long: 213462 tokens > 200000 maximum", 200000),
            ("413 {\"error\":{\"type\":\"request_too_large\",\"message\":\"Request exceeds the maximum size\"}}", 200000),
            // Google
            ("The input token count (1196265) exceeds the maximum number of tokens allowed (1048575)", 1048576),
            // xAI
            ("This model's maximum prompt length is 131072 but the request contains 537812 tokens", 131072),
            // Groq
            ("Please reduce the length of the messages or completion", 8192),
            // Cerebras
            ("413 status code (no body)", 8192),
            // GitHub Copilot
            ("prompt token count of 200000 exceeds the limit of 128000", 128000),
            // Mistral
            ("Prompt contains 300000 tokens ... too large for model with 128000 maximum context length", 128000),
            // DashScope / Qwen
            ("Range of input length should be [1, 129024]", 129024),
        ];
        for (text, window) in cases {
            assert!(
                is_context_overflow(&error_message(text), Some(*window)),
                "should detect overflow: {text}"
            );
        }
    }

    #[test]
    fn does_not_treat_transient_failures_as_overflow() {
        let cases = [
            "500 `model runner crashed unexpectedly`",
            // Bedrock throttling would otherwise match /too many tokens/.
            "Throttling error: Too many tokens, please wait before trying again.",
            "Service unavailable: The service is temporarily unavailable.",
            "Rate limit exceeded, please retry after 30 seconds.",
            "Too many requests. Please slow down.",
        ];
        for text in cases {
            assert!(
                !is_context_overflow(&error_message(text), Some(200_000)),
                "should not detect overflow: {text}"
            );
        }
    }

    #[test]
    fn a_non_error_stop_reason_ignores_the_error_text() {
        let mut message = error_message("prompt is too long: 1 > 0");
        message.stop_reason = StopReason::Stop;
        assert!(!is_context_overflow(&message, None));
    }

    #[test]
    fn detects_silent_overflow_from_usage() {
        let mut message = AssistantMessage::pending("a", "z-ai", "glm");
        message.stop_reason = StopReason::Stop;
        message.usage = Usage {
            input: 100_000,
            cache_read: 40_000,
            ..Default::default()
        };
        assert!(is_context_overflow(&message, Some(128_000)));
        assert!(!is_context_overflow(&message, Some(200_000)));
        // Without the window there is nothing to compare against.
        assert!(!is_context_overflow(&message, None));
    }

    #[test]
    fn detects_xiaomi_style_length_stop_overflow() {
        let message = length_stop(58, 1_048_512, 0);
        assert!(is_context_overflow(&message, Some(1_048_576)));
    }

    #[test]
    fn a_zero_output_length_stop_far_below_the_window_is_not_overflow() {
        assert!(!is_context_overflow(&length_stop(100, 0, 0), Some(200_000)));
    }

    #[test]
    fn a_normal_length_stop_with_output_is_not_overflow() {
        assert!(!is_context_overflow(
            &length_stop(1_000, 0, 4_096),
            Some(200_000)
        ));
    }

    #[test]
    fn recoverable_length_compares_against_the_desired_limit() {
        assert!(is_recoverable_length(&length_stop(3, 253_584, 16), 128_000));
        assert!(!is_recoverable_length(&length_stop(4_062, 0, 1_024), 1_024));
        assert!(is_recoverable_length(&length_stop(100, 0, 0), 128_000));
        // A zero limit means the caller did not set one.
        assert!(!is_recoverable_length(&length_stop(100, 0, 0), 0));
        // Only `length` stops are recoverable this way.
        let mut stopped = length_stop(100, 0, 0);
        stopped.stop_reason = StopReason::Stop;
        assert!(!is_recoverable_length(&stopped, 128_000));
    }

    #[test]
    fn every_pattern_compiles() {
        assert_eq!(OVERFLOW_PATTERNS.len(), OVERFLOW_PATTERN_SOURCES.len());
        assert_eq!(overflow_patterns().len(), OVERFLOW_PATTERN_SOURCES.len());
    }
}
