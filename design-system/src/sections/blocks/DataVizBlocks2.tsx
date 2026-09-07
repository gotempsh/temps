// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { Block, Demo, Rule } from '@/components/op-doc'
import {
  BandChart, ChartFooter, CohortGrid, DeltaTable, Gauge, LatencyHeatmap, MetricGrid,
  PathTree, PercentileLadder, SessionTimeline, StackedInk, StateTimeline, StatusStrip,
  TimeChart, Topology, UsageBar, WindowTimeline, fmtBytes,
  type Cohort, type InkLayer, type SessionEvent, type StateSegment, type TimePoint, type TopoLink, type TopoNode,
} from '@/components/op'

/**
 * The second wave of data-visualisation primitives, drawn. Companion to
 * `docs/data-viz.md` §§9–21: the forms Temps data needs that `TimeChart`,
 * `Breakdown`, `Funnel` and `StatusStrip` could not draw — an expected range,
 * a period-over-period ghost, a composition, a latency grid, cohorts,
 * journeys, a session, an allowance, a machine's pressure, a resource's
 * state, a release comparison, a recovery window, a cluster.
 *
 * Every fixture here is shaped like real Temps data (acme-storefront,
 * api-gateway, hetzner-1..3) so a reader can see what the primitive says
 * before they have any of their own.
 */

export const DATAVIZ2_TOC = [
  ['viz-band', 'BandChart — expected range'],
  ['viz-compare', 'Compare — period over period'],
  ['viz-stacked', 'StackedInk — composition over time'],
  ['viz-heatmap', 'LatencyHeatmap — time × bucket'],
  ['viz-ladder', 'PercentileLadder — p50 · p95 · p99'],
  ['viz-cohort', 'CohortGrid — retention'],
  ['viz-paths', 'PathTree — journeys'],
  ['viz-session', 'SessionTimeline — a session'],
  ['viz-usage', 'UsageBar — usage against an allowance'],
  ['viz-gauge', 'Gauge — cpu, memory, disk'],
  ['viz-state', 'StateTimeline — state over time'],
  ['viz-delta', 'DeltaTable — release comparison'],
  ['viz-window', 'WindowTimeline — backups and PITR'],
  ['viz-topology', 'Topology — cluster and service map'],
] as const

// ── fixtures ───────────────────────────────────────────────────────────

const HOURS = Array.from({ length: 24 }, (_, i) => `${String(i).padStart(2, '0')}:00`)

/** api-gateway p95, with the model's expected range around it and one spike. */
const P95 = [186, 178, 172, 170, 181, 199, 233, 288, 344, 402, 318, 296, 281, 274, 269, 277, 301, 338, 391, 356, 292, 241, 212, 197]
const BAND_DATA: TimePoint[] = HOURS.map((t, i) => {
  const spike = i === 10 ? 980 : i === 11 ? 640 : 0
  const base = P95[i]
  return { t, p95: spike || base, lo: Math.round(base * 0.72), hi: Math.round(base * 1.28) }
})
/* The excursions are not listed: `BandChart` derives them from the data and
   the band, so the plot, the footer, the list and the sentence agree. */

/** Visitors on acme-storefront, this week and the one before it. */
const VISITORS: TimePoint[] = HOURS.map((t, i) => ({ t, visitors: Math.round(300 + Math.sin(i / 3.4) * 210 + (i > 8 && i < 20 ? 260 : 0)) }))
const VISITORS_PRIOR: TimePoint[] = HOURS.map((t, i) => ({ t, visitors: Math.round((300 + Math.sin(i / 3.4) * 190 + (i > 8 && i < 20 ? 220 : 0)) * 0.92) }))

/** Proxy status classes per five minutes, with the 10:44 upstream reset. */
const STATUS: TimePoint[] = Array.from({ length: 24 }, (_, i) => {
  const t = `10:${String(i * 5 % 60).padStart(2, '0')}`
  const bad = i === 8 || i === 9
  const total = 380 + ((i * 37) % 90)
  return { t: i < 12 ? t : `11:${String((i - 12) * 5).padStart(2, '0')}`, ok: bad ? Math.round(total * 0.82) : total - (i % 7 === 0 ? 4 : 0), e3: i % 5 === 0 ? 6 : 2, e4: i % 7 === 0 ? 4 : 1, e5: bad ? Math.round(total * 0.16) : 0 }
})
const STATUS_LAYERS: InkLayer[] = [
  { key: 'ok', name: '2xx' },
  { key: 'e3', name: '3xx' },
  { key: 'e4', name: '4xx' },
  { key: 'e5', name: '5xx', state: 'error' },
]

