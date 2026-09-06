# Data visualisation

How Temps draws data. Companion to `brand-guidelines.md` §6 (colour is status,
"a legend does not license colour") and `design-system-handoff.md` §6 and §8.
The primitives live in `@temps-sdk/op`: `TimeChart`, `Breakdown`, `Funnel`,
`Flow`, `StatusStrip`, `ScoreRing`, `CalendarHeatmap`, `Waterfall`,
`Histogram`, `Sparkline`, `GeoMap`. Reference render: `/op-components#chart`
and the `DataVizBlocks` section.

Imperative only. When this file and the two above disagree, they win.

## 1. Pick the chart from the question

Ask what the reader came to find out, then read the row.

| The question | The chart |
|---|---|
| A value over time — "when did it change?" | `TimeChart`, one line per series, deploy markers on the axis |
| Share of a whole — "what is 5xx out of everything?" | `Breakdown`, share as an ink bar behind the row |
| Ranked categories — "which pages, which countries?" | `Breakdown`, sorted, honest "other" remainder |
| Steps and drop-off — "where do they leave?" | `Funnel`, bars by share of entrants, drop-off ≥ 50% red |
| From → to — "what did they do next?" | `Flow`, ranked "A → B" rows with count and share |
| A rate against a threshold — "is it over budget?" | `TimeChart` with a `thresholds` line and the series toned by its state |
| Distribution — "is the p95 one slow route or all of them?" | `Histogram` with the percentile selector |
| Availability by bucket — "was it up all day?" | `StatusStrip`, one segment per bucket |
| A score 0–100 — "how is LCP doing?" | `ScoreRing`, number in the middle, tone at the vitals thresholds |
| Activity by day — "how often do we ship?" | `CalendarHeatmap`, five ink intensities |
| Nested timing — "which span ate the request?" | `Waterfall`, bars by offset and width |
| By country | The ranked `Breakdown` list first; `GeoMap` is its second view, never the only one |
| One number's shape in a row | `Sparkline`: no axes, no number of its own, the cell beside it carries the value |

- Never draw a pie, a donut, a treemap or a stacked area. Share of a whole is a
  ranked list with a bar; the reader compares lengths from one baseline, and
  can read the number.
- Never draw a chart for three numbers. Three numbers are a `MetricGrid`.
- Good: `<Breakdown rows={statuses} total={SUM} unit="requests" />` for the
  status-class split. Bad: a four-slice donut of 2xx/3xx/4xx/5xx where 5xx is
  0.4% and invisible.

## 2. Series without a second hue

- Tell series apart by pattern, never by hue. `Series` takes `stroke`
  (`solid` · `dashed` · `dotted`) and `weight` (`thin` · `regular`), defaulted
  by position: the first line solid and regular, then dashed, dotted, solid,
  each thin.
- Never write the legend by hand. `TimeChart` generates it from `series`: the
  swatch is a sample of the real line, the name is muted, and the value at the
  cursor rides the label. A hand-written key drifts from the plot the first
  time an order changes, and a muted sentence ("thick p50, thin p99") cannot be
  matched to a line at all.
- Good: `series={[{ key: 'p50', name: 'p50' }, { key: 'p99', name: 'p99', stroke: 'dashed' }]}`.
  Bad: `<ChartFooter>· thick p50, thin p99</ChartFooter>`.
- Label the line at its end when the plot is wide and the names are short; the
  legend stays as the keyboard and phone reading of the same thing.
- More than four series on one plot is a table or small multiples — one plot
  per series, same y scale, stacked. Four dash patterns is the limit of what
  the eye separates; the fifth line is decoration. `TimeChart` warns in dev.
- Colour appears only on a series that *is* a state: set `series.state` and the
  line takes that tone. The case that earns it is a rate against a threshold —
  an error rate in `error` above its `thresholds` line. Everything else is ink.
- Never use `--chart-1` / `--chart-2` (or any hue) to separate two series. A
  legend does not license colour: the reader who needs the colour is the reader
  who did not read the legend.

## 3. Axes and scales

- Start a count axis at zero. A truncated y turns a 2% wobble into a cliff.
- Never truncate the y axis on bars, ever. A bar's length *is* the value.
- Let a line chart of a bounded rate (latency, percentage) start above zero
  only when the floor is labelled on the axis and the footer says so.
- Use a log scale only when the axis is labelled `log`, and never for a rate.
- Put deploy markers on every time axis (`markers`), with `at` and `note` so
  the cluster strip can name them. An axis without deploys cannot answer "since
  which deploy", which is the question.
- Ticks come from the locale helpers (`fmtNum`, `fmtAbsolute`), not from
  hand-built strings; four to six ticks on x, three on y.
- Zero is a value: the line sits on the axis and the readout says `0`. Missing
  is a gap and an en dash in the table view. Never draw them the same.
- Keep the y unit out of the ticks. `184` on the axis, `ms` in the header.

