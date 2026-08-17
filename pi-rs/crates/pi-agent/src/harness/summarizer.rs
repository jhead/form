//! The [`Summarizer`] compaction and branch summarization run on.
//!
//! W10 left this as the one impure dependency of `pi_session::compaction`,
//! because the model call needs a provider and that is this crate's business.
//! Port of `completeSimpleWithRetries` from
//! `packages/agent/src/harness/compaction/compaction.ts`.

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AssistantMessage, CacheRetention, Context, Message, Model, SimpleStreamOptions, StopReason,
    StreamFn, ThinkingLevel, UserContent, UserMessage,
};
use pi_session::compaction::{CompactionError, SummarizationRequest, Summarizer};

/// A [`Summarizer`] backed by a [`StreamFn`].
///
/// Two things upstream is careful about and this preserves:
///
/// - **Cache isolation.** A summary is a standalone request over a transcript
///   prefix nothing else will ever send again. Writing it into the prompt cache
///   costs money for an entry that can never be reused, and reusing the main
///   conversation's `sessionId` would let a cache-affinity backend route the
///   summary onto the conversation's sticky connection. So every request goes
///   out with `cache_retention: None` and a **fresh** `session_id`.
/// - **Failures are values.** Provider errors come back as an
///   [`AssistantMessage`] with `stop_reason` `Error`/`Aborted`, matching
///   `ApiClient::stream`; `Err` is reserved for a stream that never terminated.
pub struct StreamFnSummarizer {
    stream_fn: StreamFn,
    model: Model,
    /// Base options. `cache_retention`, `session_id` and `signal` are always
    /// overwritten per request.
    options: SimpleStreamOptions,
}

impl StreamFnSummarizer {
    pub fn new(stream_fn: StreamFn, model: Model) -> Self {
        Self {
            stream_fn,
            model,
            options: SimpleStreamOptions::default(),
        }
    }