/** Latency grid: six buckets by twelve two-hour columns. */
const HEAT_COLS = Array.from({ length: 12 }, (_, i) => `${String(i * 2).padStart(2, '0')}:00`)
const HEAT_ROWS = [{ le: 25 }, { le: 50 }, { le: 100 }, { le: 250 }, { le: 500 }, { le: 2000, label: '500–2s' }]
const HEAT = HEAT_ROWS.map((_, r) => HEAT_COLS.map((_, c) => {
  const day = c > 3 && c < 10
  const second = r === 3 && day ? 320 : 0
  return Math.max(0, Math.round((r === 1 ? 900 : r === 2 ? 540 : r === 0 ? 210 : r === 3 ? 90 : r === 4 ? 22 : 4) * (day ? 1.6 : 0.5) + second + ((r * 7 + c * 13) % 30)))
}))
const HEAT_P95 = HEAT_COLS.map((_, c) => (c > 3 && c < 10 ? 320 : 120))

/** Weekly signup cohorts on acme-storefront. */
const COHORTS: Cohort[] = [
  { label: 'week of Jul 27', size: 412, values: [100, 44, 31, 26, 24, 22] },
  { label: 'week of Aug 03', size: 388, values: [100, 41, 29, 24, 23] },
  { label: 'week of Aug 10', size: 501, values: [100, 46, 33, 28] },
  { label: 'week of Aug 17', size: 470, values: [100, 39, 27] },
  { label: 'week of Aug 24', size: 522, values: [100, 43] },
  { label: 'week of Aug 31', size: 318, values: [100] },
]

const JOURNEY = {
  label: '/',
  count: 12_418,
  exits: 4820,
  children: [
    { label: '/pricing', count: 4210, exits: 2100, children: [
      { label: '/signup', count: 1490, exits: 402, children: [{ label: 'signup completed', count: 1088, exits: 0, note: 'goal' }] },
      { label: '/docs/quickstart', count: 620, exits: 380 },
    ] },
    { label: '/docs/quickstart', count: 2380, exits: 900, children: [
      { label: '/signup', count: 980, exits: 260, children: [{ label: 'signup completed', count: 720, exits: 0, note: 'goal' }] },
    ] },
    { label: '/blog/self-hosting', count: 1008, exits: 810 },
  ],
}

const SESSION: SessionEvent[] = [
  { at_ms: 0, kind: 'pageview', label: '/ · acme.sh' },
  { at_ms: 4200, kind: 'click', label: 'nav → pricing' },
  { at_ms: 4900, kind: 'pageview', label: '/pricing' },
  { at_ms: 21_400, kind: 'input', label: 'seats = 12' },
  { at_ms: 26_100, kind: 'network', label: 'POST /v1/quote · 200 · 188 ms' },
  { at_ms: 41_800, kind: 'click', label: 'start free trial' },
  { at_ms: 42_300, kind: 'pageview', label: '/signup' },
  { at_ms: 58_900, kind: 'network', label: 'POST /v1/signup · 502', state: 'error', note: 'api:8080 reset' },
  { at_ms: 59_100, kind: 'error', label: 'TypeError: cannot read "id" of undefined', state: 'error', note: 'signup.tsx:41' },
  { at_ms: 74_000, kind: 'click', label: 'retry' },
  { at_ms: 88_600, kind: 'custom', label: 'signup completed' },
]

const NODES: TopoNode[] = [
  { id: 'cp', label: 'hetzner-1', kind: 'control plane', state: 'ok', layer: 0, facts: '10.0.3.1 · 3 vCPU · mem 91%' },
  { id: 'w2', label: 'hetzner-2', kind: 'worker', state: 'ok', layer: 1, facts: '10.0.3.2 · 4 vCPU · 6 containers' },
  { id: 'w3', label: 'hetzner-3', kind: 'worker', state: 'error', layer: 1, facts: '10.0.3.3 · no heartbeat for 4m' },
]
const LINKS: TopoLink[] = [
  { from: 'cp', to: 'w2', kind: 'direct', label: 'UDP 51820' },
  { from: 'cp', to: 'w3', kind: 'relay', label: 'relay over 443', state: 'error' },
]

