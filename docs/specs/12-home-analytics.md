# Spec 12 — Home analytics (`FormUI/Home`)

> **Workstream W12.** Owns `app/Sources/FormUI/Home/`. Satisfies F11. Renders exactly one
> `UsageStats` document per period (spec 03) — **no aggregation in Swift**, no per-chart
> queries. Uses Swift Charts.

## 1. Layout

A scrolling dashboard in a centered column, max width 1100 pt, 24 pt gutters, a 12-column
grid with 16 pt gaps. Above the grid:

- The serif `Wordmark` and a display-size greeting (`typography.display`).
- A control row: a segmented period selector `7d / 30d / All` and tabs
  `Overview / Models / Activity / Cost`. Both persist across launches.

Cards are `surface` panels, radius `lg`, 1 pt `border`, 16 pt padding, with a 12 pt
`caption` title, an optional 11 pt tertiary subtitle, and a chart or figure below.

## 2. Tabs

### Overview
- **Headline tiles** (F11.1) — a 4 × 2 grid of compact tiles: sessions, messages, total
  tokens, active days, current streak, longest streak, peak hour, favorite model. Value in
  `typography.title` with tabular figures; label in `micro` tertiary. Abbreviate large
  numbers (`21.8M`, `23,627`) with the full value in a tooltip.
- **Activity heatmap** (F11.2) — day × week grid, 11 pt cells, 3 pt gaps, 5 intensity stops
  from `heatmapScale`, month labels along the top and weekday labels at the leading edge.
  Hover shows date, tokens, sessions. Matches the reference's compact heatmap block.
- **Tokens over time** (F11.3) — stacked area, series input / output / cacheRead /
  cacheWrite, from `daily`.
- **Sessions and messages per day** — bar + line combo.
- A one-line playful footnote comparing total tokens to a familiar quantity, matching the
  reference's tone (e.g. `You've used ~991× more tokens than The Little Prince.`). Compute
  it from a small table of reference works; keep it in one file so it is easy to change.

### Models
- **Token share donut** (F11.5) with a ranked legend, and **ranked bars** by tokens and by
  message count.
- **Per-model table**: model, provider, turns, tokens, share, cost, avg TTFT, avg tok/s,
  error rate. Sortable by column.
- **Latency and throughput** (F11.6): grouped bars for p50/p90/p99 TTFT and tok/s per model,
  plus a distribution plot from `LatencyStat.histogram`.

### Activity
- **Hour-of-day histogram** (F11.4) and the **weekday × hour matrix** as a heat grid.
- **Turn duration distribution**.
- **Tool usage** (F11.8): most-invoked tools with counts, success rate, mean duration.
- **Session leaderboards** (F11.9): three ranked lists — by tokens, duration, turn count —
  each row navigating to the session on click.

### Cost
- **Spend over time** (F11.7) — area, with a cumulative overlay.
- **By provider** and **by model** — ranked bars.
- **Projected monthly run rate** — a headline figure with the basis stated
  ("14-day average × 30").
- **Cache effectiveness** (F11.10) — read vs write over time, hit ratio, estimated savings.

## 3. Chart conventions (F11.11)

Build a `ChartCard` container and a shared `ChartStyle` so every chart agrees on:

- Series colors from `color.chartSeries` in order, consistent per series **across all
  charts** (input is always the same color everywhere).
- Grid `color.chartGrid` at 1 pt, horizontal only; axes `color.chartAxis`, `micro` labels.
- Y-axis token counts abbreviated (`1.2M`), currency as `$0.00`, durations as `1.2s`/`340ms`.
- A shared hover/tooltip treatment: a vertical rule plus a popover card with the date and
  every series value.
- Legends below the chart, wrapping, 11 pt, with a color swatch.
- Charts animate in on first appearance and interpolate on period change
  (`motion.emphasized`), and do neither under reduce-motion.

## 4. States (F11.12)

- **Loading:** skeleton cards with a shimmer, not a spinner.
- **Empty** (no data in range): a designed `EmptyState` per card explaining what will appear.
- **Sparse** (< 3 active days): charts still render; projections and percentiles show
  `—` with a tooltip explaining the minimum sample.

## 5. Data flow

`StatsStore` (W7) provides `UsageStats`. Refetch on period change and on
`stats_invalidated` (coalesced). Rendering must be pure over the document — a test that
feeds two fixture documents and snapshots the dashboard proves it.

## 6. Done when

- Acceptance criterion 3 holds: on first launch, every chart renders real aggregates from
  the seeded corpus.
- All four tabs are complete; no placeholder cards.
- Period switching re-renders in under 100 ms with no layout jump.
- Both themes; charts legible in each; contrast test passes for chart text.
- Clicking a leaderboard row routes to that session.
