//! Usage aggregation for the Home dashboard.
//!
//! **Owner: W3** (`docs/specs/03-stats.md`).
//!
//! One query, one document. [`compute`] runs a handful of `GROUP BY`s over `turns`,
//! `tool_invocations`, `entries` and `sessions`, buckets everything in the caller's IANA
//! timezone, and returns the whole [`UsageStats`] document the dashboard renders — no
//! per-chart round trips, no aggregation in Swift.
//!
//! Wiring: `core.rs` routes `getStats` to [`compute_at`] with the core's data directory.
//! Every entry point degrades to `UsageStats::empty` rather than failing — a store with
//! no database file, no `turns` table or no rows still renders F11.12's empty state.

mod calc;
mod query;
mod types;
mod tz;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use chrono_tz::Tz;
use rusqlite::Connection;

use crate::catalog::{self, Pricing};
use crate::error::Result;
use crate::protocol::{now_ms, StatsRange};

pub use types::*;

use calc::*;
use query::Window;
use tz::{day_to_date, Offsets, DAY_MS};

/// The `getStats` entry point: aggregate `{data_dir}/form.sqlite` for one period.
///
/// Opens its own `query_only` connection rather than borrowing the store's, so a
/// dashboard refresh never queues behind a streaming run on the store mutex — WAL makes
/// the concurrent reader free, and spec 00 §7 requires `query` to stay lock-light. A
/// database that does not exist yet is not an error; it is an empty dashboard.
pub fn compute_at(data_dir: &Path, range: StatsRange, tz: &str) -> Result<UsageStats> {
    let path = data_dir.join("form.sqlite");
    if !path.exists() {
        return Ok(zero_document(range, tz));
    }
    let conn = Connection::open(&path)?;
    conn.busy_timeout(std::time::Duration::from_millis(5_000))?;
    conn.pragma_update(None, "query_only", true)?;
    compute_cached(&conn, range, tz)
}

/// Cached entry point, keyed by `(database, range, tz, max(turns.started_at), today)`.
///
/// The harness emits `stats_invalidated` when a run ends and the watermark moves with it,
/// so the key expires naturally; `today` is in the key because a `d7` document silently
/// goes stale at local midnight even when nothing was recorded.
pub fn compute_cached(conn: &Connection, range: StatsRange, tz: &str) -> Result<UsageStats> {
    if !query::has_table(conn, "turns") {
        return Ok(zero_document(range, tz));
    }
    let watermark = query::turn_bounds(conn)?.map_or(0, |(_, hi)| hi);
    let zone = zone_or_utc(tz);
    let today = Offsets::build(zone, now_ms(), now_ms()).day(now_ms());
    let key = CacheKey {
        db: conn.path().unwrap_or_default().to_string(),
        range,
        tz: tz.to_string(),
        watermark,
        today,
    };

    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(entries) = cache.lock() {
        if let Some((_, hit)) = entries.iter().find(|(k, _)| *k == key) {
            return Ok(hit.clone());
        }
    }

    let stats = compute(conn, range, tz)?;
    if let Ok(mut entries) = cache.lock() {
        entries.retain(|(k, _)| k != &key);
        entries.push((key, stats.clone()));
        // Three ranges × a couple of zones is the whole realistic working set.
        if entries.len() > 8 {
            entries.remove(0);
        }
    }
    Ok(stats)
}

#[derive(PartialEq, Eq)]
struct CacheKey {
    db: String,
    range: StatsRange,
    tz: String,
    watermark: i64,
    today: i64,
}

static CACHE: OnceLock<Mutex<Vec<(CacheKey, UsageStats)>>> = OnceLock::new();

