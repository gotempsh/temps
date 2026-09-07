# Data visualisation

How Temps draws data. Companion to `brand-guidelines.md` §6 (colour is status,
"a legend does not license colour") and `design-system-handoff.md` §6 and §8.
The primitives live in `@temps-sdk/op`: `TimeChart`, `Breakdown`, `Funnel`,
`Flow`, `StatusStrip`, `ScoreRing`, `CalendarHeatmap`, `Waterfall`,
`Histogram`, `Sparkline`, `GeoMap`, and the second wave in §§9–21:
`BandChart`, `TimeChart`'s `compare`, `StackedInk`, `LatencyHeatmap`,
`PercentileLadder`, `CohortGrid`, `PathTree`, `SessionTimeline`, `UsageBar`,
`Gauge`, `StateTimeline`, `DeltaTable`, `WindowTimeline`, `Topology`.
Reference render: `/op-components#chart` and the `DataVizBlocks` section;
the second wave is `DataVizBlocks2` (`viz-band` … `viz-topology`).

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
| Is this value normal? — "is 980ms unusual for 10:00?" | `BandChart`: expected range hatched behind the line, anomalies as × and as rows |
| Against the period before — "is this week up on last?" | `TimeChart` with `compare`: prior period as a dotted ghost, delta in the legend |
| Composition over time — "when did the 5xx arrive?" | `StackedInk`, ≤4 layers by pattern, stacked bars from zero. Never a stacked area |
| Two populations or one — "is the p95 one slow route?" | `LatencyHeatmap`, time × latency bucket, ink density by count |
| The shape of a distribution in a tile | `PercentileLadder`: p50 · p95 · p99 · max, each with its own baseline delta |
| Do they come back? — "does week 1 hold?" | `CohortGrid`: a real `<table>`, cohorts as rows, periods as columns |
| What did they do next? | `PathTree`: indented tree with counts and drop-off. Never a Sankey |
| What happened in this session? | `SessionTimeline`: axis plus a synchronised event list; the list carries the keyboard |
| Against an allowance — "am I over?" | `UsageBar`: the sentence first, the overage hatched, sampling marked |
| A machine's pressure — cpu, memory, disk | `Gauge` tiles in a `MetricGrid`. Never a radial gauge |
| How long was it down? | `StateTimeline`: segments as wide as they were long, with durations |
| Did the release help? — "before and after" | `DeltaTable`: metric · before · after · delta, tone only at a threshold |
| What can a restore reach? | `WindowTimeline`: backups as marks, the covered window as a band, the target as a cursor |
| What talks to what? | `Topology`: layered, deterministic, with the same nodes as a list beneath |

- Never draw a pie, a donut, a treemap, a stacked area, a Sankey, a radial
  gauge or a force-directed graph. Share of a whole is a
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

## 9. BandChart — is this value normal?

**When.** The metrics explorer, and any alert that fires on a model rather than
on a fixed line. Use it when the reader cannot judge a number without knowing
what was expected of it: 980ms means nothing until the band says the ceiling
was 407ms.

**What it must state.**

1. The expected range as a **hatched ink band**, its bounds taken from the data
   (`band={{ lower, upper }}`); the measured value as the ink line over it.
2. Where the line leaves the band, **that segment** in the state tone with a ×
   at its peak. Colour is allowed there, and only there, because the segment
   *is* a state — a value outside its own model. `worse` says which direction
   counts (`up` for latency and errors, `down` for throughput and conversion,
   `both` for a ratio); `errorAt` escalates `warn` to `error` when the peak also
   crosses a budget.
3. The excursions **derived** from the data and the band, never hand-listed, so
   the plot, the footer, the list and the sentence cannot disagree about how
   many there were or which was worst.
4. A footer that states them as facts with the deploy beside — `3 anomalies ·
   worst +42% at 14:20 ┆ dep_91a · band: rolling 7d` — and a generated legend:
   expected · actual · anomaly. A band nobody can explain is a decoration, so
   `bandNote` says how it was computed.
5. A `vs expected` column in the table view: `inside`, `+141% above`,
   `−22% below`.
6. An `aria-label` that states how many anomalies there were and the worst one.
7. A cursor readout in that order: actual · expected range · delta.
8. A focusable row per excursion under the plot, with its value, how far
   outside, how many buckets it lasted and the deploy nearest it.

**Banned.** A red area flood or a shaded rectangle behind the plot: a wash
cannot be compared with anything and it hides the band. A filled coloured area
for the band itself — it is a range, not a series. A × on the plot with no row
under it: a glyph is unreachable by keyboard. A second chart engine —
`BandChart` is `TimeChart` with `band` and `anomalies`.

## 10. Compare — against the period before

**When.** Any metric a reader judges as "up or down on last time": visitors,
requests, signups, spend. Off by default; a comparison the reader did not ask
for doubles the ink on every chart.

