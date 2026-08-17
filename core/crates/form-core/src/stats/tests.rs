//! Tests for spec 03 §5. The fixture below mirrors the parts of W1's schema (spec 01 §2)
//! that the engine reads; when the real store lands these `CREATE TABLE`s must match it,
//! and the queries here are the tripwire if they ever drift.

use std::time::Instant;

use chrono::{Duration, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::Connection;

use super::*;

const SCHEMA: &str = "
CREATE TABLE sessions (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, title_is_custom INTEGER NOT NULL DEFAULT 0,
  group_id TEXT, idx INTEGER NOT NULL, workspace_root TEXT,
  provider_id TEXT NOT NULL, model_id TEXT NOT NULL, thinking_level TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'idle', archived INTEGER NOT NULL DEFAULT 0,
  pinned INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);

CREATE TABLE entries (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, seq INTEGER NOT NULL, parent_id TEXT,
  kind TEXT NOT NULL, role TEXT, timestamp INTEGER NOT NULL, payload TEXT NOT NULL);
CREATE UNIQUE INDEX entries_seq ON entries(session_id, seq);

CREATE TABLE turns (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, run_id TEXT NOT NULL,
  provider_id TEXT NOT NULL, model_id TEXT NOT NULL, thinking_level TEXT NOT NULL,
  started_at INTEGER NOT NULL, ended_at INTEGER NOT NULL,
  ttft_ms INTEGER, duration_ms INTEGER NOT NULL,
  input INTEGER NOT NULL, output INTEGER NOT NULL,
  cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
  reasoning INTEGER, total_tokens INTEGER NOT NULL,
  cost_total REAL NOT NULL, outcome TEXT NOT NULL);
CREATE INDEX turns_started ON turns(started_at);

CREATE TABLE tool_invocations (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, turn_id TEXT NOT NULL,
  tool_name TEXT NOT NULL, started_at INTEGER NOT NULL,
  duration_ms INTEGER NOT NULL, is_error INTEGER NOT NULL);
";

const NY: Tz = Tz::America__New_York;

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(SCHEMA).expect("schema");
    conn
}

/// A turn with plausible defaults; tests override only what they are asserting on.
#[derive(Clone)]
struct T {
    session: String,
    provider: String,
    model: String,
    started_at: i64,
    ttft_ms: Option<i64>,
    duration_ms: i64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost: f64,
    outcome: String,
}

impl T {
    fn at(started_at: i64) -> Self {
        Self {
            session: "ses_1".into(),
            provider: "anthropic".into(),
            model: "claude-opus-5".into(),
            started_at,
            ttft_ms: Some(400),
            duration_ms: 4_400,
            input: 1_000,
            output: 500,
            cache_read: 800,
            cache_write: 200,
            cost: 0.25,
            outcome: "completed".into(),
        }
    }
    fn session(mut self, id: &str) -> Self {
        self.session = id.into();
        self
    }
    fn model(mut self, provider: &str, model: &str) -> Self {
        self.provider = provider.into();
        self.model = model.into();
        self
    }
    fn tokens(mut self, input: u64, output: u64) -> Self {
        self.input = input;
        self.output = output;
        self
    }
    fn cache(mut self, read: u64, write: u64) -> Self {
        self.cache_read = read;
        self.cache_write = write;
        self
    }
    fn timing(mut self, ttft_ms: Option<i64>, duration_ms: i64) -> Self {
        self.ttft_ms = ttft_ms;
        self.duration_ms = duration_ms;
        self
    }
    fn cost(mut self, cost: f64) -> Self {
        self.cost = cost;
        self
    }
    fn outcome(mut self, outcome: &str) -> Self {
        self.outcome = outcome.into();
        self
    }
    fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

fn insert(conn: &Connection, turns: &[T]) {
    let mut sessions: Vec<&str> = turns.iter().map(|t| t.session.as_str()).collect();
    sessions.sort_unstable();
    sessions.dedup();
    for (i, id) in sessions.iter().enumerate() {
        conn.execute(
            "INSERT OR IGNORE INTO sessions (id, title, idx, provider_id, model_id,
                                             thinking_level, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'anthropic', 'claude-opus-5', 'high', 'idle', 0, 0)",
            rusqlite::params![id, format!("Session {i}"), i as i64],
        )
        .expect("session");
    }
    // Tests insert in several passes; ids continue from what is already there.
    let seen: i64 = conn
        .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
        .expect("count");
    for (i, t) in turns.iter().enumerate() {
        let i = i + seen as usize;
        conn.execute(
            "INSERT INTO turns (id, session_id, run_id, provider_id, model_id, thinking_level,
                                started_at, ended_at, ttft_ms, duration_ms, input, output,
                                cache_read, cache_write, reasoning, total_tokens, cost_total,
                                outcome)
             VALUES (?1, ?2, 'run', ?3, ?4, 'high', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0,
                     ?13, ?14, ?15)",
            rusqlite::params![
                format!("turn_{i}"),
                t.session,
                t.provider,
                t.model,
                t.started_at,
                t.started_at + t.duration_ms,
                t.ttft_ms,
                t.duration_ms,
                t.input as i64,
                t.output as i64,
                t.cache_read as i64,
                t.cache_write as i64,
                t.total() as i64,
                t.cost,
                t.outcome,
            ],
        )
        .expect("turn");
    }
}

