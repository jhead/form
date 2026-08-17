//! Usage aggregation for the Home dashboard.
//!
//! **Owner: W3** (`docs/specs/03-stats.md`).
//!
//! The type below is the contract the dashboard renders — one query, one document, no
//! aggregation in Swift. What is here now is the shape plus a zero document so the
//! boundary compiles and the empty state is exercisable. W3 fills in the real aggregation
//! against the `turns` / `tool_invocations` tables and adds the remaining fields from the
//! spec (weekday×hour matrix, heatmap, leaderboards, latency histograms, cache, cost).

use serde::{Deserialize, Serialize};

use crate::protocol::{now_ms, ModelRef, StatsRange};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Headline {
    pub sessions: u64,
    pub messages: u64,
    pub turns: u64,
    pub total_tokens: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    pub active_days: u32,
    pub current_streak: u32,
    pub longest_streak: u32,
    pub peak_hour: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorite_model: Option<ModelRef>,
    pub total_cost: f64,
    pub avg_session_tokens: u64,
    pub avg_turn_duration_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyBucket {
    /// `YYYY-MM-DD` in the caller's timezone.
    pub date: String,
    pub sessions: u64,
    pub messages: u64,
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total_tokens: u64,
    pub cost: f64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HourlyBucket {
    pub hour: u8,
    pub total_tokens: u64,
    pub turns: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub range: Option<StatsRange>,
    pub generated_at: i64,
    pub headline: Headline,
    pub daily: Vec<DailyBucket>,
    pub hourly: Vec<HourlyBucket>,
    // TODO(W3): weekday_hour, heatmap, models, providers, tools, sessions_top, cache, cost,
    // latency — all defined in spec 03 §2.
}

impl UsageStats {
    /// A fully-populated zero document. Never `null`, never an error — the dashboard's
    /// empty state (F11.12) renders from this.
    pub fn empty(range: StatsRange) -> Self {
        Self {
            range: Some(range),
            generated_at: now_ms(),
            headline: Headline::default(),
            daily: Vec::new(),
            hourly: (0..24)
                .map(|hour| HourlyBucket {
                    hour,
                    ..Default::default()
                })
                .collect(),
        }
    }
}