**What it must state.** The prior period as a **dotted thin ghost** — the same
measure, so it must not read as a peer of the line in front of it — and the
delta in the generated legend beside the label it belongs to, with its baseline
spelled out (`+9% vs prior 7d`). The compared window must be the same length as
the shown one.

**Banned.** A second full-weight series for the prior period. A delta in the
footer with no baseline. Comparing a 7-day window with a 30-day one.

## 11. StackedInk — composition over time

**When.** The reader wants both the split and when it changed: status classes
(2xx/3xx/4xx/5xx), tokens by model, log volume by level, backup size by source.
When only the split matters, that is `Breakdown`; when only the total matters,
that is `TimeChart`.

**What it must state.** Stacked **bars** from a zero baseline, at most four
layers, told apart by pattern (solid · hatched · dotted · cross-hatched) at no
more than three greys. A generated legend with the layer's total and share, the
pattern named for a screen reader, a table view, and a keyboard readout that
walks the buckets. A partial trailing bucket is hatched and named in the footer.

**Banned.** A stacked **area**: its middle bands float off the baseline and
cannot be compared or read. A fifth layer. Hue to separate layers. Tone on any
layer that is not itself a state — 5xx and `error` earn it, "3xx" does not.

## 12. LatencyHeatmap — time × bucket

**When.** A percentile has moved and the reader needs to know whether one route
got slow or everything did. Two horizontal bands in this grid are two
populations, which no percentile line can show.

**What it must state.** Time on x, latency buckets on y, count as one of the
five ink steps (the same ladder `CalendarHeatmap` uses), zero as the empty
step. A legend that prints the top of the scale. A cell readout reachable with
the arrow keys, and a table view with every count. Percentile overlays are
optional, dashed, and land in the bucket their value belongs to — never on a
linear scale over non-linear buckets.

**Banned.** A colour ramp. A cell whose only encoding is its shade with no way
to read the number. An overlay drawn over the axis labels.

## 13. PercentileLadder — p50 · p95 · p99

**When.** An aside or a tile where a `Histogram` will not fit, and the reader
wants the shape of a distribution in four numbers.

**What it must state.** Each rung's name, its number, an ink bar on one shared
scale from zero (so a p99 four times p50 looks four times as long), and each
rung's own delta **with its baseline**. Tone only on a rung that is a state —
a p99 over its budget.

**Banned.** A delta without the window it is measured against. A truncated
scale. Drawing four numbers as a chart with axes: this is a small table of
numbers and reads as one.

## 14. CohortGrid — retention

**When.** "Do they come back?" — signup, activation and billing cohorts.

**What it must state.** It *is* a table, so it is a `<table>` with a row header
per cohort and a column header per period. The percentage is printed in the
cell; ink density is the second encoding, never the only one. The cohort size
is a column, because a bright 100% on 12 people is not a result. A period a
cohort has not lived through yet is an en dash, not a zero.

**Banned.** A red-to-green scale: retention is how much, not how well. An
unlabelled density key. Rendering "not yet" and "nobody came back" the same.

## 15. PathTree — journeys

**When.** "Where do they go next?" on analytics journeys and multi-step flows.

**What it must state.** Entry at the root; each step with its count, its share
of the step above, and how many left there. Branches collapse, and every toggle
is a real button so Tab and Enter walk the journey. Drop-off at or above the
alert threshold is the only thing on a row that takes a tone.

**Banned.** A Sankey or an alluvial diagram: the ribbons have no baseline, the
labels overprint the moment there are more than four branches, and a keyboard
cannot reach any of it. A share that is of the total rather than of the step
above, without saying so.

## 16. SessionTimeline — a session

**When.** Session replay, and any "what happened in this run" view. The player
itself (rrweb) is not ours; this is the contract around it.

**What it must state.** The events on a time axis with the player's position,
and the same events as a synchronised list. The list is the primary view and
carries the keyboard: arrows step, Enter seeks. The axis draws ticks, not
glyphs: a page view is a full-height rule (a page boundary), any other event a
short tick, an error a red rule; the kind glyph lives in the list, where there
is room for it. The track is inset from the frame so `0s` and the end sit
inside it, and the scrubber, a native range input, spans exactly the track so
the thumb sits under the cursor line. Only an event that is a state (a failed
request, a thrown error) takes a tone; a click is not a state. Offsets are
from the start of the recording, and zero is `0s`.

**Banned.** Glyphs on the axis: at a session's density they collide, and the
one at `0s` sits on the border. Marks on the axis with a hover-only tooltip
and no list. A custom scrubber a keyboard cannot drive. Colour-coding every
event kind.

## 17. UsageBar — usage against an allowance

**When.** Ingest, disk, bandwidth, AI credits, seats — anywhere a number is
read against a limit, on a plan page, an analytics header or a node record.