/// UTC millis for a local wall-clock time in `tz`, taking the earlier instant when the
/// clock repeats itself.
fn local(tz: Tz, y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
    tz.with_ymd_and_hms(y, m, d, h, min, 0)
        .earliest()
        .expect("local time exists")
        .timestamp_millis()
}

/// UTC millis for local noon `days` before today.
fn days_ago(tz: Tz, days: i64) -> i64 {
    let date = Utc::now().with_timezone(&tz).date_naive() - Duration::days(days);
    tz.from_local_datetime(&date.and_hms_opt(12, 0, 0).expect("noon"))
        .earliest()
        .expect("noon exists")
        .timestamp_millis()
}

// ------------------------------------------------------------- empty range

#[test]
fn empty_range_returns_a_populated_zero_document() {
    let conn = db();
    for range in [StatsRange::D7, StatsRange::D30, StatsRange::All] {
        let s = compute(&conn, range, "America/New_York").expect("compute");
        assert_eq!(s.hourly.len(), 24);
        assert_eq!(s.weekday_hour.len(), 7);
        assert!(s.weekday_hour.iter().all(|row| row.len() == 24));
        assert_eq!(s.headline, Headline::default());
        assert_eq!(s.cost.total, 0.0);
        assert_eq!(s.cost.projected_monthly, 0.0);
        assert_eq!(s.cache.hit_ratio, 0.0);
        assert!(s.models.is_empty() && s.providers.is_empty() && s.latency.is_empty());
        assert_eq!(s.daily.len(), s.heatmap.len());
        assert!(s.daily.iter().all(|d| d.total_tokens == 0));
        assert!(s.heatmap.iter().all(|c| c.level == 0));
    }
    // The period still has its full axis, so the chart renders an empty week not nothing.
    assert_eq!(
        compute(&conn, StatsRange::D7, "UTC").unwrap().daily.len(),
        7
    );
    assert_eq!(
        compute(&conn, StatsRange::D30, "UTC").unwrap().daily.len(),
        30
    );
}

#[test]
fn a_store_without_a_turns_table_still_answers() {
    let conn = Connection::open_in_memory().unwrap();
    let s = compute(&conn, StatsRange::D30, "America/New_York").expect("compute");
    assert_eq!(s.hourly.len(), 24);
    assert_eq!(s.weekday_hour.len(), 7);
}

// ------------------------------------------------------------------- dates

#[test]
fn daily_gap_fills_across_a_spring_forward_boundary() {
    // 2025-03-09 in New York is 23 hours long and 02:00 local never happens.
    let conn = db();
    insert(
        &conn,
        &[
            T::at(local(NY, 2025, 3, 8, 12, 0)),
            T::at(local(NY, 2025, 3, 9, 23, 30)),
            T::at(local(NY, 2025, 3, 11, 1, 0)),
        ],
    );

    let s = compute(&conn, StatsRange::All, "America/New_York").expect("compute");
    let dates: Vec<&str> = s.daily.iter().map(|d| d.date.as_str()).collect();
    assert_eq!(
        &dates[..4],
        &["2025-03-08", "2025-03-09", "2025-03-10", "2025-03-11"]
    );

    // 2025-03-10 had no turns and must be present with zeros rather than skipped.
    assert_eq!(
        s.daily[1].turns, 1,
        "late-evening turn stays on the short day"
    );
    assert_eq!(s.daily[2].turns, 0);
    assert_eq!(s.daily[2].total_tokens, 0);
    assert_no_gaps(&s.daily);
}

#[test]
fn daily_gap_fills_across_a_fall_back_boundary() {
    // 2025-11-02 in New York is 25 hours long and 01:30 local happens twice.
    let conn = db();
    insert(
        &conn,
        &[
            T::at(local(NY, 2025, 11, 1, 12, 0)),
            T::at(local(NY, 2025, 11, 2, 1, 30)),
            T::at(local(NY, 2025, 11, 2, 1, 30) + 3_600_000), // the repeat of 01:30
            T::at(local(NY, 2025, 11, 3, 9, 0)),
        ],
    );

    let s = compute(&conn, StatsRange::All, "America/New_York").expect("compute");
    let dates: Vec<&str> = s.daily.iter().map(|d| d.date.as_str()).collect();
    assert_eq!(&dates[..3], &["2025-11-01", "2025-11-02", "2025-11-03"]);
    assert_eq!(s.daily[1].turns, 2, "both 01:30s belong to the long day");
    assert_no_gaps(&s.daily);
}

fn assert_no_gaps(daily: &[DailyBucket]) {
    for pair in daily.windows(2) {
        let a = chrono::NaiveDate::parse_from_str(&pair[0].date, "%Y-%m-%d").unwrap();
        let b = chrono::NaiveDate::parse_from_str(&pair[1].date, "%Y-%m-%d").unwrap();
        assert_eq!(b - a, Duration::days(1), "gap between {a} and {b}");
    }
}