    /// Seed the provider options (api key, headers, timeouts, ...). The three
    /// fields this type owns are overwritten regardless.
    pub fn with_options(mut self, options: SimpleStreamOptions) -> Self {
        self.options = options;
        self
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The isolated options for one summarization request.
    fn request_options(&self, request: &SummarizationRequest) -> SimpleStreamOptions {
        let mut options = self.options.clone();
        options.stream.cache_retention = Some(CacheRetention::None);
        options.stream.session_id = Some(pi_core::uuidv7());
        options.stream.max_tokens = u64::try_from(request.max_tokens).ok();
        options.stream.request.signal = request.signal.clone();
        options.reasoning = request
            .thinking_level
            .as_deref()
            .and_then(parse_thinking_level);
        options
    }
}

fn parse_thinking_level(level: &str) -> Option<ThinkingLevel> {
    match level {
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        "max" => Some(ThinkingLevel::Max),
        // "off" and anything unrecognized mean "no reasoning".
        _ => None,
    }
}

#[async_trait]
impl Summarizer for StreamFnSummarizer {
    async fn summarize(
        &self,
        request: &SummarizationRequest,
    ) -> Result<AssistantMessage, CompactionError> {
        if request
            .signal
            .as_ref()
            .is_some_and(|signal| signal.is_aborted())
        {
            return Err(CompactionError::aborted("Compaction aborted"));
        }

        let context = Context {
            system_prompt: Some(request.system_prompt.clone()),
            messages: vec![Message::User(UserMessage {
                content: UserContent::Text(request.prompt.clone()),
                timestamp: pi_core::now_ms(),
            })],
            tools: None,
        };

        let stream = match (self.stream_fn)(
            self.model.clone(),
            context,
            self.request_options(request),
        )
        .await
        {
            Ok(stream) => stream,
            // The adapter rejected before any request work began, which is a
            // programmer error on its side rather than a provider failure.
            Err(error) => return Err(CompactionError::summarization_failed(error.message())),
        };

        match stream.into_final_message().await {
            Some(message) => Ok(message),
            None => Err(CompactionError::summarization_failed(
                "summarization stream ended without a done or error event",
            )),
        }
    }
}

/// Convenience: the summarizer as the `Arc<dyn Summarizer>` compaction takes.
pub fn stream_fn_summarizer(
    stream_fn: StreamFn,
    model: Model,
) -> pi_session::compaction::SummarizerRef {
    Arc::new(StreamFnSummarizer::new(stream_fn, model))
}

/// Whether a summarization response should be treated as a failure.
///
/// Upstream checks `stopReason` on the returned message rather than catching.
pub fn summary_failed(message: &AssistantMessage) -> bool {
    matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{mock_model, text_message, ScriptedStream, Turn};
    use pi_core::AbortHandle;

    fn request() -> SummarizationRequest {
        SummarizationRequest {
            system_prompt: "summarize".into(),
            prompt: "the conversation".into(),
            max_tokens: 4096,
            thinking_level: None,
            signal: None,
        }
    }

    #[tokio::test]
    async fn isolates_the_cache_and_uses_a_fresh_session_id_each_call() {
        let script = ScriptedStream::new(vec![]).with_fallback(Turn::Done(text_message("summary")));
        let mut options = SimpleStreamOptions::default();
        // A caller-supplied session id must not leak into the summary request.
        options.stream.session_id = Some("conversation-session".into());
        options.stream.cache_retention = Some(CacheRetention::Long);

        let summarizer = StreamFnSummarizer::new(script.clone().into_stream_fn(), mock_model())
            .with_options(options);

        summarizer.summarize(&request()).await.unwrap();
        summarizer.summarize(&request()).await.unwrap();

        let requests = script.requests();
        assert_eq!(requests.len(), 2);
        let first = requests[0].session_id.clone().unwrap();
        let second = requests[1].session_id.clone().unwrap();
        assert_ne!(first, "conversation-session");
        assert_ne!(second, "conversation-session");
        assert_ne!(first, second, "each summary needs its own session id");
    }

    #[tokio::test]
    async fn returns_the_summary_text() {
        let script = ScriptedStream::new(vec![Turn::Done(text_message("a summary"))]);
        let summarizer = StreamFnSummarizer::new(script.into_stream_fn(), mock_model());
        let message = summarizer.summarize(&request()).await.unwrap();
        assert_eq!(message.text(), "a summary");
        assert!(!summary_failed(&message));
    }

    #[tokio::test]
    async fn sends_the_prompt_as_a_single_user_message_under_the_system_prompt() {
        let script = ScriptedStream::new(vec![Turn::Done(text_message("s"))]);
        let summarizer = StreamFnSummarizer::new(script.clone().into_stream_fn(), mock_model());
        summarizer.summarize(&request()).await.unwrap();

        let context = &script.requests()[0].context;
        assert_eq!(context.system_prompt.as_deref(), Some("summarize"));
        assert_eq!(context.messages.len(), 1);
        assert_eq!(
            context.messages[0].as_user().unwrap().content.to_text(),
            "the conversation"
        );
        // Summaries never expose tools.
        assert!(context.tools.is_none());
    }

    #[tokio::test]
    async fn a_provider_failure_comes_back_as_a_message_not_an_error() {
        let failed = crate::testing::assistant_message(Vec::new(), StopReason::Error);
        let script = ScriptedStream::new(vec![Turn::Failed(failed)]);
        let summarizer = StreamFnSummarizer::new(script.into_stream_fn(), mock_model());

        let message = summarizer.summarize(&request()).await.unwrap();
        assert!(summary_failed(&message));
    }

    #[tokio::test]
    async fn an_already_aborted_signal_short_circuits() {
        let script = ScriptedStream::new(vec![]).with_fallback(Turn::Done(text_message("s")));
        let summarizer = StreamFnSummarizer::new(script.clone().into_stream_fn(), mock_model());

        let (handle, signal) = AbortHandle::new();
        handle.abort();
        let mut request = request();
        request.signal = Some(signal);

        let error = summarizer.summarize(&request).await.unwrap_err();
        assert_eq!(error.code(), "aborted");
        assert_eq!(script.call_count(), 0);
    }

    #[tokio::test]
    async fn thinking_level_maps_onto_reasoning_and_off_clears_it() {
        assert_eq!(parse_thinking_level("high"), Some(ThinkingLevel::High));
        assert_eq!(parse_thinking_level("off"), None);
        assert_eq!(parse_thinking_level("nonsense"), None);
    }
}