/// Aggregate the whole document. An unknown timezone falls back to UTC rather than
/// failing: the dashboard must always have something to render (F11.12).
pub fn compute(conn: &Connection, range: StatsRange, tz: &str) -> Result<UsageStats> {
    if !query::has_table(conn, "turns") {
        return Ok(zero_document(range, tz));
    }

    let zone = zone_or_utc(tz);
    let now = now_ms();
    let bounds = query::turn_bounds(conn)?;
    let earliest = bounds.map_or(now, |(lo, _)| lo).min(now).max(0);
    let offsets = Offsets::build(zone, earliest, now + DAY_MS);

    let today = offsets.day(now);
    let start_day = match range {
        StatsRange::D7 => today - 6,
        StatsRange::D30 => today - 29,
        StatsRange::All => bounds.map_or(today, |(lo, _)| offsets.day(lo)),
    }
    .min(today);

    let window = Window {
        from_ms: offsets.day_start_utc(start_day).max(0),
        to_ms: offsets.day_start_utc(today + 1),
        offsets,
    };

    let days = query::daily(conn, &window)?;
    let messages = if query::has_table(conn, "entries") {
        query::messages_by_day(conn, &window)?
    } else {
        HashMap::new()
    };
    let week_hours = query::weekday_hour(conn, &window)?;
    let model_rows = query::models(conn, &window)?;
    let samples = query::latency_samples(conn, &window, &model_rows)?;
    let tool_rows = if query::has_table(conn, "tool_invocations") {
        query::tools(conn, &window)?
    } else {
        Vec::new()
    };
    let session_rows = if query::has_table(conn, "sessions") {
        query::sessions(conn, &window)?
    } else {
        Vec::new()
    };
    let all_days = query::active_days_all_time(conn, &window.offsets)?;
    let trailing = query::trailing_cost(
        conn,
        &window.offsets,
        window.offsets.day_start_utc(today - 13).max(0),
    )?;

    let daily = build_daily(&days, &messages, start_day, today);
    let heatmap = build_heatmap(&daily);
    let (hourly, weekday_hour) = build_hours(&week_hours);
    let catalog = CatalogIndex::load();
    let (models, latency) = build_models(&model_rows, &samples, &catalog);
    let providers = build_providers(&model_rows, &catalog);
    let tools = build_tools(&tool_rows);
    let sessions_top = build_leaderboards(&session_rows);
    let cache = build_cache(&daily, &model_rows, &catalog);
    let cost = build_cost(&daily, &models, &providers, trailing);
    let headline = build_headline(
        &daily,
        &hourly,
        &models,
        &session_rows,
        &all_days,
        today,
        cost.total,
        days.iter().map(|d| d.reasoning).sum(),
    );

    Ok(UsageStats {
        range,
        generated_at: now,
        headline,
        daily,
        hourly,
        weekday_hour,
        heatmap,
        models,
        providers,
        tools,
        sessions_top,
        cache,
        cost,
        latency,
    })
}

fn zone_or_utc(tz: &str) -> Tz {
    Tz::from_str(tz).unwrap_or(Tz::UTC)
}

/// Zeros, but with the period's day axis filled in — a store with no `turns` table yet
/// should still draw an empty week rather than an empty chart frame.
fn zero_document(range: StatsRange, tz: &str) -> UsageStats {
    let now = now_ms();
    let offsets = Offsets::build(zone_or_utc(tz), now, now + DAY_MS);
    let today = offsets.day(now);
    let start_day = match range {
        StatsRange::D7 => today - 6,
        StatsRange::D30 => today - 29,
        StatsRange::All => today,
    };

    let daily = build_daily(&[], &HashMap::new(), start_day, today);
    UsageStats {
        heatmap: build_heatmap(&daily),
        cache: build_cache(&daily, &[], &CatalogIndex::load()),
        cost: build_cost(&daily, &[], &[], (0.0, 0)),
        daily,
        ..UsageStats::empty(range)
    }
}

// ------------------------------------------------------------------ assembly