#[test]
fn hours_and_weekdays_bucket_in_the_callers_timezone() {
    let conn = db();
    // 2025-06-16 was a Monday. 21:00 in New York is 01:00 the next day in UTC.
    insert(
        &conn,
        &[T::at(local(NY, 2025, 6, 16, 21, 0))
            .tokens(100, 0)
            .cache(0, 0)],
    );

    let ny = compute(&conn, StatsRange::All, "America/New_York").expect("compute");
    assert_eq!(ny.hourly[21].turns, 1);
    assert_eq!(ny.weekday_hour[0][21], 100, "Monday 21:00 local");
    assert_eq!(ny.headline.peak_hour, 21);

    let utc = compute(&conn, StatsRange::All, "UTC").expect("compute");
    assert_eq!(utc.hourly[1].turns, 1);
    assert_eq!(utc.weekday_hour[1][1], 100, "Tuesday 01:00 UTC");

    // An unknown zone must not fail the dashboard; it falls back to UTC.
    let bogus = compute(&conn, StatsRange::All, "Mars/Olympus").expect("compute");
    assert_eq!(bogus.hourly[1].turns, 1);
}

// ----------------------------------------------------------------- streaks

#[test]
fn streak_edge_cases() {
    // Single day.
    assert_eq!(streaks(&[100], 100), (1, 1));
    // Today empty but yesterday active — the streak has not broken yet.
    assert_eq!(streaks(&[98, 99], 100), (2, 2));
    // Two empty days breaks it, while the record stands.
    assert_eq!(streaks(&[95, 96, 97], 100), (0, 3));
    // Broken by exactly one day in the middle.
    assert_eq!(streaks(&[90, 91, 93, 94, 95], 95), (3, 3));
    // No history at all.
    assert_eq!(streaks(&[], 100), (0, 0));
    // A long run ending today.
    let run: Vec<i64> = (80..=100).collect();
    assert_eq!(streaks(&run, 100), (21, 21));
}

#[test]
fn streaks_are_counted_over_all_history_not_just_the_period() {
    let conn = db();
    let mut turns = Vec::new();
    for day in 0..20 {
        turns.push(T::at(days_ago(NY, day)));
    }
    insert(&conn, &turns);

    let week = compute(&conn, StatsRange::D7, "America/New_York").expect("compute");
    assert_eq!(
        week.headline.active_days, 7,
        "active days are period-scoped"
    );
    assert_eq!(week.headline.current_streak, 20);
    assert_eq!(week.headline.longest_streak, 20);
}

// ------------------------------------------------------------- percentiles

#[test]
fn nearest_rank_matches_a_hand_computed_sample() {
    let sample: Vec<u64> = (1..=10).map(|n| n * 10).collect(); // 10, 20 … 100
                                                               // rank = ceil(p·N): p50 → 5th → 50, p90 → 9th → 90, p99 → 10th → 100.
    assert_eq!(nearest_rank_u64(&sample, 0.50), 50);
    assert_eq!(nearest_rank_u64(&sample, 0.90), 90);
    assert_eq!(nearest_rank_u64(&sample, 0.99), 100);
    // Not an interpolating percentile: every answer is an observed value.
    let odd = [3u64, 7, 11];
    assert_eq!(nearest_rank_u64(&odd, 0.50), 7);
    assert_eq!(nearest_rank_u64(&odd, 0.34), 7);
    assert_eq!(nearest_rank_u64(&odd, 0.33), 3);
    assert_eq!(nearest_rank_u64(&[], 0.5), 0);
    assert_eq!(nearest_rank_u64(&[42], 0.01), 42);
}

#[test]
fn latency_percentiles_come_from_raw_turn_rows() {
    let conn = db();
    let turns: Vec<T> = (1..=10)
        .map(|n| {
            // ttft 100…1000 ms; the post-ttft window is a flat 1 s so tok/s == output.
            T::at(days_ago(NY, 3) + n * 1_000)
                .timing(Some(n * 100), n * 100 + 1_000)
                .tokens(0, (n * 10) as u64)
                .cache(0, 0)
        })
        .collect();
    insert(&conn, &turns);

    let s = compute(&conn, StatsRange::All, "America/New_York").expect("compute");
    let l = &s.latency[0];
    assert_eq!(l.samples, 10);
    assert_eq!((l.ttft_p50, l.ttft_p90, l.ttft_p99), (500, 900, 1_000));
    assert_eq!((l.tps_p50, l.tps_p90, l.tps_p99), (50.0, 90.0, 100.0));
    assert_eq!(s.models[0].avg_ttft_ms, 550);
    assert!((s.models[0].avg_output_tps - 55.0).abs() < 1e-9);

    let counted: u64 = l.histogram.iter().map(|b| b.count).sum();
    assert_eq!(counted, 10);
    assert_eq!(l.histogram[0].count, 2, "100 and 200 ms fall under 250");
    assert_eq!(l.histogram.last().unwrap().upper_ms, None);
}

