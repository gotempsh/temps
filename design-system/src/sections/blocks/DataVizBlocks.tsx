// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { ChartFooter, TimeChart, type Series, type TimePoint } from '@/components/op'

/**
 * The data-visualisation rules, drawn. Companion to `docs/data-viz.md`:
 * which chart answers which question, how two series are told apart without a
 * second hue, what the generated legend replaces, and what makes a chart
 * readable without the picture.
 */

/** Same shape as `OpComponents.tsx`'s Block; local so the two files stay independent. */
function Block({ id, title, rule, api, children }: { id: string; title: string; rule: ReactNode; api: string; children: ReactNode }) {
  return (
    <section id={id} className="scroll-mt-16 border-t pt-8">
      <div className="grid gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
        <div className="min-w-0">
          <h2 className="op-h2">{title}</h2>
          <div className="op-prose mt-2 space-y-2 text-sm text-muted-foreground">{rule}</div>
          <pre tabIndex={0} className="op-inset mt-4 overflow-auto border p-3 font-mono text-[11px] leading-5 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">{api}</pre>
        </div>
        <div className="min-w-0 space-y-4">{children}</div>
      </div>
    </section>
  )
}

function Demo({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col">
      <p className="op-label mb-2">{label}</p>
      <div className="flex-1">{children}</div>
    </div>
  )
}

/** question → chart. The table in `docs/data-viz.md` §1, as rows. */
const CHOICES: [string, string][] = [
  ['A value over time', 'TimeChart · one line per series, deploy markers'],
  ['Share of a whole', 'Breakdown · ink bar behind the row. Never a pie'],
  ['Ranked categories', 'Breakdown · sorted, honest "other" remainder'],
  ['Steps and drop-off', 'Funnel · bars by share of entrants'],
  ['From → to', 'Flow · ranked "A → B" with count and share'],
  ['A rate against a threshold', 'TimeChart · threshold line, series toned by its state'],
  ['Distribution', 'Histogram · with the percentile selector'],
  ['Availability by bucket', 'StatusStrip · one segment per bucket'],
  ['A score 0–100', 'ScoreRing · number in the middle, tone at the thresholds'],
  ['Activity by day', 'CalendarHeatmap · five ink intensities'],
  ['Nested timing', 'Waterfall · bars by offset and width'],
  ['By country', 'The ranked list first; GeoMap is its second view'],
]

const HOURS = Array.from({ length: 24 }, (_, i) => `${String(i).padStart(2, '0')}:00`)
const P50 = [41, 39, 38, 38, 40, 44, 51, 63, 74, 71, 66, 62, 60, 58, 57, 59, 64, 72, 81, 76, 63, 52, 46, 43]
const P99 = [186, 178, 172, 170, 181, 199, 233, 288, 344, 402, 318, 296, 281, 274, 269, 277, 301, 338, 391, 356, 292, 241, 212, 197]
const LATENCY: TimePoint[] = HOURS.map((t, i) => ({ t, p50: P50[i], p99: P99[i] }))

/** Declared once so the plot, the legend and the table cannot drift apart. */
const LATENCY_SERIES: Series[] = [
  { key: 'p50', name: 'p50' },
  { key: 'p99', name: 'p99', stroke: 'dashed', weight: 'thin' },
]

const VERDICT = 'Both lines follow the working day; p99 peaks at 402ms at 09:00, under the 500ms budget.'

/** The wrong half of the pair: two hues and a key, hand-drawn so the package cannot be used to make it. */
function TwoHuesChart() {
  const w = 560, h = 140, pad = 4
  const max = Math.max(...P99)
  const path = (vals: number[]) => vals.map((v, i) => `${i ? 'L' : 'M'}${(i / (vals.length - 1)) * (w - pad * 2) + pad},${h - pad - (v / max) * (h - pad * 2)}`).join(' ')
  return (
    <div className="space-y-1">
      <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="block h-[140px] w-full border" aria-hidden>
        <path d={path(P99)} fill="none" stroke="var(--chart-2)" strokeWidth={1.5} vectorEffect="non-scaling-stroke" />
        <path d={path(P50)} fill="none" stroke="var(--chart-1)" strokeWidth={1.5} vectorEffect="non-scaling-stroke" />
      </svg>
      <ChartFooter><span>latency / hour</span><span>· thick line p50, thin line p99</span></ChartFooter>
    </div>
  )
}

