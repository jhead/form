//! Local-time bucketing without a SQLite timezone extension.
//!
//! Every bucket in `UsageStats` is a *local* day, hour or weekday, but SQLite knows
//! nothing about IANA zones and `rusqlite` is built without the `functions` feature, so we
//! cannot hand it `chrono-tz` as a scalar function. What we can do is exploit the fact
//! that a zone's UTC offset is piecewise constant with a couple of transitions per year:
//! resolve those transitions once in Rust, then render them as an integer `CASE`
//! expression that SQLite evaluates inside the `GROUP BY`. Aggregation stays in SQL and
//! the DST arithmetic stays in `chrono-tz`.

use std::fmt::Write as _;

use chrono::{DateTime, Offset, TimeZone};
use chrono_tz::Tz;

pub(crate) const DAY_MS: i64 = 86_400_000;
pub(crate) const HOUR_MS: i64 = 3_600_000;

/// Transitions are months apart in every real zone, so a 12-hour probe cannot step over
/// one; the exact instant is then found by bisection.
const PROBE_MS: i64 = 12 * HOUR_MS;

fn offset_at(tz: Tz, ms: i64) -> i64 {
    let utc = DateTime::from_timestamp_millis(ms).unwrap_or_default();
    i64::from(
        tz.offset_from_utc_datetime(&utc.naive_utc())
            .fix()
            .local_minus_utc(),
    ) * 1000
}

/// The UTC offset of one zone over one window, as sorted `(segment start, offset ms)`.
pub(crate) struct Offsets {
    segments: Vec<(i64, i64)>,
}

impl Offsets {
    pub(crate) fn build(tz: Tz, from_ms: i64, to_ms: i64) -> Self {
        let from = from_ms.min(to_ms);
        let mut segments = vec![(i64::MIN, offset_at(tz, from))];
        let mut prev = from;
        let mut probe = from + PROBE_MS;
        loop {
            let at = probe.min(to_ms);
            let offset = offset_at(tz, at);
            if offset != segments[segments.len() - 1].1 {
                // First instant in `(prev, at]` that carries the new offset.
                let (mut lo, mut hi) = (prev, at);
                while lo + 1 < hi {
                    let mid = lo + (hi - lo) / 2;
                    if offset_at(tz, mid) == offset {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                segments.push((hi, offset));
            }
            if at >= to_ms {
                break;
            }
            prev = at;
            probe += PROBE_MS;
        }
        Self { segments }
    }

    pub(crate) fn offset_ms(&self, ms: i64) -> i64 {
        match self.segments.binary_search_by_key(&ms, |&(start, _)| start) {
            Ok(i) => self.segments[i].1,
            // The first segment starts at `i64::MIN`, so `i` is never 0.
            Err(i) => self.segments[i - 1].1,
        }
    }

    pub(crate) fn local_ms(&self, ms: i64) -> i64 {
        ms + self.offset_ms(ms)
    }

    /// Days since the local 1970-01-01.
    pub(crate) fn day(&self, ms: i64) -> i64 {
        self.local_ms(ms).div_euclid(DAY_MS)
    }

    /// UTC instant of local midnight starting `day`. Two passes converge everywhere
    /// except inside a spring-forward gap, where local midnight does not exist and the
    /// fixpoint lands on the instant the day begins — which is the honest answer.
    pub(crate) fn day_start_utc(&self, day: i64) -> i64 {
        let local = day * DAY_MS;
        let approx = local - self.offset_ms(local);
        local - self.offset_ms(approx)
    }

    /// `(<col> + <offset>)` in local milliseconds, for use inside a `GROUP BY`.
    pub(crate) fn sql_local_ms(&self, col: &str) -> String {
        if self.segments.len() == 1 {
            return format!("({col} + {})", self.segments[0].1);
        }
        let mut sql = format!("({col} + CASE");
        for pair in self.segments.windows(2) {
            let _ = write!(sql, " WHEN {col} < {} THEN {}", pair[1].0, pair[0].1);
        }
        let _ = write!(
            sql,
            " ELSE {} END)",
            self.segments[self.segments.len() - 1].1
        );
        sql
    }

    /// Local day index. Safe as truncating division because every window we query is
    /// clamped to non-negative instants.
    pub(crate) fn sql_day(&self, col: &str) -> String {
        format!("({} / {DAY_MS})", self.sql_local_ms(col))
    }

    pub(crate) fn sql_hour(&self, col: &str) -> String {
        format!("(({} % {DAY_MS}) / {HOUR_MS})", self.sql_local_ms(col))
    }
}

/// `YYYY-MM-DD` for a local day index.
pub(crate) fn day_to_date(day: i64) -> String {
    DateTime::from_timestamp(day * 86_400, 0)
        .unwrap_or_default()
        .date_naive()
        .to_string()
}