#[test]
fn throughput_skips_sub_50ms_denominators() {
    assert_eq!(throughput(500, 1_050, Some(50)), Some(500.0));
    assert_eq!(throughput(500, 1_000, Some(960)), None);
    assert_eq!(throughput(500, 49, Some(0)), None);
    assert_eq!(throughput(500, 1_000, None), Some(500.0));

    let conn = db();
    insert(
        &conn,
        &[
            T::at(days_ago(NY, 1))
                .timing(Some(100), 1_100)
                .tokens(0, 1_000),
            // Same tokens, but the whole turn is first-token latency: no usable rate.
            T::at(days_ago(NY, 1) + 1)
                .timing(Some(1_070), 1_100)
                .tokens(0, 1_000),
        ],
    );
    let s = compute(&conn, StatsRange::All, "America/New_York").expect("compute");
    assert!((s.models[0].avg_output_tps - 1_000.0).abs() < 1e-9);
    assert_eq!(
        s.models[0].turns, 2,
        "the turn still counts, only its rate is dropped"
    );
}

// ------------------------------------------------------------------ shares

#[test]
fn model_shares_sum_to_one() {
    for weights in [
        vec![1u64, 1, 1],
        vec![7, 3],
        vec![1_000_003, 17, 5, 5, 5, 5, 5],
        vec![1],
    ] {
        let shares = normalize_shares(&weights);
        let sum: f64 = shares.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "{weights:?} summed to {sum}");
    }
    assert!(normalize_shares(&[]).is_empty());
}

#[test]
fn shares_across_models_sum_to_one_end_to_end() {
    let conn = db();
    insert(
        &conn,
        &[
            T::at(days_ago(NY, 2))
                .model("anthropic", "claude-opus-5")
                .tokens(1, 0)
                .cache(0, 0),
            T::at(days_ago(NY, 2) + 1)
                .model("anthropic", "claude-sonnet-5")
                .tokens(1, 0)
                .cache(0, 0),
            T::at(days_ago(NY, 2) + 2)
                .model("openai", "gpt-5.2")
                .tokens(1, 0)
                .cache(0, 0),
        ],
    );
    let s = compute(&conn, StatsRange::D7, "America/New_York").expect("compute");
    assert_eq!(s.models.len(), 3);
    let sum: f64 = s.models.iter().map(|m| m.share).sum();
    assert!((sum - 1.0).abs() < 1e-9, "shares summed to {sum}");
    let providers: f64 = s.providers.iter().map(|p| p.share).sum();
    assert!((providers - 1.0).abs() < 1e-9);
    assert_eq!(s.providers.len(), 2);
    // Names come from the catalog, not from the id.
    assert_eq!(s.models[0].display_name, "Opus 5");
    assert!(s.providers.iter().any(|p| p.display_name == "Anthropic"));
}

#[test]
fn a_period_of_only_failed_turns_still_produces_shares() {
    let conn = db();
    insert(
        &conn,
        &[
            T::at(days_ago(NY, 1))
                .outcome("failed")
                .tokens(0, 0)
                .cache(0, 0)
                .cost(0.0),
            T::at(days_ago(NY, 1) + 1)
                .outcome("aborted")
                .tokens(0, 0)
                .cache(0, 0)
                .cost(0.0),
        ],
    );
    let s = compute(&conn, StatsRange::D7, "America/New_York").expect("compute");
    assert_eq!(s.headline.turns, 2, "failed turns are counted");
    assert!((s.models[0].share - 1.0).abs() < 1e-9);
    assert!((s.models[0].error_rate - 1.0).abs() < 1e-9);
}

// ----------------------------------------------------------------- heatmap