**What it must state.** Text first: used, allowance, share and the plan word as
a sentence **above** the bar, so the fact survives with the bar switched off.
The overage past the allowance is hatched, because beyond the line is a
different fact and not more of the same. Where sampling began is marked, and
the footer says the figure past it is an estimate. The action that changes the
limit sits in the footer.

**Banned.** A bar pinned at 100% with the overage invisible. A percentage with
no absolute numbers beside it. A limit with no plan word: the reader must know
*which* limit this is.

## 18. Gauge — cpu, memory, disk

**When.** A machine's pressure on a node record, a container's on a service.
`MetricGrid`-shaped, so three of them sit in one bordered grid.

**What it must state.** The figure, a horizontal ink bar from zero to the full
scale, what the scale is *of* (`of 4 GB`), the peak in the window with the
window named, and the warn and error lines as ticks that each carry their own
word. A node with no samples keeps its tiles and says why they are empty —
never a missing tile.

**Banned.** A radial gauge or a needle: an arc cannot be compared with the arc
beside it, its ends are not a baseline, and the needle repeats the number.
A truncated scale. A threshold line the reader cannot name.

## 19. StateTimeline — state over time

**When.** One resource's record, where the question is *for how long*: uptime
up/degraded/down, a deployment's lifecycle, a node's heartbeat.

**`StateTimeline` or `StatusStrip`?** `StatusStrip` buckets a window into equal
segments, so a column of monitors in a ledger can be compared by shape; it
cannot say how long anything lasted. `StateTimeline` draws the real transitions
at their real widths, so thirty minutes down does not look like three one-minute
blips. Ledger: `StatusStrip`. Record: `StateTimeline`.

**What it must state.** Segments as wide as they were long, the duration and
share of each state in a generated legend, a readout per segment reachable with
the arrow keys, and a table view with every segment and its reason.

**Banned.** Both forms for the same window on the same screen — a fact appears
once. A segment with no duration. A legend typed by hand instead of derived
from the states present.

## 20. DeltaTable — release comparison

**When.** Two deploys, two windows, two nodes: "did the release help?"

**What it must state.** Metric · before · after · delta, with both columns named
by what they are (a deploy tag, a window). The deltas are `Num`s and stay ink.
Tone appears **only** when a threshold makes the after value a state, and it
arrives with the threshold's own words.

**Banned.** A red "+12%" on a metric with no budget: the reader cannot tell bad
from bigger. A delta column with no note of which direction is better. Comparing
windows of different lengths without saying so.

## 21. WindowTimeline — backups and PITR

**When.** Beside the point-in-time restore form, and anywhere a recovery window
has to be understood before it is typed into.

**What it must state.** Full backups as marks (a failed one in `error` tone),
the window the write-ahead log covers as a hatched band, and the restore target
as a cursor on the same axis. The zone, printed: a time with no zone beside it
is a guess. A per-mark readout on the arrow keys and a table of every backup.

**Banned.** A restore form with no picture of the window it accepts. A cursor
outside the window drawn as though it were inside it. Relative times ("2h ago")
on the axis: the picture is a clock.

## 22. Topology — cluster and service map

**When.** The cluster page (control plane, workers, their WireGuard reach) and
a service map (services and their call edges).

**What it must state.** A layered, deterministic layout — `layer` decides the
row, array order decides the column — so today's picture can be compared with
yesterday's. Nodes are cards with a glyph and a word; relay links are dashed,
because a relay is a different kind of reach and not a worse one. The list
beneath is the primary view and carries the keyboard, the facts and the state
words; the graph is one `role="img"` with a sentence and **no focusable
children**, exactly as `GeoMap` sits under a ranked list — a control inside a
picture is a nested-interactive violation, and the list already has a button
for every node. On a phone the graph clips and the list carries everything.

**Banned.** A force-directed layout: a graph that moves between reloads cannot
be compared with the one the operator saw yesterday. A graph with no list. An
edge whose only meaning is a colour. A focusable node inside the `role="img"`.

## 23. CalendarHeatmap has a readout too

The oldest of the density figures had none: its cells carried a native `title`,
which is a delayed tooltip that never appears on touch and cannot be reached by
a keyboard, and its legend said "less … more" with no numbers.

It now follows the same rule as `GeoMap` and the charts: on a fine pointer the
hovered day reads at the cursor and nothing is added under the grid; below `md`
the readout is a row under the grid (tap to read, tap again to open); the grid
is one focusable region where `←` `→` move a week, `↑` `↓` move a day and `⏎`
opens; and the legend prints the numbers behind the five swatches
(`0 · 1–2 · 3–4 · 5–7 · 8+`). Pass `ids` and the readout names what shipped
that day (`3 deploys · dep_91a, dep_91b, dep_90e`) instead of only counting it.