export function DataVizBlocks() {
  return (
    <>
      <Block
        id="viz-choice"
        title="Pick the chart from the question"
        rule={<>
          <p>Ask what the reader came to find out, then read the row. A chart chosen from habit answers a question nobody asked.</p>
          <p>Share of a whole is a ranked list with a bar, never a pie: the reader compares lengths from one baseline and can read the number.</p>
        </>}
        api={`// docs/data-viz.md §1
question → chart
"what is 5xx out of everything?" → Breakdown
"where do they leave?"           → Funnel
"is it over budget?"             → TimeChart + thresholds`}
      >
        <Demo label="question → chart">
          <div className="op-rows border">
            {CHOICES.map(([q, c]) => (
              <div key={q} className="flex flex-col gap-0.5 px-3 py-1.5 text-xs sm:flex-row sm:items-baseline sm:gap-3">
                <span className="w-full shrink-0 sm:w-52">{q}</span>
                <span className="min-w-0 font-mono text-[11px] text-muted-foreground">{c}</span>
              </div>
            ))}
          </div>
        </Demo>
      </Block>

      <Block
        id="viz-series"
        title="Two series, one ink"
        rule={<>
          <p>Series are told apart by pattern, never by hue: <code>stroke</code> is solid, dashed or dotted, <code>weight</code> is thin or regular, both defaulted by position.</p>
          <p>The legend is generated from <code>series</code> — the swatch is a sample of the real line and carries the value at the cursor — so it cannot drift from the plot. The <em>table</em> toggle beside it swaps the plot for the same buckets as rows, deploy markers included.</p>
          <p>More than four lines is small multiples or a table. Four dash patterns is the limit of what the eye separates.</p>
        </>}
        api={`<TimeChart data={LATENCY}
  series={[{ key: 'p50', name: 'p50' },
           { key: 'p99', name: 'p99',
             stroke: 'dashed', weight: 'thin' }]}
  unit="ms" title="latency" range="last 24h"
  verdict="p99 peaks at 402ms, under budget."
  markers={[{ id: 'dep_91a', x: '09:00' }]} />`}
      >
        <Demo label="the legend the chart draws; the table view is one click away">
          <TimeChart
            data={LATENCY}
            series={LATENCY_SERIES}
            unit="ms"
            height={176}
            xInterval={5}
            title="latency"
            range="last 24h"
            verdict={VERDICT}
            markers={[{ id: 'dep_91a', x: '09:00', at: '09:04', note: 'perf(router): cache edge lookups' }]}
          />
          <ChartFooter><span>latency / hour · 24h</span><span>· retention 30d</span><span>· ┆ deploy</span></ChartFooter>
        </Demo>
      </Block>

      <Block
        id="viz-legend"
        title="A legend does not license colour"
        rule={<>
          <p>Two hues and a sentence in the footer is the pattern this replaces. The hues carry the whole distinction, and the reader who needs them is the reader who did not read the key.</p>
          <p>Ink and a dash pattern carry the same distinction with no hue, and the generated legend puts the swatch beside the name so the match is made for the reader.</p>
        </>}
        api={`// wrong
series={[{ key: 'p50' }, { key: 'p99' }]} // chart-1 / chart-2
<ChartFooter>· thick p50, thin p99</ChartFooter>

// right
series={[{ key: 'p50', name: 'p50' },
         { key: 'p99', name: 'p99', stroke: 'dashed' }]}
// the chart draws the legend`}
      >
        <div className="grid gap-6 xl:grid-cols-2">
          <Demo label="× two hues, hand-written key">
            <TwoHuesChart />
          </Demo>
          <Demo label="● ink dashes, generated legend">
            <TimeChart data={LATENCY} series={LATENCY_SERIES} unit="ms" height={140} xInterval={5} table={false} title="latency" range="last 24h" verdict={VERDICT} />
            <ChartFooter><span>latency / hour · 24h</span><span>· retention 30d</span></ChartFooter>
          </Demo>
        </div>
      </Block>

      <Block
        id="viz-a11y"
        title="Readable without the picture"
        rule={<>
          <p>Every chart root is <code>role="img"</code> with an <code>aria-label</code> that is a sentence: what it is, over what range, and the verdict. Pass <code>title</code>, <code>range</code> and <code>verdict</code>.</p>
          <p>Every chart ships a table view of the same data. A chart with no table view is not shippable, and a readout reachable only by a pointer is invisible on a phone and to a keyboard.</p>
        </>}
        api={`<TimeChart title="p95 latency" range="last 24h"
  verdict="Flat at 50ms except one burst at 10:41." />

// role="img" aria-label=
// "p95 latency in ms, last 24h. Flat at 50ms
//  except one burst at 10:41. 24 points;
//  switch to the table view to read every value."`}
      >
        <Demo label="the sentence a screen reader gets">
          <p className="border p-3 font-mono text-[11px] leading-5">
            latency in ms, last 24h. {VERDICT} 24 points; switch to the table view to read every value.
          </p>
        </Demo>
        <Demo label="the same data as rows, at the same height">
          <TimeChart data={LATENCY.slice(6, 14)} series={LATENCY_SERIES} unit="ms" height={176} title="latency" range="06:00 to 13:00" verdict={VERDICT} legend />
          <ChartFooter><span>latency / hour · 06:00 → 13:00</span><span>· press "table" to read the numbers</span></ChartFooter>
        </Demo>
      </Block>
    </>
  )
}