#[test]
fn heatmap_levels_are_quantiles_of_the_non_zero_distribution() {
    // Zero is its own level; the rest spread across 1..=4 even when all values are small.
    let levels = heat_levels(&[0, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(levels[0], 0);
    assert_eq!(*levels.iter().max().unwrap(), 4);
    assert_eq!(*levels[1..].iter().min().unwrap(), 1);
    assert!(levels.windows(2).skip(1).all(|w| w[0] <= w[1]), "monotone");

    // A light week: the only active day reads at full intensity, not as background.
    assert_eq!(heat_levels(&[0, 0, 12, 0]), vec![0, 0, 4, 0]);
    assert!(heat_levels(&[0, 0, 0]).iter().all(|&l| l == 0));
}

#[test]
fn heatmap_mirrors_the_daily_series() {
    let conn = db();
    insert(
        &conn,
        &[
            T::at(days_ago(NY, 4)).tokens(10, 0).cache(0, 0),
            T::at(days_ago(NY, 1))
                .session("ses_2")
                .tokens(9_000, 0)
                .cache(0, 0),
            T::at(days_ago(NY, 1) + 1)
                .session("ses_3")
                .tokens(1_000, 0)
                .cache(0, 0),
        ],
    );
    let s = compute(&conn, StatsRange::D7, "America/New_York").expect("compute");
    assert_eq!(s.heatmap.len(), 7);
    let busiest = s
        .heatmap
        .iter()
        .find(|c| c.tokens == 10_000)
        .expect("busy day");
    assert_eq!(busiest.level, 4);
    assert_eq!(busiest.sessions, 2);
    let quiet = s
        .heatmap
        .iter()
        .find(|c| c.tokens == 10)
        .expect("quiet day");
    assert!((1..4).contains(&quiet.level), "active but not the busiest");
    assert!(s
        .heatmap
        .iter()
        .filter(|c| c.tokens == 0)
        .all(|c| c.level == 0));
}

// -------------------------------------------------------------------- cost

#[test]
fn projected_monthly_needs_three_active_days() {
    let conn = db();
    insert(
        &conn,
        &[
            T::at(days_ago(NY, 1)).cost(7.0),
            T::at(days_ago(NY, 2)).cost(7.0),
        ],
    );
    let two = compute(&conn, StatsRange::D30, "America/New_York").expect("compute");
    assert_eq!(
        two.cost.projected_monthly, 0.0,
        "two days is not a run rate"
    );

    insert(&conn, &[T::at(days_ago(NY, 3)).cost(7.0)]);
    let three = compute(&conn, StatsRange::D30, "America/New_York").expect("compute");
    // 21.00 over a 14-day basis, ×30.
    assert!((three.cost.projected_monthly - 45.0).abs() < 1e-9);
    // The basis is the trailing fortnight even when the selected period is shorter.
    let week = compute(&conn, StatsRange::D7, "America/New_York").expect("compute");
    assert!((week.cost.projected_monthly - 45.0).abs() < 1e-9);
}

#[test]
fn cost_series_carries_its_own_cumulative() {
    let conn = db();
    insert(
        &conn,
        &[
            T::at(days_ago(NY, 2)).cost(1.5),
            T::at(days_ago(NY, 0)).model("openai", "gpt-5.2").cost(2.5),
        ],
    );
    let s = compute(&conn, StatsRange::D7, "America/New_York").expect("compute");
    assert!((s.cost.total - 4.0).abs() < 1e-9);
    assert!((s.cost.by_day.last().unwrap().cumulative - 4.0).abs() < 1e-9);
    assert!(s
        .cost
        .by_day
        .windows(2)
        .all(|w| w[0].cumulative <= w[1].cumulative));
    assert_eq!(s.cost.by_provider.len(), 2);
    assert_eq!(s.cost.by_provider[0].0, "openai", "ranked by spend");
    assert!((s.cost.by_model[0].1 - 2.5).abs() < 1e-9);
    assert!((s.headline.total_cost - 4.0).abs() < 1e-9);
}

#[test]
fn cache_savings_price_reads_against_fresh_input() {
    let conn = db();
    insert(&conn, &[T::at(days_ago(NY, 1)).cache(1_000_000, 500)]);
    let s = compute(&conn, StatsRange::D7, "America/New_York").expect("compute");
    assert_eq!(s.cache.read, 1_000_000);
    assert_eq!(s.cache.write, 500);
    // Opus 5 is $5.00/M input against $0.50/M cache read.
    assert!((s.cache.estimated_savings - 4.5).abs() < 1e-9);
    assert!((s.cache.hit_ratio - 1_000_000.0 / 1_000_500.0).abs() < 1e-9);
    assert_eq!(s.cache.daily.len(), s.daily.len());
}

// ------------------------------------------------------- tools & sessions

#[test]
fn tool_usage_and_session_leaderboards() {
    let conn = db();
    insert(
        &conn,
        &[
            T::at(days_ago(NY, 1))
                .session("ses_a")
                .tokens(5_000, 0)
                .cache(0, 0),
            T::at(days_ago(NY, 1) + 1)
                .session("ses_b")
                .tokens(100, 0)
                .cache(0, 0)
                .timing(Some(10), 90_000),
        ],
    );
    for (i, (tool, err)) in [("read", 0), ("read", 0), ("read", 1), ("bash", 0)]
        .iter()
        .enumerate()
    {
        conn.execute(
            "INSERT INTO tool_invocations (id, session_id, turn_id, tool_name, started_at,
                                           duration_ms, is_error)
             VALUES (?1, 'ses_a', 'turn_0', ?2, ?3, ?4, ?5)",
            rusqlite::params![format!("ti_{i}"), tool, days_ago(NY, 1), 120_i64, err],
        )
        .unwrap();
    }

    let s = compute(&conn, StatsRange::D7, "America/New_York").expect("compute");
    assert_eq!(s.tools[0].name, "read");
    assert_eq!(s.tools[0].invocations, 3);
    assert_eq!(s.tools[0].errors, 1);
    assert!((s.tools[0].success_rate - 2.0 / 3.0).abs() < 1e-9);
    assert_eq!(s.tools[0].mean_duration_ms, 120);

    assert_eq!(s.sessions_top.by_tokens[0].session_id, "ses_a");
    assert_eq!(s.sessions_top.by_duration[0].session_id, "ses_b");
    assert_eq!(s.sessions_top.by_tokens[0].title, "Session 0");
    assert_eq!(s.headline.sessions, 2);
    assert_eq!(s.headline.avg_session_tokens, 2_550);
}

#[test]
fn messages_come_from_the_transcript_log() {
    let conn = db();
    insert(&conn, &[T::at(days_ago(NY, 1))]);
    for (i, kind) in ["message", "message", "message", "model_change"]
        .iter()
        .enumerate()
    {
        conn.execute(
            "INSERT INTO entries (id, session_id, seq, kind, role, timestamp, payload)
             VALUES (?1, 'ses_1', ?2, ?3, 'user', ?4, '{}')",
            rusqlite::params![format!("ent_{i}"), i as i64, kind, days_ago(NY, 1)],
        )
        .unwrap();
    }
    let s = compute(&conn, StatsRange::D7, "America/New_York").expect("compute");
    assert_eq!(s.headline.messages, 3, "only kind='message' counts");
    assert_eq!(
        s.daily.last().unwrap().messages + s.daily[s.daily.len() - 2].messages,
        3
    );
}

// ------------------------------------------------------------------- cache

#[test]
fn repeated_queries_are_served_from_the_memo() {
    let conn = db();
    // A watermark no other test shares, so the process-wide memo cannot cross-talk.
    insert(&conn, &[T::at(local(NY, 2021, 4, 5, 6, 7))]);
    let first = compute_cached(&conn, StatsRange::All, "America/New_York").expect("compute");
    let second = compute_cached(&conn, StatsRange::All, "America/New_York").expect("compute");
    assert_eq!(
        first.generated_at, second.generated_at,
        "memoized, not recomputed"
    );
    assert_eq!(first.headline, second.headline);

    // A new turn moves max(started_at) and the key with it.
    insert(&conn, &[T::at(local(NY, 2021, 4, 6, 6, 7))]);
    let third = compute_cached(&conn, StatsRange::All, "America/New_York").expect("compute");
    assert_eq!(third.headline.turns, 2);
}

#[test]
fn the_query_entry_point_reads_the_stores_database() {
    let dir = std::env::temp_dir().join(format!("form-stats-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // No database yet — first launch before the store has ever opened.
    let blank = compute_at(&dir, StatsRange::D7, "America/New_York").expect("compute");
    assert_eq!(blank.daily.len(), 7);
    assert_eq!(blank.headline.turns, 0);

    std::fs::create_dir_all(&dir).unwrap();
    {
        let conn = Connection::open(dir.join("form.sqlite")).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        insert(&conn, &[T::at(days_ago(NY, 1)).tokens(11, 22)]);
    }
    let s = compute_at(&dir, StatsRange::D7, "America/New_York").expect("compute");
    assert_eq!(s.headline.turns, 1);
    assert_eq!(s.headline.input, 11);
    assert_eq!(s.headline.output, 22);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_document_round_trips_through_json() {
    let conn = db();
    insert(&conn, &[T::at(days_ago(NY, 1))]);
    let s = compute(&conn, StatsRange::D30, "America/New_York").expect("compute");
    let json = serde_json::to_string(&s).expect("encode");
    // The boundary is camelCase and nothing is `null` (spec 00 §1).
    assert!(json.contains("\"weekdayHour\""));
    assert!(json.contains("\"sessionsTop\""));
    assert!(json.contains("\"projectedMonthly\""));
    assert!(!json.contains("null"));
    let back: UsageStats = serde_json::from_str(&json).expect("decode");
    assert_eq!(back.headline, s.headline);
    assert_eq!(back.daily.len(), s.daily.len());
}

// -------------------------------------------------------------- performance

/// Spec 03 §4: `all` over 100k turns in under 150 ms. The budget is a release-mode
/// promise — `cargo test` builds the bundled SQLite at `-O0`, where the same work costs
/// several times more, so the assertion is loosened there and the measurement printed.
#[test]
fn hundred_thousand_turns_stay_within_budget() {
    let conn = db();
    seed_synthetic(&conn, 100_000);

    // Warm the page cache; the budget describes a running app, not a cold open.
    let _ = compute(&conn, StatsRange::All, "America/New_York").expect("compute");

    let started = Instant::now();
    let s = compute(&conn, StatsRange::All, "America/New_York").expect("compute");
    let elapsed = started.elapsed();
    println!("getStats(all) over 100k turns: {elapsed:?}");

    assert_eq!(s.headline.turns, 100_000);
    assert!(s.daily.len() > 300);
    assert_eq!(s.hourly.len(), 24);
    assert!(!s.models.is_empty());

    let budget = if cfg!(debug_assertions) { 1_200 } else { 150 };
    assert!(
        elapsed.as_millis() < budget,
        "getStats took {elapsed:?}, budget {budget} ms"
    );

    // And the memo makes the repeat free.
    let started = Instant::now();
    let _ = compute_cached(&conn, StatsRange::All, "America/New_York").expect("compute");
    let _ = compute_cached(&conn, StatsRange::All, "America/New_York").expect("compute");
    assert!(started.elapsed().as_millis() < budget);
}

fn seed_synthetic(conn: &Connection, turns: usize) {
    const MODELS: [(&str, &str); 4] = [
        ("anthropic", "claude-opus-5"),
        ("anthropic", "claude-sonnet-5"),
        ("openai", "gpt-5.2"),
        ("openai", "gpt-5.2-mini"),
    ];
    const TOOLS: [&str; 5] = ["read", "edit", "bash", "grep", "write"];
    let sessions = 800usize;
    let day_zero = days_ago(NY, 400);

    conn.execute_batch("PRAGMA journal_mode = OFF; BEGIN")
        .unwrap();
    {
        let mut stmt = conn
            .prepare(
                "INSERT INTO sessions (id, title, idx, provider_id, model_id, thinking_level,
                                       status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'anthropic', 'claude-opus-5', 'high', 'idle', 0, 0)",
            )
            .unwrap();
        for i in 0..sessions {
            stmt.execute(rusqlite::params![
                format!("ses_{i}"),
                format!("Synthetic session {i}"),
                i as i64
            ])
            .unwrap();
        }

        let mut turn_stmt = conn
            .prepare(
                "INSERT INTO turns (id, session_id, run_id, provider_id, model_id,
                                    thinking_level, started_at, ended_at, ttft_ms, duration_ms,
                                    input, output, cache_read, cache_write, reasoning,
                                    total_tokens, cost_total, outcome)
                 VALUES (?1, ?2, 'run', ?3, ?4, 'high', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                         ?13, ?14, ?15, ?16)",
            )
            .unwrap();
        let mut entry_stmt = conn
            .prepare(
                "INSERT INTO entries (id, session_id, seq, kind, role, timestamp, payload)
                 VALUES (?1, ?2, ?3, 'message', 'assistant', ?4, ?5)",
            )
            .unwrap();
        let mut tool_stmt = conn
            .prepare(
                "INSERT INTO tool_invocations (id, session_id, turn_id, tool_name, started_at,
                                               duration_ms, is_error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .unwrap();
        let payload = "x".repeat(512);

        // A cheap deterministic spread — no RNG dependency, but every column varies.
        for i in 0..turns {
            let n = i as i64;
            let started = day_zero + n * (400 * 86_400_000 / turns as i64) + (n % 997) * 37;
            let (provider, model) = MODELS[i % MODELS.len()];
            let input = 400 + (n % 3_000);
            let output = 120 + (n % 900);
            let cache_read = (n % 7) * 500;
            let cache_write = (n % 5) * 90;
            let outcome = match i % 37 {
                0 => "failed",
                17 => "aborted",
                _ => "completed",
            };
            turn_stmt
                .execute(rusqlite::params![
                    format!("turn_{i}"),
                    format!("ses_{}", i % sessions),
                    provider,
                    model,
                    started,
                    started + 3_000 + (n % 20_000),
                    200 + (n % 5_000),
                    3_000 + (n % 20_000),
                    input,
                    output,
                    cache_read,
                    cache_write,
                    n % 300,
                    input + output + cache_read + cache_write,
                    0.004 * (n % 50) as f64,
                    outcome,
                ])
                .unwrap();

            for m in 0..2 {
                entry_stmt
                    .execute(rusqlite::params![
                        format!("ent_{i}_{m}"),
                        format!("ses_{}", i % sessions),
                        n * 2 + m,
                        started + m,
                        payload
                    ])
                    .unwrap();
            }
            if i % 3 == 0 {
                tool_stmt
                    .execute(rusqlite::params![
                        format!("ti_{i}"),
                        format!("ses_{}", i % sessions),
                        format!("turn_{i}"),
                        TOOLS[i % TOOLS.len()],
                        started + 500,
                        50 + (n % 4_000),
                        i64::from(i % 23 == 0),
                    ])
                    .unwrap();
            }
        }
    }
    conn.execute_batch("COMMIT").unwrap();
}

#[test]
#[ignore = "measurement aid, not an assertion"]
fn perf_breakdown() {
    let conn = db();
    seed_synthetic(&conn, 100_000);
    let now = now_ms();
    let offsets = Offsets::build(NY, days_ago(NY, 400), now + DAY_MS);
    let today = offsets.day(now);
    let w = Window {
        from_ms: offsets.day_start_utc(today - 400).max(0),
        to_ms: offsets.day_start_utc(today + 1),
        offsets,
    };
    macro_rules! t {
        ($label:expr, $e:expr) => {{
            let s = Instant::now();
            let out = $e;
            println!("{:>22}: {:?}", $label, s.elapsed());
            out
        }};
    }
    let _ = t!("daily", query::daily(&conn, &w).unwrap());
    let _ = t!("messages", query::messages_by_day(&conn, &w).unwrap());
    let _ = t!("weekday_hour", query::weekday_hour(&conn, &w).unwrap());
    let models = t!("models", query::models(&conn, &w).unwrap());
    let _ = t!("latency_samples", query::latency_samples(&conn, &w, &models).unwrap());
    let _ = t!("tools", query::tools(&conn, &w).unwrap());
    let _ = t!("sessions", query::sessions(&conn, &w).unwrap());
    let _ = t!("active_days", query::active_days_all_time(&conn, &w.offsets).unwrap());
    let _ = t!("trailing", query::trailing_cost(&conn, &w.offsets, 0).unwrap());

    conn.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA cache_size = -65536;").unwrap();
    println!("-- with temp_store=MEMORY, 64MB cache --");
    let _ = t!("daily", query::daily(&conn, &w).unwrap());
    let _ = t!("messages", query::messages_by_day(&conn, &w).unwrap());
    let _ = t!("weekday_hour", query::weekday_hour(&conn, &w).unwrap());
    let models = t!("models", query::models(&conn, &w).unwrap());
    let _ = t!("latency_samples", query::latency_samples(&conn, &w, &models).unwrap());
    let _ = t!("sessions", query::sessions(&conn, &w).unwrap());

    println!("-- merged cube --");
    let day = w.offsets.sql_day("started_at");
    let hour = w.offsets.sql_hour("started_at");
    let cube = format!(
        "SELECT {day} AS d, {hour} AS h, provider_id, model_id, COUNT(*), \
                SUM(CASE WHEN outcome = 'completed' THEN 0 ELSE 1 END), \
                SUM(input), SUM(output), SUM(cache_read), SUM(cache_write), \
                COALESCE(SUM(reasoning),0), SUM(total_tokens), SUM(cost_total), SUM(duration_ms) \
         FROM turns WHERE started_at >= ?1 AND started_at < ?2 GROUP BY d, h, provider_id, model_id"
    );
    let s = Instant::now();
    let mut stmt = conn.prepare(&cube).unwrap();
    let n = stmt
        .query_map([w.from_ms, w.to_ms], |r| {
            let _: i64 = r.get(0)?;
            let _: i64 = r.get(1)?;
            let _: String = r.get(2)?;
            let _: String = r.get(3)?;
            let _: i64 = r.get(11)?;
            Ok(())
        })
        .unwrap()
        .count();
    println!("{:>22}: {:?} ({n} rows)", "cube", s.elapsed());

    let sess = format!(
        "SELECT t.session_id, {day} AS d, SUM(t.total_tokens), SUM(t.duration_ms), COUNT(*) \
         FROM turns t WHERE t.started_at >= ?1 AND t.started_at < ?2 GROUP BY t.session_id, d"
    );
    let s = Instant::now();
    let mut stmt = conn.prepare(&sess).unwrap();
    let n = stmt
        .query_map([w.from_ms, w.to_ms], |r| {
            let _: String = r.get(0)?;
            let _: i64 = r.get(1)?;
            Ok(())
        })
        .unwrap()
        .count();
    println!("{:>22}: {:?} ({n} rows)", "session×day", s.elapsed());

    println!("-- single raw pass over turns --");
    let raw = format!(
        "SELECT started_at, session_id, provider_id, model_id, ttft_ms, duration_ms, \
                input, output, cache_read, cache_write, COALESCE(reasoning,0), \
                total_tokens, cost_total, outcome \
         FROM turns WHERE started_at >= ?1 AND started_at < ?2"
    );
    let s = Instant::now();
    let mut stmt = conn.prepare(&raw).unwrap();
    let mut rows = stmt.query([w.from_ms, w.to_ms]).unwrap();
    let mut days: std::collections::HashMap<i64, [u64; 8]> = std::collections::HashMap::new();
    let mut sess: std::collections::HashMap<String, [u64; 3]> = std::collections::HashMap::new();
    let mut mods: std::collections::HashMap<(String, String), [u64; 5]> = std::collections::HashMap::new();
    let mut n = 0u64;
    while let Some(r) = rows.next().unwrap() {
        let started: i64 = r.get(0).unwrap();
        let sid = r.get_ref(1).unwrap().as_str().unwrap();
        let provider = r.get_ref(2).unwrap().as_str().unwrap();
        let model = r.get_ref(3).unwrap().as_str().unwrap();
        let _ttft: Option<i64> = r.get(4).unwrap();
        let _dur: i64 = r.get(5).unwrap();
        let input: i64 = r.get(6).unwrap();
        let output: i64 = r.get(7).unwrap();
        let cr: i64 = r.get(8).unwrap();
        let cw: i64 = r.get(9).unwrap();
        let _re: i64 = r.get(10).unwrap();
        let total: i64 = r.get(11).unwrap();
        let _cost: f64 = r.get(12).unwrap();
        let outcome = r.get_ref(13).unwrap().as_str().unwrap();
        let d = w.offsets.day(started);
        let e = days.entry(d).or_default();
        e[0] += 1; e[1] += input as u64; e[2] += output as u64; e[3] += cr as u64;
        e[4] += cw as u64; e[5] += total as u64;
        if let Some(v) = sess.get_mut(sid) { v[0] += total as u64; v[2] += 1; }
        else { sess.insert(sid.to_string(), [total as u64, 0, 1]); }
        let key = (provider, model);
        if let Some(v) = mods.get_mut(&(key.0.to_string(), key.1.to_string())) { v[0] += 1; }
        else { mods.insert((key.0.to_string(), key.1.to_string()), [1, 0, 0, 0, 0]); }
        if outcome != "completed" { n += 1; }
    }
    println!("{:>22}: {:?} ({} days, {} sessions, {} models, {n} errors)",
             "raw + rust agg", s.elapsed(), days.len(), sess.len(), mods.len());
}
