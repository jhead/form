//! The arithmetic the SQL cannot do: nearest-rank percentiles over raw samples, streaks,
//! quantile levels, and share normalisation. Kept free of SQL and of `UsageStats` so the
//! rules in spec 03 §3 can be tested directly.

use super::types::HistogramBin;

/// Nearest-rank percentile on an ascending sample: rank = ⌈p·N⌉, 1-based.
pub(crate) fn nearest_rank_u64(sorted: &[u64], p: f64) -> u64 {
    rank_index(sorted.len(), p).map_or(0, |i| sorted[i])
}

pub(crate) fn nearest_rank_f64(sorted: &[f64], p: f64) -> f64 {
    rank_index(sorted.len(), p).map_or(0.0, |i| sorted[i])
}

fn rank_index(len: usize, p: f64) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let rank = (p * len as f64).ceil().max(1.0) as usize;
    Some(rank.min(len) - 1)
}

/// `(current, longest)` runs of consecutive days. `days` is sorted and deduplicated.
///
/// A streak survives an empty *today* until the day rolls over: someone who worked
/// yesterday and has not started yet this morning has not lost their streak.
pub(crate) fn streaks(days: &[i64], today: i64) -> (u32, u32) {
    if days.is_empty() {
        return (0, 0);
    }

    let mut longest = 1u32;
    let mut run = 1u32;
    for pair in days.windows(2) {
        run = if pair[1] == pair[0] + 1 { run + 1 } else { 1 };
        longest = longest.max(run);
    }

    let last = days[days.len() - 1];
    let current = if last == today || last == today - 1 {
        let mut n = 1u32;
        let mut expected = last - 1;
        for &day in days.iter().rev().skip(1) {
            if day != expected {
                break;
            }
            n += 1;
            expected -= 1;
        }
        n
    } else {
        0
    };

    (current, longest.max(current))
}

/// Shares that sum to exactly 1.0: the largest bucket absorbs the rounding, so a donut
/// never shows a sliver of leftover.
pub(crate) fn normalize_shares(values: &[u64]) -> Vec<f64> {
    let total: u64 = values.iter().sum();
    if values.is_empty() || total == 0 {
        return vec![0.0; values.len()];
    }
    let mut shares: Vec<f64> = values
        .iter()
        .map(|&v| v as f64 / total as f64)
        .collect::<Vec<_>>();
    let largest = values
        .iter()
        .enumerate()
        .max_by_key(|&(i, v)| (v, std::cmp::Reverse(i)))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let rest: f64 = shares
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != largest)
        .map(|(_, s)| *s)
        .sum();
    shares[largest] = 1.0 - rest;
    shares
}

/// Heatmap intensity per cell: 0 for exactly zero, else the quantile of the value within
/// the non-zero distribution mapped onto 1..=4 (five stops with zero, per spec 03 §3 and
/// spec 12 §2 — level 0 stays reserved for an empty day).
pub(crate) fn heat_levels(tokens: &[u64]) -> Vec<u8> {
    let mut nonzero: Vec<u64> = tokens.iter().copied().filter(|&t| t > 0).collect();
    nonzero.sort_unstable();
    let n = nonzero.len();
    tokens
        .iter()
        .map(|&t| {
            if t == 0 || n == 0 {
                return 0;
            }
            // Rank within the non-zero distribution rather than a cut on its values:
            // with two busy days a value-threshold rule can never reach the top stop,
            // and the busiest day in the period must always read as the busiest.
            let rank = nonzero.partition_point(|&v| v <= t);
            (((rank * 4) + n - 1) / n).clamp(1, 4) as u8
        })
        .collect()
}

/// Time-to-first-token bins, fixed so the distribution plot is comparable across models.
const TTFT_EDGES: [u64; 7] = [250, 500, 1_000, 2_000, 4_000, 8_000, 16_000];

pub(crate) fn ttft_histogram(sorted: &[u64]) -> Vec<HistogramBin> {
    let mut bins: Vec<HistogramBin> = Vec::with_capacity(TTFT_EDGES.len() + 1);
    let mut lower = 0u64;
    for &edge in &TTFT_EDGES {
        bins.push(HistogramBin {
            lower_ms: lower,
            upper_ms: Some(edge),
            count: 0,
        });
        lower = edge;
    }
    bins.push(HistogramBin {
        lower_ms: lower,
        upper_ms: None,
        count: 0,
    });

    for &sample in sorted {
        let idx = TTFT_EDGES
            .iter()
            .position(|&edge| sample < edge)
            .unwrap_or(TTFT_EDGES.len());
        bins[idx].count += 1;
    }
    bins
}

/// Output throughput for one turn, in tokens/sec. `None` when the post-first-token window
/// is under 50 ms — a denominator that small turns rounding into a fantasy number.
pub(crate) fn throughput(output: u64, duration_ms: i64, ttft_ms: Option<i64>) -> Option<f64> {
    let window = duration_ms - ttft_ms.unwrap_or(0);
    (window >= 50).then(|| output as f64 * 1000.0 / window as f64)
}

pub(crate) fn mean_u64(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    (values.iter().sum::<u64>() as f64 / values.len() as f64).round() as u64
}

pub(crate) fn mean_f64(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}