## 4. Annotations

- Deploy markers: dotted ink verticals labelled with the deploy id, collapsing
  into "3 deploys" when the labels would overprint. Every deploy keeps its line.
- Threshold lines: `thresholds={[{ y, label, state }]}` — dashed, labelled at
  the right edge, in the state tone. The series that crosses it may carry the
  same tone; nothing else on the plot may.
- Sampled band: `sampled={{ from, to, label }}` shades the window in muted with
  `◌ sampled 1 in 4` inside it. Never silently thin a line.
- Retention horizon: ranges past it are struck through in the `RangePicker` and
  named in the footer. Strike, never hide.
- Selection window: an ink band at 6% with a dashed edge and a strip under the
  plot stating the bounds, the point count and "clear (esc)". A selection
  filters what is *below* the chart; it never changes the chart's own range.
- Four annotations on one plot is the ceiling. A fifth belongs in the footer.

## 5. Empty and partial

- A chart with no data says which of the four reasons it is, in a `PageState`
  where the plot would be, at the plot's height:
  - **no traffic** — `empty`: nothing has happened yet; say what would make it
    happen ("open a project's *.temps URL").
  - **not configured** — `unconfigured`: say what is missing and link the
    settings page. Never render nothing.
  - **sampled** — the plot renders with the sampled band and the footer says
    the ratio; the numbers are estimates and the footer says that too.
  - **past retention** — `empty` with the horizon named and the range that
    would work; the gated range stays visible, struck through.
- Hatch a partial bucket (the one still filling) and say "current bucket
  partial" in the footer. Never let the last bar dive because the minute is
  half over.
- A flat line at zero is data, not an empty state. Say "no requests in this
  window", keep the plot.

## 6. Accessibility

- Every chart root is `role="img"` with an `aria-label` that is a sentence
  stating what it is, over what range, and the verdict: pass `title`, `range`
  and `verdict` to `TimeChart`. Good: `"p95 latency in ms, last 24h. Flat at
  50ms except one burst at 10:41. 240 points."` Bad: `aria-label="chart"`.
- Every chart has a "view as table" affordance rendering the same data as an
  `.op-rows` table — `TimeChart`'s `table` toggle does this by default. A chart
  with no table view is not shippable.
- Make the readout row keyboard-navigable: one focusable region, `←` and `→`
  move through the buckets and announce each in a live region, the way
  `StatusStrip` does.
- Hover-only readouts are banned: anything reachable only by a pointer is
  invisible on a phone and to a keyboard. The `GeoMap` desktop pointer readout
  is the one exception, and only because the ranked list beside it carries the
  same data for the keyboard.
- Put touch readouts under the chart, not in a tooltip over it: a finger
  covers the point it is asking about.
- Never encode a value in colour alone. Tone always arrives with a glyph and a
  word (`Status`), on the chart and in its legend.
- Contrast: lines are ink on paper and pass by construction. State tones are
  the audited `--success` / `--warning` / `--destructive`, never a light tint.

## 7. Numbers on charts

- Mono, tabular, always. A number that changes on hover must not move the
  layout.
- Put the unit once, in the header or the column head (`p95 latency (ms)`), not
  on every tick and not on every row.
- Format with the `fmt*` helpers so `30.8k`, `184ms` and `0.61%` read the same
  everywhere. Never `toFixed` in a screen.
- Never set a number on top of a coloured bar. It sits beside the bar in its
  own column.
- Round on the axis, never in the readout: the axis says `2k`, the readout says
  `2,041`.

## 8. Footer contract

Every chart's `ChartFooter` states, in this order, only what applies:

1. **What and how big a bucket** — `requests / minute`.
2. **Range** — `last 24h`, or the custom window.
3. **Retention** — `retention 30d`, with gated ranges struck through.
4. **Sampled** — `◌ sampled 1 in 4 since 14:00`, if any.
5. **Baseline of every delta** — `+12% vs the previous 24h`. A delta with no
   baseline is a rumour.
6. **What a drag does**, if the chart is selectable.

Never put the legend in the footer: the chart draws it.

Good:

```tsx
<TimeChart data={T} series={[{ key: 'p50', name: 'p50' }, { key: 'p99', name: 'p99', stroke: 'dashed' }]}
  unit="ms" title="p95 latency" range="last 1h" verdict="Flat at 50ms except one burst at 10:41."
  markers={[{ id: 'dep_91a', x: '10:41' }]} />
<ChartFooter><span>latency / minute · 1h</span><span>· retention 30d</span><span>· ┆ deploy</span></ChartFooter>
```

Bad:

```tsx
<TimeChart data={T} series={[{ key: 'p50', name: 'p50' }, { key: 'p99', name: 'p99' }]} />
<ChartFooter><span>latency</span><span>· thick p50, thin p99</span><span>· +12%</span></ChartFooter>
```

No range, no retention, a legend the reader cannot match to a line, and a
delta with no baseline.
