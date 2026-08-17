//! The `UsageStats` document — spec 03 §2. Everything the Home dashboard renders.
//!
//! One query, one document: if a chart needs a number it is a field here, because the
//! alternative is aggregation in Swift and a round trip per chart.

use serde::{Deserialize, Serialize};

use crate::protocol::{now_ms, StatsRange};

/// A model identity without the thinking level — analytics group by *what ran*, and the
/// same model at two thinking levels is one row on every chart.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRefLite {
    pub provider_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// Consecutive local days with at least one turn, counted over all history rather
    /// than the selected period — a streak is a property of the user, not of the tab.
    pub current_streak: u32,
    pub longest_streak: u32,
    pub peak_hour: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorite_model: Option<ModelRefLite>,
    pub total_cost: f64,
    pub avg_session_tokens: u64,
    pub avg_turn_duration_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HourlyBucket {
    pub hour: u8,
    pub total_tokens: u64,
    pub turns: u64,
}

/// One cell of the GitHub-style calendar. `level` is `0` for exactly zero tokens and
/// `1..=4` for the quartiles of the *non-zero* distribution, so a light week still
/// shows contrast instead of four shades of nearly-empty.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeatmapCell {
    pub date: String,
    pub tokens: u64,
    pub sessions: u64,
    pub level: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStat {
    pub model: ModelRefLite,
    pub display_name: String,
    pub turns: u64,
    pub total_tokens: u64,
    pub share: f64,
    pub cost: f64,
    pub avg_ttft_ms: u64,
    pub avg_output_tps: f64,
    pub error_rate: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStat {
    pub provider_id: String,
    pub display_name: String,
    pub turns: u64,
    pub total_tokens: u64,
    pub share: f64,
    pub cost: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStat {
    pub name: String,
    pub invocations: u64,
    pub errors: u64,
    pub success_rate: f64,
    pub mean_duration_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRank {
    pub session_id: String,
    pub title: String,
    pub tokens: u64,
    pub duration_ms: u64,
    pub turns: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLeaderboards {
    pub by_tokens: Vec<SessionRank>,
    pub by_duration: Vec<SessionRank>,
    pub by_turns: Vec<SessionRank>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachePoint {
    pub date: String,
    pub read: u64,
    pub write: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStats {
    pub read: u64,
    pub write: u64,
    /// `read / (read + write)` — the shape of cache traffic, which is what F11.10 plots.
    pub hit_ratio: f64,
    /// USD not spent because those tokens were read from cache instead of billed as input.
    pub estimated_savings: f64,
    pub daily: Vec<CachePoint>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostPoint {
    pub date: String,
    pub cost: f64,
    /// Running total across the period, so the chart's overlay needs no Swift-side scan.
    pub cumulative: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostStats {
    pub total: f64,
    pub by_day: Vec<CostPoint>,
    pub by_provider: Vec<(String, f64)>,
    pub by_model: Vec<(ModelRefLite, f64)>,
    /// Mean daily cost over the trailing 14 days × 30; `0.0` under 3 active days.
    pub projected_monthly: f64,
}

/// A half-open `[lower_ms, upper_ms)` bucket; the last bin omits `upperMs` (unbounded).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistogramBin {
    pub lower_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upper_ms: Option<u64>,
    pub count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyStat {
    pub model: ModelRefLite,
    pub ttft_p50: u64,
    pub ttft_p90: u64,
    pub ttft_p99: u64,
    pub tps_p50: f64,
    pub tps_p90: f64,
    pub tps_p99: f64,
    /// Time-to-first-token distribution.
    pub histogram: Vec<HistogramBin>,
    pub samples: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub range: StatsRange,
    pub generated_at: i64,
    pub headline: Headline,
    /// Every local date in the period, zeros included — the chart must not interpolate
    /// across a missing day.
    pub daily: Vec<DailyBucket>,
    /// Always 24, hour `0..=23` local.
    pub hourly: Vec<HourlyBucket>,
    /// Always 7 × 24 tokens, outer index Monday = 0 (ISO), inner index local hour.
    pub weekday_hour: Vec<Vec<u64>>,
    pub heatmap: Vec<HeatmapCell>,
    pub models: Vec<ModelStat>,
    pub providers: Vec<ProviderStat>,
    pub tools: Vec<ToolStat>,
    pub sessions_top: SessionLeaderboards,
    pub cache: CacheStats,
    pub cost: CostStats,
    pub latency: Vec<LatencyStat>,
}

impl UsageStats {
    /// A fully-populated zero document. Never `null`, never an error — the dashboard's
    /// empty state (F11.12) renders from this, and so does a core whose store has no
    /// `turns` table yet.
    pub fn empty(range: StatsRange) -> Self {
        Self {
            range,
            generated_at: now_ms(),
            headline: Headline::default(),
            daily: Vec::new(),
            hourly: (0..24)
                .map(|hour| HourlyBucket {
                    hour,
                    ..Default::default()
                })
                .collect(),
            weekday_hour: vec![vec![0; 24]; 7],
            heatmap: Vec::new(),
            models: Vec::new(),
            providers: Vec::new(),
            tools: Vec::new(),
            sessions_top: SessionLeaderboards::default(),
            cache: CacheStats::default(),
            cost: CostStats::default(),
            latency: Vec::new(),
        }
    }
}