/// Every local date in the period, zeros included — spec 03 §3. The chart must not
/// interpolate across a day nobody worked.
fn build_daily(
    days: &[query::DayRow],
    messages: &HashMap<i64, u64>,
    start_day: i64,
    today: i64,
) -> Vec<DailyBucket> {
    let by_day: HashMap<i64, &query::DayRow> = days.iter().map(|d| (d.day, d)).collect();
    (start_day..=today)
        .map(|day| {
            let row = by_day.get(&day);
            DailyBucket {
                date: day_to_date(day),
                sessions: row.map_or(0, |r| r.sessions),
                messages: messages.get(&day).copied().unwrap_or(0),
                turns: row.map_or(0, |r| r.turns),
                input: row.map_or(0, |r| r.input),
                output: row.map_or(0, |r| r.output),
                cache_read: row.map_or(0, |r| r.cache_read),
                cache_write: row.map_or(0, |r| r.cache_write),
                total_tokens: row.map_or(0, |r| r.total_tokens),
                cost: row.map_or(0.0, |r| r.cost),
                duration_ms: row.map_or(0, |r| r.duration_ms),
            }
        })
        .collect()
}

fn build_heatmap(daily: &[DailyBucket]) -> Vec<HeatmapCell> {
    let tokens: Vec<u64> = daily.iter().map(|d| d.total_tokens).collect();
    let levels = heat_levels(&tokens);
    daily
        .iter()
        .zip(levels)
        .map(|(d, level)| HeatmapCell {
            date: d.date.clone(),
            tokens: d.total_tokens,
            sessions: d.sessions,
            level,
        })
        .collect()
}

fn build_hours(rows: &[query::WeekHourRow]) -> (Vec<HourlyBucket>, Vec<Vec<u64>>) {
    let mut weekday_hour = vec![vec![0u64; 24]; 7];
    let mut hourly: Vec<HourlyBucket> = (0..24)
        .map(|hour| HourlyBucket {
            hour,
            ..Default::default()
        })
        .collect();
    for row in rows {
        weekday_hour[row.weekday][row.hour] += row.total_tokens;
        hourly[row.hour].total_tokens += row.total_tokens;
        hourly[row.hour].turns += row.turns;
    }
    (hourly, weekday_hour)
}

fn build_models(
    rows: &[query::ModelRow],
    samples: &[Vec<query::TurnSample>],
    catalog: &CatalogIndex,
) -> (Vec<ModelStat>, Vec<LatencyStat>) {
    let shares = share_of(
        rows.iter().map(|r| r.total_tokens),
        rows.iter().map(|r| r.turns),
    );
    let mut models = Vec::with_capacity(rows.len());
    let mut latency = Vec::with_capacity(rows.len());

    for (i, row) in rows.iter().enumerate() {
        let model = ModelRefLite {
            provider_id: row.provider_id.clone(),
            model_id: row.model_id.clone(),
        };
        let empty = Vec::new();
        let turn_samples = samples.get(i).unwrap_or(&empty);

        let mut ttfts: Vec<u64> = turn_samples
            .iter()
            .filter_map(|s| s.ttft_ms)
            .filter(|&t| t >= 0)
            .map(|t| t as u64)
            .collect();
        ttfts.sort_unstable();

        let mut tps: Vec<f64> = turn_samples
            .iter()
            .filter_map(|s| throughput(s.output, s.duration_ms, s.ttft_ms))
            .collect();
        tps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        models.push(ModelStat {
            model: model.clone(),
            display_name: catalog.model_name(&row.provider_id, &row.model_id),
            turns: row.turns,
            total_tokens: row.total_tokens,
            share: shares[i],
            cost: row.cost,
            avg_ttft_ms: mean_u64(&ttfts),
            avg_output_tps: mean_f64(&tps),
            error_rate: ratio(row.errors, row.turns),
        });

        latency.push(LatencyStat {
            model,
            ttft_p50: nearest_rank_u64(&ttfts, 0.50),
            ttft_p90: nearest_rank_u64(&ttfts, 0.90),
            ttft_p99: nearest_rank_u64(&ttfts, 0.99),
            tps_p50: nearest_rank_f64(&tps, 0.50),
            tps_p90: nearest_rank_f64(&tps, 0.90),
            tps_p99: nearest_rank_f64(&tps, 0.99),
            histogram: ttft_histogram(&ttfts),
            samples: ttfts.len() as u64,
        });
    }
    (models, latency)
}

