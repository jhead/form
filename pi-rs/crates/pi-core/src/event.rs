//! The assistant message event protocol and its stream type.
//!
//! Port of `AssistantMessageEvent` (`packages/ai/src/types.ts`) and
//! `AssistantMessageEventStream` (`packages/ai/src/utils/event-stream.ts`).
//!
//! Contract, unchanged from upstream:
//! - emit `Start` first, then partial updates,
//! - terminate with exactly one of `Done` (success) or `Error`,
//! - request/model/runtime failures are encoded in the stream, never thrown.

use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::content::ToolCall;
use crate::message::{AssistantMessage, StopReason};

/// Terminal reason for a successful stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DoneReason {
    Stop,
    Length,
    ToolUse,
    Deferred,
}

impl From<DoneReason> for StopReason {
    fn from(r: DoneReason) -> Self {
        match r {
            DoneReason::Stop => StopReason::Stop,
            DoneReason::Length => StopReason::Length,
            DoneReason::ToolUse => StopReason::ToolUse,
            DoneReason::Deferred => StopReason::Deferred,
        }
    }
}

/// Terminal reason for a failed or aborted stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorReason {
    Aborted,
    Error,
}

impl From<ErrorReason> for StopReason {
    fn from(r: ErrorReason) -> Self {
        match r {
            ErrorReason::Aborted => StopReason::Aborted,
            ErrorReason::Error => StopReason::Error,
        }
    }
}

/// Streaming protocol event. Serializes exactly like the TypeScript union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageEvent {
    Start {
        partial: AssistantMessage,
    },
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        content: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
        partial: AssistantMessage,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: ToolCall,
        partial: AssistantMessage,
    },
    Done {
        reason: DoneReason,
        message: AssistantMessage,
    },
    Error {
        reason: ErrorReason,
        error: AssistantMessage,
    },
}

impl AssistantMessageEvent {
    /// The final message if this is a terminal event.
    pub fn terminal_message(&self) -> Option<&AssistantMessage> {
        match self {
            AssistantMessageEvent::Done { message, .. } => Some(message),
            AssistantMessageEvent::Error { error, .. } => Some(error),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal_message().is_some()
    }

    /// The in-progress snapshot carried by non-terminal events.
    pub fn partial(&self) -> Option<&AssistantMessage> {
        match self {
            AssistantMessageEvent::Start { partial }
            | AssistantMessageEvent::TextStart { partial, .. }
            | AssistantMessageEvent::TextDelta { partial, .. }
            | AssistantMessageEvent::TextEnd { partial, .. }
            | AssistantMessageEvent::ThinkingStart { partial, .. }
            | AssistantMessageEvent::ThinkingDelta { partial, .. }
            | AssistantMessageEvent::ThinkingEnd { partial, .. }
            | AssistantMessageEvent::ToolCallStart { partial, .. }
            | AssistantMessageEvent::ToolCallDelta { partial, .. }
            | AssistantMessageEvent::ToolCallEnd { partial, .. } => Some(partial),
            _ => None,
        }
    }
}

/// Boxed stream of protocol events. The concrete type is erased so it can cross
/// crate and FFI boundaries; every provider returns this.
pub struct AssistantMessageEventStream {
    inner: Pin<Box<dyn Stream<Item = AssistantMessageEvent> + Send>>,
}

impl AssistantMessageEventStream {
    pub fn new(stream: impl Stream<Item = AssistantMessageEvent> + Send + 'static) -> Self {
        Self {
            inner: Box::pin(stream),
        }
    }

    /// A stream that yields the given events in order. Useful for fakes/tests.
    pub fn from_events(events: Vec<AssistantMessageEvent>) -> Self {
        Self::new(futures_util::stream::iter(events))
    }

    /// Build a stream fed by a channel. Returns the sender for the producer task.
    pub fn channel(buffer: usize) -> (AssistantMessageEventSink, Self) {
        let (tx, rx) = tokio::sync::mpsc::channel(buffer.max(1));
        (
            AssistantMessageEventSink { tx },
            Self::new(tokio_stream::wrappers::ReceiverStream::new(rx)),
        )
    }

    /// Drain the stream, returning the terminal message.
    ///
    /// Errors are carried in the returned [`AssistantMessage`] (`stop_reason`
    /// `Error`/`Aborted`), matching upstream. `None` means the producer dropped
    /// the stream without a terminal event, which is a protocol violation.
    ///
    /// Named for what it returns rather than `collect`: an inherent `collect`
    /// shadows [`StreamExt::collect`](futures_util::StreamExt::collect), which
    /// forced every caller that wanted the *events* into UFCS.
    pub async fn into_final_message(mut self) -> Option<AssistantMessage> {
        use futures_util::StreamExt;
        let mut last_partial = None;
        while let Some(event) = self.inner.next().await {
            if let Some(msg) = event.terminal_message() {
                return Some(msg.clone());
            }
            if let Some(partial) = event.partial() {
                last_partial = Some(partial.clone());
            }
        }
        let _ = last_partial;
        None
    }

    /// Consume the stream, invoking `on_event` for each event, and return the
    /// terminal message. This is the shape Swift/FFI callers bridge to.
    pub async fn for_each_event<F>(mut self, mut on_event: F) -> Option<AssistantMessage>
    where
        F: FnMut(&AssistantMessageEvent) + Send,
    {
        use futures_util::StreamExt;
        let mut terminal = None;
        while let Some(event) = self.inner.next().await {
            on_event(&event);
            if let Some(msg) = event.terminal_message() {
                terminal = Some(msg.clone());
            }
        }
        terminal
    }
}

impl Stream for AssistantMessageEventStream {
    type Item = AssistantMessageEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl std::fmt::Debug for AssistantMessageEventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AssistantMessageEventStream")
    }
}

/// Producer half of [`AssistantMessageEventStream::channel`].
#[derive(Clone, Debug)]
pub struct AssistantMessageEventSink {
    tx: tokio::sync::mpsc::Sender<AssistantMessageEvent>,
}

impl AssistantMessageEventSink {
    /// Send an event. Returns `false` once the consumer has dropped the stream,
    /// which providers should treat as a cancellation signal.
    pub async fn send(&self, event: AssistantMessageEvent) -> bool {
        self.tx.send(event).await.is_ok()
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}
