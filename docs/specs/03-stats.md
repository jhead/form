# Spec 03 — Stats engine (`form-core::stats`)

> **Workstream W3.** Owns `core/crates/form-core/src/stats/`. Reads the `turns`,
> `tool_invocations`, `sessions` and `entries` tables (W1) — read-only, via SQL. Produces
> the single `UsageStats` document the Home dashboard renders (F11).

## 1. Principle

**One query, one document.** The Home page calls `getStats` once per period change and gets
everything it needs. No per-chart round trips, no aggregation in Swift. If a chart needs a
number, it is in `UsageStats`.

All bucketing is done in the caller's timezone, passed as an IANA id (`tz`), because
"tokens by hour of day" is meaningless in UTC. Use `chrono-tz`.

## 2. Shape

```rust
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub range: StatsRange,               // d7 | d30 | all
    pub generated_at: i64,
    pub headline: Headline,
    pub daily: Vec<DailyBucket>,
    pub hourly: Vec<HourlyBucket>,       // 24
    pub weekday_hour: Vec<Vec<u64>>,     // 7 × 24 tokens
    pub heatmap: Vec<HeatmapCell>,       // date, tokens, sessions, level 0–4
    pub models: Vec<ModelStat>,
    pub providers: Vec<ProviderStat>,
    pub tools: Vec<ToolStat>,
    pub sessions_top: SessionLeaderboards,
    pub cache: CacheStats,
    pub cost: CostStats,
    pub latency: Vec<LatencyStat>,       // per model
}

pub struct Headline {
    pub sessions: u64, pub messages: u64, pub turns: u64,
    pub total_tokens: u64, pub input: u64, pub output: u64,
    pub cache_read: u64, pub cache_write: u64, pub reasoning: u64,
    pub active_days: u32, pub current_streak: u32, pub longest_streak: u32,
    pub peak_hour: u8, pub favorite_model: Option<ModelRefLite>,
    pub total_cost: f64, pub avg_session_tokens: u64,
    pub avg_turn_duration_ms: u64,
}

pub struct DailyBucket { pub date: String,      // YYYY-MM-DD, local
                         pub sessions: u64, pub messages: u64, pub turns: u64,
                         pub input: u64, pub output: u64,
                         pub cache_read: u64, pub cache_write: u64,
                         pub total_tokens: u64, pub cost: f64,
                         pub duration_ms: u64 }

pub struct ModelStat { pub model: ModelRefLite, pub display_name: String,
                       pub turns: u64, pub total_tokens: u64, pub share: f64,
                       pub cost: f64, pub avg_ttft_ms: u64,
                       pub avg_output_tps: f64, pub error_rate: f64 }

pub struct LatencyStat { pub model: ModelRefLite,
                         pub ttft_p50: u64, pub ttft_p90: u64, pub ttft_p99: u64,
                         pub tps_p50: f64, pub tps_p90: f64, pub tps_p99: f64,
                         pub histogram: Vec<HistogramBin>, pub samples: u64 }

pub struct CacheStats { pub read: u64, pub write: u64, pub hit_ratio: f64,
                        pub estimated_savings: f64, pub daily: Vec<CachePoint> }

pub struct CostStats { pub total: f64, pub by_day: Vec<CostPoint>,
                       pub by_provider: Vec<(String, f64)>,
                       pub by_model: Vec<(ModelRefLite, f64)>,
                       pub projected_monthly: f64 }

pub struct SessionLeaderboards { pub by_tokens: Vec<SessionRank>,
                                 pub by_duration: Vec<SessionRank>,
                                 pub by_turns: Vec<SessionRank> }  // 10 each
```

## 3. Rules

- **Gap filling.** `daily` includes every date in range, zeros included — the chart must not
  interpolate across missing days. `hourly` is always 24 entries; `weekday_hour` always
  7 × 24.
- **Streaks** count consecutive local days with ≥ 1 turn. `current_streak` counts back from
  today and is 0 if today and yesterday are both empty (today alone being empty does not
  break a streak until the day rolls over).
- **Percentiles** use nearest-rank on the sorted sample, computed in Rust over the raw
  `turns` rows, not in SQL.
- **Throughput** is `output / (duration_ms - ttft_ms)` in tokens/sec, skipping turns where
  that denominator is < 50 ms.
- **`share`** values in `models` sum to 1.0 ± 1e-9; the largest bucket absorbs the rounding.
- **Heatmap levels** are quintiles of the non-zero token distribution, so a light week still
  shows contrast. Level 0 is reserved for exactly zero.
- **Projected monthly** = mean daily cost over the trailing 14 days × 30, `0.0` with fewer
  than 3 active days.
- **Failed and aborted turns** are counted in `turns` and `error_rate` but contribute their
  actual (partial) tokens.
- Empty range returns a fully-populated document of zeros — never `null`, never an error, so
  F11.12 has something to render.

## 4. Performance

`getStats` for `all` over 100k turns must return in < 150 ms. Aggregate in SQL
(`GROUP BY`), pull raw rows only for percentile work, and cache the result keyed by
`(range, tz, max(turns.started_at))` — the harness emits `stats_invalidated` and the cache
key changes naturally.

## 5. Done when

- `cargo test -p form-core stats::` covers: gap filling across a DST boundary, streak edge
  cases (today empty, single day, broken by one day), percentile correctness against a
  hand-computed sample, share summing to 1.0, empty-range zero document, and the 150 ms
  budget over a synthetic 100k-turn database.