fn build_providers(rows: &[query::ModelRow], catalog: &CatalogIndex) -> Vec<ProviderStat> {
    let mut order: Vec<String> = Vec::new();
    let mut totals: HashMap<&str, (u64, u64, f64)> = HashMap::new();
    for row in rows {
        let entry = totals.entry(row.provider_id.as_str()).or_insert_with(|| {
            order.push(row.provider_id.clone());
            (0, 0, 0.0)
        });
        entry.0 += row.turns;
        entry.1 += row.total_tokens;
        entry.2 += row.cost;
    }

    let mut stats: Vec<ProviderStat> = order
        .iter()
        .map(|id| {
            let (turns, tokens, cost) = totals[id.as_str()];
            ProviderStat {
                provider_id: id.clone(),
                display_name: catalog.provider_name(id),
                turns,
                total_tokens: tokens,
                share: 0.0,
                cost,
            }
        })
        .collect();
    stats.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens));

    let shares = share_of(
        stats.iter().map(|s| s.total_tokens),
        stats.iter().map(|s| s.turns),
    );
    for (stat, share) in stats.iter_mut().zip(shares) {
        stat.share = share;
    }
    stats
}

/// Shares by tokens, falling back to turn counts when a period recorded only failures —
/// a donut of zeros is worse than a donut of the runs that happened.
fn share_of(tokens: impl Iterator<Item = u64>, turns: impl Iterator<Item = u64>) -> Vec<f64> {
    let tokens: Vec<u64> = tokens.collect();
    if tokens.iter().sum::<u64>() > 0 {
        normalize_shares(&tokens)
    } else {
        normalize_shares(&turns.collect::<Vec<_>>())
    }
}

fn build_tools(rows: &[query::ToolRow]) -> Vec<ToolStat> {
    rows.iter()
        .map(|r| ToolStat {
            name: r.name.clone(),
            invocations: r.invocations,
            errors: r.errors,
            success_rate: 1.0 - ratio(r.errors, r.invocations),
            mean_duration_ms: if r.invocations == 0 {
                0
            } else {
                r.duration_ms / r.invocations
            },
        })
        .collect()
}