const UPTIME: StateSegment[] = [
  { state: 'ok', word: 'up', from: '00:00', seconds: 73_800 },
  { state: 'warn', word: 'degraded', from: '20:30', seconds: 600, note: 'p95 above 1s from two regions' },
  { state: 'error', word: 'down', from: '20:40', seconds: 1800, note: 'connection refused from all 3 regions · right after dep_91a' },
  { state: 'ok', word: 'up', from: '21:10', seconds: 10_200, note: 'recovered on dep_91b' },
]

/** The same 24 hours as `StatusStrip` sees them, for the pair demo. */
const BUCKETS = Array.from({ length: 48 }, (_, i) => ({
  start: `${String(Math.floor(i / 2)).padStart(2, '0')}:${i % 2 ? '30' : '00'}`,
  state: (i === 41 ? 'error' : i === 40 ? 'warn' : 'ok') as 'ok' | 'warn' | 'error',
  checks: 60, down: i === 41 ? 60 : 0,
}))

const PITR_FLOOR = '2026-08-30T20:33:00'
const PITR_CEIL = '2026-09-06T20:33:00'

// ── blocks ─────────────────────────────────────────────────────────────

export function DataVizBlocks2() {
  const [compare, setCompare] = useState(true)
  const [pos, setPos] = useState(42_300)
  const [pit, setPit] = useState('2026-09-06T18:33:00')
  return (
    <>
      <Block
        id="viz-band"
        title="BandChart — expected range"
        rule={<>
          <p>The model's expected range as a hatched ink band, the measured value as the ink line, and the stretch where the line leaves the band drawn <em>in the state tone</em> with a × at its peak.</p>
          <Rule state="ok">Colour the out-of-band segment: it <em>is</em> a state, so it earns the tone.</Rule>
          <Rule state="ok">Derive the excursions from the data and the band, so the plot, the footer, the list and the sentence cannot disagree.</Rule>
          <Rule state="ok">State them as facts in the footer, with the deploy beside: "3 anomalies · worst +42% at 14:20 ┆ dep_91a · band: rolling 7d".</Rule>
          <Rule state="error">A red area flood or a shaded rectangle behind the plot. A wash cannot be compared with anything and hides the band.</Rule>
          <Rule state="error">A × on the plot with no row under it: a glyph is invisible to a keyboard.</Rule>
        </>}
        api={`<BandChart data={points} actual="p95"
  band={{ lower: 'lo', upper: 'hi', label: 'expected' }}
  worse="up" errorAt={800} bandNote="rolling 7d, ±2σ"
  markers={[{ id: 'dep_91a', x: '10:00' }]}
  unit="ms" title="p95 · api-gateway" range="last 24h"
  verdict="Inside the band except 10:00–11:00." />

// legend: ⣿ expected range · ─ p95 · ─ anomaly
// table:  vs expected → "inside" · "+141% above"`}
      >
        <Demo label="p95 against the expected range · one excursion, above the 800ms budget">
          <BandChart data={BAND_DATA} actual="p95" actualName="p95" band={{ lower: 'lo', upper: 'hi', label: 'expected range' }}
            worse="up" errorAt={800} bandNote="rolling 7d, ±2σ"
            markers={[{ id: 'dep_91a', x: '10:00' }]} unit="ms"
            title="p95 latency · api-gateway" range="last 24h"
            verdict="Inside the band all day except a spike to 980ms an hour after dep_91a."
            footer={<><span>p95 / hour · 24h</span><span>· retention 30d</span><span>· ┆ deploy</span></>} />
        </Demo>
      </Block>

      <Block
        id="viz-compare"
        title="Compare — period over period"
        rule={<>
          <p><code>TimeChart</code> takes a <code>compare</code>: the prior period as a dotted thin ghost, and the delta in the generated legend with the baseline it is measured against.</p>
          <Rule state="ok">Compare the same length of window, or say nothing.</Rule>
          <Rule state="ok">The delta rides the legend, next to the label it belongs to.</Rule>
          <Rule state="error">"+9%" in a footer with no baseline. A delta with no baseline is a rumour.</Rule>
        </>}
        api={`<TimeChart data={T} series={[{ key: 'visitors', name: 'visitors' }]}
  compare={{ label: 'prior 24h', data: PRIOR }} />

// legend: ─ visitors 812 · ┈ prior 24h 748 · +9% vs prior 24h`}
      >
        <Demo label="visitors, with the previous 24 hours behind them">
          <label className="mb-2 inline-flex items-center gap-1.5 text-xs">
            <input type="checkbox" checked={compare} onChange={(e) => setCompare(e.target.checked)} className="accent-foreground" /> compare with the previous 24h
          </label>
          <TimeChart data={VISITORS} series={[{ key: 'visitors', name: 'visitors' }]}
            compare={compare ? { label: 'prior 24h', data: VISITORS_PRIOR } : undefined}
            unit="visitors" height={180} xInterval={5} markers={[{ id: 'dep_91a', x: '10:00' }]}
            title="visitors · acme-storefront" range="last 24h" verdict="The working-day shape held; the evening is ahead of the previous day." />
          <ChartFooter><span>visitors / hour · 24h</span><span>· retention 30d</span><span>· ┆ deploy</span></ChartFooter>
        </Demo>
      </Block>

      <Block
        id="viz-stacked"
        title="StackedInk — composition over time"
        rule={<>
          <p>Composition over time as stacked <strong>bars</strong> from zero: four layers, told apart by hatch, dot and solid at three greys. Status classes, tokens by model, log volume by level, backup size by source.</p>
          <Rule state="ok">State tone only on the layer that <em>is</em> a state (5xx is <code>error</code>).</Rule>
          <Rule state="ok">Four layers is the ceiling; a fifth is a table.</Rule>
          <Rule state="error">A stacked area. Its middle bands have no baseline, so they cannot be compared.</Rule>
        </>}
        api={`<StackedInk data={T} layers={[
  { key: 'ok', name: '2xx' }, { key: 'e3', name: '3xx' },
  { key: 'e4', name: '4xx' },
  { key: 'e5', name: '5xx', state: 'error' }]}
  unit="requests" title="requests by status class" … />`}
      >
        <Demo label="proxy requests by status class, five-minute buckets">
          <StackedInk data={STATUS} layers={STATUS_LAYERS} unit="requests" height={150} partial
            title="requests by status class" range="10:00 → 11:55"
            verdict="All 2xx until 10:40, when api:8080 reset connections and 16% of requests answered 502 for ten minutes."
            footer={<><span>requests / 5 min · 2h</span><span>· retention 30d</span><span>· ┆ dep_91a at 10:41</span></>} />
        </Demo>
      </Block>

      <Block
        id="viz-heatmap"
        title="LatencyHeatmap — time × bucket"
        rule={<>
          <p>Time on x, latency buckets on y, ink density is the count. It answers what a percentile line hides: two horizontal bands are two populations, not one slow tail.</p>
          <Rule state="ok">Five ink steps, the same ladder <code>CalendarHeatmap</code> uses. Zero is the empty step.</Rule>
          <Rule state="ok">Arrow keys walk the grid and announce the cell; the table view has every number.</Rule>
          <Rule state="error">A colour ramp. Density is how much, and ink already says how much.</Rule>
          <Rule state="error">A legend that says "less … more". A swatch with no number is a key the reader has to guess at.</Rule>
        </>}
        api={`<LatencyHeatmap columns={hours} rows={[{ le: 25 }, { le: 50 }…]}
  counts={grid} unit="ms" title="request latency"
  overlays={[{ name: 'p95', values: p95 }]} />`}
      >
        <Demo label="request latency on api-gateway, two-hour columns">
          <LatencyHeatmap columns={HEAT_COLS} rows={HEAT_ROWS} counts={HEAT} unit="ms"
            title="request latency · api-gateway" range="last 24h"
            verdict="A second band at 100–250ms appears between 08:00 and 18:00; the fast band never leaves."
            overlays={[{ name: 'p95', values: HEAT_P95 }]}
            footer={<><span>requests by latency bucket / 2h · 24h</span><span>· retention 30d</span></>} />
        </Demo>
      </Block>

      <Block
        id="viz-ladder"
        title="PercentileLadder — p50 · p95 · p99"
        rule={<>
          <p>The aside and tile form of a distribution: each statistic with its number, an ink bar on one shared scale from zero, and its own delta with the window that delta is measured against.</p>
          <Rule state="ok">Every delta carries its baseline, on its own rung.</Rule>
          <Rule state="ok">Tone only on a rung that is a state — a p99 over its budget.</Rule>
          <Rule state="error">Four numbers drawn as a chart. This is a small table of numbers, and says so.</Rule>
        </>}
        api={`<PercentileLadder unit="ms" label="checkout latency"
  rungs={[{ name: 'p50', value: 41 },
    { name: 'p99', value: 402, delta: '+18%',
      baseline: 'vs prior 24h', state: 'warn' }]}
  meta="41,208 requests · last 24h" />`}
      >
        <Demo label="checkout latency, with the previous day as the baseline">
          <PercentileLadder unit="ms" label="checkout latency"
            rungs={[
              { name: 'p50', value: 41, delta: '−4%', baseline: 'vs prior 24h' },
              { name: 'p95', value: 210, delta: '+6%', baseline: 'vs prior 24h' },
              { name: 'p99', value: 402, delta: '+18%', baseline: 'vs prior 24h', state: 'warn' },
              { name: 'max', value: 980, delta: '+141%', baseline: 'vs prior 24h', state: 'error' },
            ]}
            meta="41,208 requests · last 24h · budget p99 400 ms" />
        </Demo>
      </Block>

      <Block
        id="viz-cohort"
        title="CohortGrid — retention"
        rule={<>
          <p>Rows are cohorts, columns are periods, the cell is the share still active. It <em>is</em> a table, so it is a <code>&lt;table&gt;</code> with a row header per cohort and a column header per period.</p>
          <Rule state="ok">The number is in the cell; ink density is the second encoding, never the only one.</Rule>
          <Rule state="ok">A period a cohort has not lived through yet is empty (–), not zero.</Rule>
          <Rule state="error">A colour scale from red to green. Retention is how much, not how well.</Rule>
        </>}
        api={`<CohortGrid cohorts={weeks} periodLabel="week"
  label="signup retention"
  verdict="Week 1 holds 41% and flattens at 22%." />`}
      >
        <Demo label="signup retention on acme-storefront, by week">
          <CohortGrid cohorts={COHORTS} periodLabel="week" label="signup retention"
            verdict="Week 1 holds 39–46% and every cohort flattens near 22% from week 4."
            meta="– not reached yet · share of the cohort still active" />
        </Demo>
      </Block>

      <Block
        id="viz-paths"
        title="PathTree — journeys"
        rule={<>
          <p>Where visitors went next, as an indented tree: entry at the root, each step with its count, its share of the step above and how many left there.</p>
          <Rule state="ok">Collapsible branches, every toggle a real button — Tab and Enter walk the journey.</Rule>
          <Rule state="ok">Drop-off is the only thing on a row that takes a tone.</Rule>
          <Rule state="error">A Sankey. Its ribbons have no baseline, its labels overprint, and a keyboard cannot reach it.</Rule>
        </>}
        api={`<PathTree root={{ label: '/', count: 12418, exits: 4820,
  children: [{ label: '/pricing', count: 4210, exits: 2100 }] }}
  label="journeys from the landing page" dropAlert={50} />`}
      >
        <Demo label="journeys from the landing page">
          <PathTree root={JOURNEY} label="journeys from /" dropAlert={50}
            verdict="Half of the sessions that reach /pricing leave without opening /signup; the docs path converts better." />
        </Demo>
      </Block>

      <Block
        id="viz-session"
        title="SessionTimeline — a session"
        rule={<>
          <p>The timeline half of session replay: the events on a time axis with the player's position, and the same events as a synchronised list. The player itself (rrweb) is not ours — this is the contract around it.</p>
          <Rule state="ok">The list is the primary view: it carries the keyboard, Enter seeks.</Rule>
          <Rule state="ok">The scrubber is a native range input, so a keyboard already knows how to drive it.</Rule>
          <Rule state="error">Marks on the axis with a hover-only tooltip and no list.</Rule>
        </>}
        api={`<SessionTimeline duration_ms={92_000} events={events}
  position_ms={pos} onSeek={setPos}
  title="session ses_8c1" verdict="One 502 at 0:59." />`}
      >
        <Demo label="a session that hit a 502 on signup">
          <SessionTimeline duration_ms={92_000} events={SESSION} position_ms={pos} onSeek={setPos}
            title="session ses_8c1 · acme-storefront"
            verdict="Eleven events; the signup POST answered 502 at 0:59 and the retry at 1:14 worked."
            footer={<><span>offsets from the start of the session</span><span>· 92s recorded</span><span>· × is an error</span></>} />
        </Demo>
      </Block>

      <Block
        id="viz-usage"
        title="UsageBar — usage against an allowance"
        rule={<>
          <p>Ingest, disk, bandwidth, AI credits, seats. Text first: used, allowance and the plan word are a sentence above the bar, so the fact survives without the picture.</p>
          <Rule state="ok">The overage is hatched — beyond the line is a different fact, not more of the same.</Rule>
          <Rule state="ok">Mark where sampling started and say the figure past it is an estimate.</Rule>
          <Rule state="error">A bar pinned at 100% with the overage invisible.</Rule>
        </>}
        api={`<UsageBar label="events" used={8_420_000}
  allowance={10_000_000} unit="events"
  plan="Cloud Pro · 10M events / month"
  sampledFrom={7_500_000} resets="resets 1 Oct" />`}
      >
        <Demo label="inside the allowance, with sampling">
          <UsageBar label="events" used={8_420_000} allowance={10_000_000} unit="events"
            plan="Cloud Pro · 10M events / month" sampledFrom={7_500_000} resets="resets 1 Oct"
            action={<a href="#" onClick={(e) => e.preventDefault()} className="underline underline-offset-4">change plan</a>} />
        </Demo>
        <Demo label="over the allowance, and a size rather than a count">
          <div className="space-y-3">
            <UsageBar label="events" used={11_900_000} allowance={10_000_000} unit="events"
              plan="Cloud Pro · 10M events / month" resets="resets 1 Oct"
              action={<a href="#" onClick={(e) => e.preventDefault()} className="underline underline-offset-4">change plan</a>} />
            <UsageBar label="disk" used={68_000_000_000} allowance={80_000_000_000} format={(n) => fmtBytes(n)}
              plan="hetzner-1 · 80 GB volume" resets="oldest telemetry is deleted at 90%"
              action={<a href="#" onClick={(e) => e.preventDefault()} className="underline underline-offset-4">retention</a>} />
          </div>
        </Demo>
      </Block>

      <Block
        id="viz-gauge"
        title="Gauge — cpu, memory, disk"
        rule={<>
          <p>A machine's pressure as a <code>MetricGrid</code> tile: the figure, a horizontal ink bar from zero, threshold ticks that carry their own words, and the peak in the window.</p>
          <Rule state="ok">Horizontal and linear, so three tiles can be compared at a glance.</Rule>
          <Rule state="ok">An offline node keeps its tile and says "no samples"; it never disappears.</Rule>
          <Rule state="error">A radial gauge. Its scale has no baseline and its needle repeats the number.</Rule>
        </>}
        api={`<MetricGrid cols={3}>
  <Gauge label="memory" value={91} of="of 4 GB"
    peak={94} peakLabel="at 20:41"
    thresholds={[{ at: 80, state: 'warn', label: 'warn' }]} />
</MetricGrid>`}
      >
        <Demo label="hetzner-1 · memory is over the warn line">
          <MetricGrid cols={3}>
            <Gauge label="cpu" value={11} of="of 3 vCPU · load 0.8" peak={34} peakLabel="at 20:12" thresholds={[{ at: 80, state: 'warn', label: 'warn' }, { at: 95, state: 'error', label: 'saturated' }]} />
            <Gauge label="memory" value={91} of="of 4 GB" peak={94} peakLabel="at 20:41" thresholds={[{ at: 80, state: 'warn', label: 'warn' }, { at: 95, state: 'error', label: 'oom risk' }]} />
            <Gauge label="disk" value={41} of="of 80 GB" peak={41} peakLabel="so far today" thresholds={[{ at: 80, state: 'warn', label: 'warn' }, { at: 90, state: 'error', label: 'writes stop' }]} />
          </MetricGrid>
        </Demo>
        <Demo label="hetzner-3 · offline, so the tiles stay and say why">
          <MetricGrid cols={3}>
            <Gauge label="cpu" value={0} of="of 2 vCPU" idle="no samples since 20:37" thresholds={[{ at: 80, state: 'warn', label: 'warn' }]} />
            <Gauge label="memory" value={0} of="of 4 GB" idle="no samples since 20:37" thresholds={[{ at: 80, state: 'warn', label: 'warn' }]} />
            <Gauge label="disk" value={0} of="of 40 GB" idle="no samples since 20:37" thresholds={[{ at: 80, state: 'warn', label: 'warn' }]} />
          </MetricGrid>
        </Demo>
      </Block>

      <Block
        id="viz-state"
        title="StateTimeline — state over time"
        rule={<>
          <p>One horizontal bar of segments, each as wide as it was long: uptime up/degraded/down, a deployment's lifecycle, a node's heartbeat. The durations are the point.</p>
          <Rule state="ok">Use <code>StateTimeline</code> on the record of one resource, where "for how long" is the question.</Rule>
          <Rule state="ok">Use <code>StatusStrip</code> in a ledger, where equal buckets let a reader compare rows by shape.</Rule>
          <Rule state="error">Both, for the same window, on the same screen. A fact appears once.</Rule>
        </>}
        api={`<StateTimeline segments={[{ state: 'ok', word: 'up',
  from: '00:00', seconds: 73_800 }, …]}
  title="acme.sh checks" range="last 24h"
  verdict="Up except 30 minutes down at 20:40." />`}
      >
        <Demo label="the record: real transitions with their durations">
          <StateTimeline segments={UPTIME} title="api-gateway checks" range="last 24h"
            verdict="Up all day except ten minutes degraded and thirty minutes down from 20:40."
            footer={<><span>state changes · 24h</span><span>· retention 90d</span><span>· ← → reads a segment</span></>} />
        </Demo>
        <Demo label="the ledger: equal buckets, so rows can be compared">
          <StatusStrip buckets={BUCKETS} height={16} />
          <p className="mt-1 font-mono text-[10px] text-muted-foreground">one segment per 30 min · ● up · ◐ slow · × down · ← → reads a segment</p>
        </Demo>
      </Block>

      <Block
        id="viz-delta"
        title="DeltaTable — release comparison"
        rule={<>
          <p>Metric · before · after · delta, for two releases, two windows or two nodes. The deltas are <code>Num</code>s and stay ink.</p>
          <Rule state="ok">Tone appears only when a threshold makes the "after" value a state.</Rule>
          <Rule state="ok">Name both columns with what they are — a deploy tag, a window.</Rule>
          <Rule state="error">A red "+12%" on a metric with no budget. The reader cannot tell bad from bigger.</Rule>
        </>}
        api={`<DeltaTable before="dep_91a" after="dep_91b" rows={[
  { metric: 'p95 latency', before: 402, after: 188,
    unit: 'ms', better: 'lower' },
  { metric: '5xx rate', before: 0.02, after: 1.4, unit: '%',
    better: 'lower',
    threshold: { at: 1, state: 'error', label: 'budget 1%' } }]} />`}
      >
        <Demo label="dep_91b against dep_91a, one hour after each">
          <DeltaTable before="dep_91a" after="dep_91b"
            rows={[
              { metric: 'p95 latency', before: 402, after: 188, unit: 'ms', better: 'lower' },
              { metric: 'p99 latency', before: 980, after: 341, unit: 'ms', better: 'lower', threshold: { at: 400, state: 'warn', label: 'budget 400 ms' } },
              { metric: '5xx rate', before: 1.62, after: 0.02, unit: '%', better: 'lower', threshold: { at: 1, state: 'error', label: 'budget 1%' } },
              { metric: 'requests', before: 41_208, after: 42_960, unit: '', better: 'higher', note: 'same hour of day' },
              { metric: 'image size', before: 212, after: 246, unit: ' MB', better: 'lower', note: 'added the pdf renderer' },
            ]}
            meta="one hour of traffic after each deploy · thresholds are this project's budgets" />
        </Demo>
      </Block>

      <Block
        id="viz-window"
        title="WindowTimeline — backups and PITR"
        rule={<>
          <p>What a restore can actually reach: full backups as marks, the window the write-ahead log covers as a hatched band, and the restore target as a cursor on the same axis.</p>
          <Rule state="ok">Put it directly above the point-in-time field, so a refused second is visible before it is typed.</Rule>
          <Rule state="ok">Print the zone: a time with no zone beside it is a guess.</Rule>
          <Rule state="error">A restore form with no picture of the window it accepts.</Rule>
        </>}
        api={`<WindowTimeline from={FLOOR} to={CEIL}
  covered={[{ from: FLOOR, to: CEIL, label: 'WAL' }]}
  marks={[{ at: '2026-09-06T02:00:00', label: 'b_41' }]}
  cursor={{ at: pit, label: 'restore to' }} zone="UTC" />`}
      >
        <Demo label="acme-pg · seven days of WAL, six nightly backups, one failed">
          <div className="space-y-3">
            <WindowTimeline from={PITR_FLOOR} to={PITR_CEIL} zone="UTC"
              covered={[{ from: PITR_FLOOR, to: PITR_CEIL, label: 'WAL covers' }]}
              marks={[
                { at: '2026-08-31T02:00:00', label: 'b_36' },
                { at: '2026-09-01T02:00:00', label: 'b_37' },
                { at: '2026-09-03T02:00:00', label: 'b_38', state: 'error', note: 'upload timed out after 3 parts' },
                { at: '2026-09-04T02:00:00', label: 'b_39' },
                { at: '2026-09-05T02:00:00', label: 'b_40' },
                { at: '2026-09-06T18:33:00', label: 'b_41' },
              ]}
              cursor={{ at: pit, label: 'restore to' }}
              title="recoverable window · acme-pg"
              verdict="Any second in the last 7 days; the nightly on Sep 3 failed but WAL still covers it."
              footer={<><span>retention 7d</span><span>· nightly at 02:00, keeps 14</span><span>· ← → reads a backup</span></>} />
            <label className="flex flex-wrap items-center gap-2 font-mono text-[11px]">
              <span className="op-label">restore to</span>
              <input type="datetime-local" step={1} value={pit} min={PITR_FLOOR} max={PITR_CEIL} onChange={(e) => setPit(e.target.value)}
                className="h-8 border bg-background px-2 font-mono text-xs tabular-nums" />
              <span className="text-muted-foreground">times are UTC · writes after that second are not in the restored volume</span>
            </label>
          </div>
        </Demo>
      </Block>

      <Block
        id="viz-topology"
        title="Topology — cluster and service map"
        rule={<>
          <p>Nodes and links with state: a cluster (control plane, workers, their WireGuard reach) or a service map. Layered and deterministic — <code>layer</code> decides the row, array order decides the column.</p>
          <Rule state="ok">The list under the graph is the primary view and carries the keyboard and the words.</Rule>
          <Rule state="ok">Relay links are dashed: a different kind of reach, not a worse one.</Rule>
          <Rule state="error">A force layout. A graph that moves between reloads cannot be compared with yesterday's.</Rule>
        </>}
        api={`<Topology nodes={[{ id: 'cp', label: 'hetzner-1',
  kind: 'control plane', state: 'ok', layer: 0 }]}
  links={[{ from: 'cp', to: 'w3', kind: 'relay', state: 'error' }]}
  label="cluster" verdict="hetzner-3 has no heartbeat." />`}
      >
        <Demo label="the cluster: one control plane, two workers, one unreachable">
          <Topology nodes={NODES} links={LINKS} label="cluster" height={200}
            verdict="hetzner-3 has not sent a heartbeat for 4 minutes; its relay connection timed out."
            meta="3 nodes · 2 links · heartbeat every 15s, offline after 3 missed" />
        </Demo>
      </Block>
    </>
  )
}