fn build_leaderboards(rows: &[query::SessionRow]) -> SessionLeaderboards {
    let rank = |row: &query::SessionRow| SessionRank {
        session_id: row.session_id.clone(),
        title: row.title.clone(),
        tokens: row.tokens,
        duration_ms: row.duration_ms,
        turns: row.turns,
    };
    let top = |mut ranked: Vec<SessionRank>, key: fn(&SessionRank) -> u64| {
        ranked.sort_by(|a, b| {
            key(b)
                .cmp(&key(a))
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        ranked.truncate(10);
        ranked
    };
    let all: Vec<SessionRank> = rows.iter().map(rank).collect();
    SessionLeaderboards {
        by_tokens: top(all.clone(), |r| r.tokens),
        by_duration: top(all.clone(), |r| r.duration_ms),
        by_turns: top(all, |r| r.turns),
    }
}

fn build_cache(
    daily: &[DailyBucket],
    models: &[query::ModelRow],
    catalog: &CatalogIndex,
) -> CacheStats {
    let read: u64 = daily.iter().map(|d| d.cache_read).sum();
    let write: u64 = daily.iter().map(|d| d.cache_write).sum();
    // What those tokens would have cost billed as fresh input, minus what they did cost.
    let estimated_savings = models
        .iter()
        .map(|m| {
            let p = catalog.pricing(&m.provider_id, &m.model_id);
            m.cache_read as f64 * (p.input - p.cache_read).max(0.0) / 1_000_000.0
        })
        .sum();

    CacheStats {
        read,
        write,
        hit_ratio: ratio(read, read + write),
        estimated_savings,
        daily: daily
            .iter()
            .map(|d| CachePoint {
                date: d.date.clone(),
                read: d.cache_read,
                write: d.cache_write,
            })
            .collect(),
    }
}

fn build_cost(
    daily: &[DailyBucket],
    models: &[ModelStat],
    providers: &[ProviderStat],
    trailing: (f64, u32),
) -> CostStats {
    let mut cumulative = 0.0;
    let by_day: Vec<CostPoint> = daily
        .iter()
        .map(|d| {
            cumulative += d.cost;
            CostPoint {
                date: d.date.clone(),
                cost: d.cost,
                cumulative,
            }
        })
        .collect();

    let mut by_provider: Vec<(String, f64)> = providers
        .iter()
        .map(|p| (p.provider_id.clone(), p.cost))
        .collect();
    by_provider.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut by_model: Vec<(ModelRefLite, f64)> =
        models.iter().map(|m| (m.model.clone(), m.cost)).collect();
    by_model.sort_by(|a, b| b.1.total_cmp(&a.1));

    let (trailing_cost, trailing_active) = trailing;
    CostStats {
        total: daily.iter().map(|d| d.cost).sum(),
        by_day,
        by_provider,
        by_model,
        // Under three active days the mean is noise, not a projection (spec 03 §3).
        projected_monthly: if trailing_active >= 3 {
            trailing_cost / 14.0 * 30.0
        } else {
            0.0
        },
    }
}

#[allow(clippy::too_many_arguments)] // one flat document, assembled from one place
fn build_headline(
    daily: &[DailyBucket],
    hourly: &[HourlyBucket],
    models: &[ModelStat],
    sessions: &[query::SessionRow],
    all_days: &[i64],
    today: i64,
    total_cost: f64,
    reasoning: u64,
) -> Headline {
    let turns: u64 = daily.iter().map(|d| d.turns).sum();
    let total_tokens: u64 = daily.iter().map(|d| d.total_tokens).sum();
    let duration_ms: u64 = daily.iter().map(|d| d.duration_ms).sum();
    let session_count = sessions.len() as u64;
    let (current_streak, longest_streak) = streaks(all_days, today);

    let peak_hour = hourly
        .iter()
        .max_by_key(|h| h.total_tokens)
        .filter(|h| h.total_tokens > 0)
        .map_or(0, |h| h.hour);

    Headline {
        sessions: session_count,
        messages: daily.iter().map(|d| d.messages).sum(),
        turns,
        total_tokens,
        input: daily.iter().map(|d| d.input).sum(),
        output: daily.iter().map(|d| d.output).sum(),
        cache_read: daily.iter().map(|d| d.cache_read).sum(),
        cache_write: daily.iter().map(|d| d.cache_write).sum(),
        // A subset of `output`, reported on its own but never added to `total_tokens`.
        reasoning,
        active_days: daily.iter().filter(|d| d.turns > 0).count() as u32,
        current_streak,
        longest_streak,
        peak_hour,
        favorite_model: models.first().map(|m| m.model.clone()),
        total_cost,
        avg_session_tokens: div(total_tokens, session_count),
        avg_turn_duration_ms: div(duration_ms, turns),
    }
}

fn div(total: u64, count: u64) -> u64 {
    if count == 0 {
        0
    } else {
        total / count
    }
}

fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

// ------------------------------------------------------------------- catalog

/// Display names and pricing, resolved once per document rather than per row —
/// `catalog::builtin()` rebuilds the whole catalog on every call.
struct CatalogIndex {
    providers: HashMap<String, String>,
    models: HashMap<(String, String), (String, Pricing)>,
}

impl CatalogIndex {
    fn load() -> Self {
        let mut providers = HashMap::new();
        let mut models = HashMap::new();
        let full = catalog::builtin();
        for provider in full.providers {
            for model in provider.models {
                models.insert(
                    (provider.id.clone(), model.id.clone()),
                    (model.name, model.pricing),
                );
            }
            providers.insert(provider.id, provider.name);
        }
        Self { providers, models }
    }

    fn provider_name(&self, id: &str) -> String {
        self.providers
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }

    fn model_name(&self, provider: &str, model: &str) -> String {
        self.models
            .get(&(provider.to_string(), model.to_string()))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| model.to_string())
    }

    fn pricing(&self, provider: &str, model: &str) -> Pricing {
        self.models
            .get(&(provider.to_string(), model.to_string()))
            .map(|(_, pricing)| pricing.clone())
            .unwrap_or_default()
    }
}
