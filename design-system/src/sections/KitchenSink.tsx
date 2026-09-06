// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { Loader2, Rocket, Trash2 } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { EmptyPlaceholder } from '@/components/ui/empty-placeholder'
import { Input } from '@/components/ui/input'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Block, Demo, DocPage, Rule } from '@/components/op-doc'
import {
  ChartFooter, EchoDialog, Field, Kbd, Ledger, Metric, MetricGrid, Num, PageState, Phrase, Picker,
  Segmented, Settings, Status, StatusLine, TimeChart,
  type LedgerRow, type PickerOption, type State, type TimePoint,
} from '@/components/op'
import { ConsoleV1 } from '@/sections/ConsoleV1'

/* ────────────────────────────────────────────────────────────────────────
   /kitchen-sink — the v1 stress test. Every other reference page shows the
   system working; this one exists to break it on purpose: the real console
   squeezed into five widths, rows whose content is far past what a designer
   drew for, a status line past the character budget, 288 chart points, a
   form in a 360px box, and a greyed gallery of the old look that v1
   replaces. Nothing here is a pattern to copy except the failures it names.
   ──────────────────────────────────────────────────────────────────────── */

const TOC = [
  ['console', 'Console at every width'],
  ['long', 'Long everything'],
  ['status', 'Status line at the limit'],
  ['states', 'Every state'],
  ['dark', 'Dark mode'],
  ['density', 'Dense vs comfortable'],
  ['charts', 'Charts under stress'],
  ['forms', 'Forms under stress'],
  ['banned', 'Banned gallery'],
] as const

const STATES: State[] = ['ok', 'warn', 'error', 'idle', 'sampled']

/** 90 characters. A real project name nobody would type, but branch-derived preview envs do. */
const LONG_NAME = 'acme-storefront-checkout-experiments-preview-eu-central-1-blue-green-canary-rollout-x-2026'
/** Five subdomains in front of the apex. */
const LONG_DOMAIN = 'canary.blue.checkout.storefront.acme.example.com'
/** 200 characters of squashed-merge commit subject. */
const LONG_MESSAGE = 'Merge pull request #482 from acme/fix-checkout-idempotency-key: retry the payment intent once when the upstream returns 409 and stop double-charging carts that were resubmitted after a gateway timeout'

// ── §1 console at every width ──────────────────────────────────────────

const WIDTHS = [
  ['390', '390'],
  ['768', '768'],
  ['1024', '1024'],
  ['1280', '1280'],
  ['full', 'full'],
] as const
type WidthKey = (typeof WIDTHS)[number][0]

function ConsoleFrame() {
  const [width, setWidth] = useState<WidthKey>('1280')
  const [view, setView] = useState('api-gateway')
  return (
    <div className="min-w-0 space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <Segmented options={WIDTHS} value={width} onChange={setWidth} />
        <span className="font-mono text-[11px] text-muted-foreground">view: {view}</span>
        <button type="button" onClick={() => setView('api-gateway')} className="h-7 border px-2 text-[11px] hover:bg-muted">reset view</button>
      </div>
      {/* The frame scrolls; the page never does. A 1280 preset on a phone is a
          scrollable strip, not a broken layout. */}
      <div className="op-scroll-x min-w-0 overflow-x-auto border">
        <div style={{ width: width === 'full' ? '100%' : `${width}px` }} className="h-[640px] overflow-y-auto">
          <div className="flex min-h-full w-full">
            <ConsoleV1 view={view} go={setView} />
          </div>
        </div>
      </div>
    </div>
  )
}

// ── §2 pathological ledger ─────────────────────────────────────────────

const LONG_ROWS: LedgerRow[] = [
  {
    id: 'long-name',
    state: 'warn',
    mobile: <span className="block truncate font-medium">{LONG_NAME}</span>,
    cells: [
      <span className="font-medium">{LONG_NAME}</span>,
      <Status state="warn" label="error rate above 0.5%" />,
      <Num value={3482119640} />,
      <span className="text-muted-foreground">{LONG_MESSAGE}</span>,
    ],
  },
  {
    id: 'long-domain',
    state: 'ok',
    mobile: <span className="block truncate font-mono">{LONG_DOMAIN}</span>,
    cells: [
      <span className="font-mono">{LONG_DOMAIN}</span>,
      <Status state="ok" label="production" />,
      <Num value={128409} />,
      <span className="text-muted-foreground">chore(deps): bump undici to 6.21.1</span>,
    ],
  },
  {
    id: 'billing-worker',
    state: 'error',
    mobile: <span className="block truncate font-medium">billing-worker</span>,
    cells: [
      <span className="font-medium">billing-worker</span>,
      <Status state="error" label="failing health checks" />,
      <Num value={0} />,
      <span className="text-muted-foreground">fix(worker): drain the retry queue before exit</span>,
    ],
  },
  {
    id: 'acme-web',
    state: 'idle',
    mobile: <span className="block truncate font-medium">acme-web</span>,
    cells: [
      <span className="font-medium">acme-web</span>,
      <Status state="idle" label="not deployed" />,
      <Num value={null} />,
      <Num value={null} />,
    ],
  },
  {
    id: 'edge-metrics',
    state: 'sampled',
    mobile: <span className="block truncate font-medium">edge-metrics</span>,
    cells: [
      <span className="font-medium">edge-metrics</span>,
      <Status state="sampled" label="1 in 4 since 14:00" />,
      <Num value={9120448} />,
      <span className="text-muted-foreground">feat(edge): forward span links to the collector</span>,
    ],
  },
]

// ── §6 density ─────────────────────────────────────────────────────────

/** Reads the resolved `--row-h` off its own box, so the label is measured, not asserted. */
function RowHeight() {
  const ref = useRef<HTMLSpanElement>(null)
  const [v, setV] = useState('')
  useEffect(() => {
    if (ref.current) setV(getComputedStyle(ref.current).getPropertyValue('--row-h').trim())
  }, [])
  return <span ref={ref} className="op-label">--row-h: <span className="font-mono normal-case">{v || 'measuring…'}</span></span>
}

const DENSITY_ROWS: LedgerRow[] = [
  { id: 'api-gateway', state: 'warn', mobile: <span className="block truncate">api-gateway</span>, cells: [<span className="font-medium">api-gateway</span>, <Status state="warn" label="0.61% errors" />, <Num value={30800} />] },
  { id: 'docs', state: 'ok', mobile: <span className="block truncate">docs</span>, cells: [<span className="font-medium">docs</span>, <Status state="ok" label="production" />, <Num value={2210} />] },
  { id: 'checkout', state: 'ok', mobile: <span className="block truncate">checkout</span>, cells: [<span className="font-medium">checkout</span>, <Status state="ok" label="production" />, <Num value={18422} />] },
  { id: 'billing-worker', state: 'error', mobile: <span className="block truncate">billing-worker</span>, cells: [<span className="font-medium">billing-worker</span>, <Status state="error" label="failing" />, <Num value={0} />] },
]

function MiniLedger({ dense }: { dense: boolean }) {
  const [q, setQ] = useState('')
  return (
    <Ledger
      status={<StatusLine sticky={false} state="error" className="mx-0 px-0 sm:mx-0 sm:px-0">billing-worker is failing health checks.</StatusLine>}
      columns={['project', 'status', 'requests 24h']}
      grid="minmax(0,1.4fr) minmax(0,1fr) 120px"
      rows={DENSITY_ROWS.filter((r) => r.id.includes(q))}
      total={DENSITY_ROWS.length}
      filter={q}
      onFilter={setQ}
      placeholder="filter projects"
      dense={dense}
    />
  )
}

// ── §7 charts ──────────────────────────────────────────────────────────

/** 288 five-minute buckets over 24h: requests and 5xx, both real shapes. */
const DAY: TimePoint[] = Array.from({ length: 288 }, (_, i) => {
  const mins = i * 5
  const t = `${String(Math.floor(mins / 60)).padStart(2, '0')}:${String(mins % 60).padStart(2, '0')}`
  const hour = mins / 60
  const diurnal = 900 + 1400 * Math.max(0, Math.sin(((hour - 5) / 24) * Math.PI * 2))
  const deployDip = hour > 14 && hour < 14.6 ? -420 : 0
  return {
    t,
    req: Math.round(diurnal + deployDip + 120 * Math.sin(i / 3)),
    err: Math.round(Math.max(0, 4 + 26 * Math.exp(-Math.abs(hour - 14.3) * 3) + 3 * Math.sin(i / 7))),
  }
})

/** Six deploys inside forty minutes: every marker label lands on top of the next. */
const CLUSTER = [
  { id: 'dep_91a', x: '14:00' },
  { id: 'dep_91b', x: '14:05' },
  { id: 'dep_91c', x: '14:15' },
  { id: 'dep_91d', x: '14:20' },
  { id: 'dep_91e', x: '14:30' },
  { id: 'dep_91f', x: '14:40' },
]

const FLAT: TimePoint[] = Array.from({ length: 96 }, (_, i) => ({
  t: `${String(Math.floor((i * 15) / 60)).padStart(2, '0')}:${String((i * 15) % 60).padStart(2, '0')}`,
  hits: 0,
}))

const SPIKE: TimePoint[] = Array.from({ length: 96 }, (_, i) => ({
  t: `${String(Math.floor((i * 15) / 60)).padStart(2, '0')}:${String((i * 15) % 60).padStart(2, '0')}`,
  p95: i === 61 ? 8420 : 96 + (i % 5) * 4,
}))

// ── §8 forms ───────────────────────────────────────────────────────────

const REGIONS: PickerOption[] = [
  { value: 'fsn1', label: 'fsn1 · Falkenstein', group: 'europe', meta: 'primary', state: 'ok' },
  { value: 'nbg1', label: 'nbg1 · Nuremberg', group: 'europe', meta: '12 ms' },
  { value: 'hki1', label: 'hki1 · Helsinki', group: 'europe', meta: '31 ms' },
  { value: 'ash', label: 'ash · Ashburn VA', group: 'north america', meta: '94 ms' },
  { value: 'hil', label: 'hil · Hillsboro OR', group: 'north america', meta: '148 ms' },
  { value: 'sin', label: 'sin · Singapore', group: 'asia', meta: 'no capacity', state: 'warn', disabled: true },
]

const LONG_HELP = 'Requests that take longer than this are terminated and counted as 504s in error tracking. The proxy applies it per attempt, so a request that is retried twice can occupy a worker for three times this value; keep it below the upstream load balancer idle timeout or the balancer will close the connection first and you will see 502s instead of 504s in the logs.'

function StressSettings({ id }: { id: string }) {
  const [region, setRegion] = useState<string | null>('fsn1')
  const [name, setName] = useState('acme prod!')
  const [dirty, setDirty] = useState(true)
  const invalid = !/^[a-z0-9-]+$/.test(name)
  return (
    <Settings
      title={`environment · ${id}`}
      meta={LONG_DOMAIN}
      status={<StatusLine sticky={false} state="warn" className="mx-0 px-0 sm:mx-0 sm:px-0">The service name is not a valid slug.</StatusLine>}
      dirty={dirty}
      onSave={() => setDirty(false)}
      sections={[
        {
          title: 'identity',
          body: (
            <>
              <Field label="service name" help={invalid ? 'lowercase letters, digits and hyphens only — "acme prod!" has a space and a "!"' : 'used in the container name and the default subdomain'}>
                <Input value={name} aria-invalid={invalid} onChange={(e) => { setName(e.target.value); setDirty(true) }} className="h-8 text-xs" />
              </Field>
              <Field label="region" help="moving a running service between regions recreates its volumes">
                <Picker value={region} onChange={(v) => { setRegion(v); setDirty(true) }} options={REGIONS} allowCustom="use region" />
              </Field>
              <Field label="primary domain">
                <Input defaultValue={LONG_DOMAIN} className="h-8 font-mono text-xs" />
              </Field>
              <Field label="git branch" help="deploys on every push to this branch">
                <Input defaultValue="release/2026.09-checkout-idempotency" className="h-8 font-mono text-xs" />
              </Field>
            </>
          ),
        },
        {
          title: 'runtime',
          body: (
            <>
              <Field label="request timeout" help={LONG_HELP}>
                <Input defaultValue="30s" className="h-8 font-mono text-xs" />
              </Field>
              <Field label="replicas" help="rolling deploys need at least 2">
                <Input defaultValue="3" className="h-8 font-mono text-xs" />
              </Field>
              <Field label="memory limit">
                <Input defaultValue="512Mi" className="h-8 font-mono text-xs" />
              </Field>
            </>
          ),
        },
        {
          title: 'telemetry',
          body: (
            <>
              <Field label="sample rate" help="head sampling; the console says when a window was sampled">
                <Input defaultValue="1.0" className="h-8 font-mono text-xs" />
              </Field>
              <Field label="retention" help="self-hosted keeps everything the disk holds">
                <Input defaultValue="30d" className="h-8 font-mono text-xs" />
              </Field>
            </>
          ),
        },
      ]}
      danger={
        <EchoDialog
          trigger={<Button size="sm" variant="outline" className="h-8 text-xs"><Trash2 /> delete environment</Button>}
          echo={`$ bunx @temps-sdk/cli env delete ${id} --project acme-storefront`}
          title="Delete environment"
          description="Removes the containers, the volumes and the DNS record. Backups are kept for 30 days."
          confirmWord={id}
          steps={['stop containers', 'release domain', 'detach volumes', 'record audit entry']}
          onDone={() => undefined}
          destructive
        />
      }
    />
  )
}

// ── page ───────────────────────────────────────────────────────────────

function Muted({ children }: { children: ReactNode }) {
  return <p className="text-[11px] text-muted-foreground">{children}</p>
}

export function KitchenSinkPage() {
  const [q, setQ] = useState('')
  const [retrying, setRetrying] = useState(false)
  const [pickerEmpty, setPickerEmpty] = useState<string | null>(null)
  const [hot, setHot] = useState<string | null>(null)
  const [tab, setTab] = useState('overview')

  const rows = LONG_ROWS.filter((r) => r.id.includes(q))

  return (
    <DocPage
      eyebrow="kitchen sink · break it here"
      intro={
        <>
          The stress test. Everything on this page is deliberately past what the components were drawn for: a 90-character
          project name, a 200-character deploy message, 288 chart points, six deploy markers inside forty minutes, a form in a
          360px box, and the real console from <Link to="/v1" className="underline underline-offset-4">/v1</Link> squeezed into five widths.
          What holds is the system; what breaks is written down next to it. The greyed gallery at the bottom is the only place
          the old look (cards, badges, pill tabs, spinners) is allowed to appear.
        </>
      }
      toc={TOC}
    >
      <Block
        id="console"
        title="The console at every width"
        rule={
          <>
            <p>The whole v1 shell, embedded exactly as the landing page embeds it. Pick a width and drive it: navigate, open the palette with <Kbd keys={['⌘', 'K']} />, toggle density with <Kbd keys="d" />.</p>
            <p>The frame scrolls horizontally, so a 1280 preset on a phone is a scrollable strip rather than a page that overflows.</p>
          </>
        }
        api={`<ConsoleV1 view={view} go={setView} />
// density is remembered in localStorage:
//   temps.ds.v1.density = 'dense' | 'comfortable'`}
      >
        <Demo label="resizable frame · 640px tall, scrolls in both axes" className="px-0 sm:px-0">
          <ConsoleFrame />
        </Demo>
        <Rule state="ok">The console remembers density in <span className="font-mono">localStorage</span> under <span className="font-mono">temps.ds.v1.density</span>. Toggling it here changes it for <span className="font-mono">/v1</span> too, and it survives a reload.</Rule>
        <Rule state="error">
          The width presets change the console's available width, not the viewport. v1's responsive rules are media queries
          (<span className="font-mono">lg:</span> sidebar, <span className="font-mono">md:</span> ledger columns), so the 390 preset still renders the desktop layout
          inside a 390px box. To see the phone layout, narrow the browser window itself. Container queries would fix this; only <span className="font-mono">Field</span> uses them today.
        </Rule>
        <Rule state="error">
          Keyboard handlers in <span className="font-mono">Ledger</span>, <span className="font-mono">Detail</span> and the console shell are bound to <span className="font-mono">window</span>, not to the
          component. With the console embedded on this page, <Kbd keys="d" /> toggles its density from anywhere on the page, and <Kbd keys="j" />/<Kbd keys="k" /> move the cursor in
          every ledger below at once.
        </Rule>
      </Block>

      <Block
        id="long"
        title="Long everything"
        rule={
          <>
            <p>Five rows, five states, and one pathological value each: a 90-character name, five subdomains, a 200-character deploy message, a number in the billions, and nothing at all.</p>
            <p>On desktop nothing wraps: every cell is one line, truncated at the column edge. On a phone the row folds to the <span className="font-mono">mobile</span> node and the name truncates there.</p>
          </>
        }
        api={`{ id, state, cells: [...], mobile: <span className="block truncate">…</span> }
grid="minmax(0,1.6fr) 180px 120px minmax(0,2fr)"`}
      >
        <Demo label="ledger · pathological rows">
          <Ledger
            title="projects"
            meta="acme · 5 of 5"
            status={<StatusLine sticky={false} state="error" more={{ label: '+2 warnings' }} className="mx-0 px-0 sm:mx-0 sm:px-0">billing-worker is failing health checks.</StatusLine>}
            columns={['project', 'status', 'requests 24h', 'last deploy']}
            grid="minmax(0,1.6fr) 180px 120px minmax(0,2fr)"
            rows={rows}
            total={LONG_ROWS.length}
            filter={q}
            onFilter={setQ}
            placeholder="filter projects"
            hint="needs attention first, then last deploy"
            action={<Button size="sm" className="op-primary h-8 text-xs"><Rocket /> deploy</Button>}
            dense={false}
          />
        </Demo>
        <Muted>Name: {LONG_NAME.length} characters. Deploy message: {LONG_MESSAGE.length} characters. Requests: 3,482,119,640. The idle row has nothing to show and renders – twice, never 0.</Muted>
        <Rule state="ok">Fixed row height on desktop holds: the long name, the long message and the billions all truncate rather than pushing the row to two lines.</Rule>
        <Rule state="error">
          Truncation on the phone fold is the caller's job, not the template's. <span className="font-mono">Ledger</span> wraps <span className="font-mono">mobile</span> in <span className="font-mono">min-w-0</span> only, so a
          <span className="font-mono"> mobile</span> node without <span className="font-mono">truncate</span> wraps a 90-character name to three lines and the row grows. Every row here passes <span className="font-mono">block truncate</span>.
        </Rule>
      </Block>

      <Block
        id="status"
        title="Status line at the limit"
        rule={
          <>
            <p>One glyph, one sentence, at most one link, plus <span className="font-mono">more</span> on the right. The budget is about 60 characters, because the line must survive a phone without truncating the verb.</p>
            <p>The last example is the failure case: past the budget, the line truncates on desktop and the reader loses the half that said what to do.</p>
          </>
        }
        api={`<StatusLine state="warn" more={{ label: '+2 warnings' }}>
  <Phrase onClick={open}>billing-worker</Phrase> is failing.
</StatusLine>`}
      >
        <Demo label="59 characters · the ceiling">
          <StatusLine sticky={false} state="warn">Two databases are past their retention windows since 09:14.</StatusLine>
        </Demo>
        <Demo label="with a link on the actionable thing">
          <StatusLine sticky={false} state="error"><Phrase>billing-worker</Phrase> is failing health checks.</StatusLine>
        </Demo>
        <Demo label="with more · the rest of the page's problems collapse here">
          <StatusLine sticky={false} state="error" more={{ label: '+2 warnings' }}><Phrase>billing-worker</Phrase> is failing health checks.</StatusLine>
        </Demo>
        <Demo label="failure case · far past the budget">
          <StatusLine sticky={false} state="warn" more={{ label: '+4 warnings' }}>
            The eu-central-1 worker pool is running one replica short because the autoscaler could not acquire capacity in fsn1, and the retry queue has been growing since 09:14.
          </StatusLine>
        </Demo>
        <Rule state="error">
          That last line is wrong, not a bug: on desktop it truncates and the reader never reaches "the retry queue has been
          growing". Below <span className="font-mono">sm</span> it wraps instead and pushes the page down. Two clauses means two facts; the second belongs in the page.
        </Rule>
      </Block>

      <Block
        id="states"
        title="Every state of every component"
        rule={
          <>
            <p>Five statuses, five metric tiles, four page states, three button states, three picker states, and the key badge on both platforms.</p>
            <p>Anything that renders nothing here is a bug: every non-happy state has a component.</p>
          </>
        }
        api={`type State = 'ok' | 'warn' | 'error' | 'idle' | 'sampled'
<PageState state="unconfigured" … />`}
      >
        <Demo label="Status ×5">
          <div className="flex flex-wrap gap-x-6 gap-y-2 text-sm">{STATES.map((s) => <Status key={s} state={s} label={s} />)}</div>
        </Demo>
        <Demo label="Metric ×5 · every tile names its baseline" className="px-0 sm:px-0">
          <MetricGrid cols={3}>
            <Metric label="uptime" value="99.98" unit="%" baseline="30d window" state="ok" />
            <Metric label="error rate" value="0.61" unit="%" delta="+0.2pt" baseline="since dep_91a" state="warn" />
            <Metric label="failed deploys" value={3} baseline="last 24h" state="error" />
            <Metric label="preview envs" value={0} baseline="none created yet" state="idle" />
            <Metric label="spans kept" value="1 in 4" baseline="since 14:00, head sampled" state="sampled" />
            <Metric label="p95 latency" value={8420} unit="ms" delta="+8.2s" baseline="vs the same hour yesterday" state="error" />
          </MetricGrid>
        </Demo>
        <Rule state="error">
          <span className="font-mono">Metric</span> only distinguishes three of the five states: <span className="font-mono">ok</span>, <span className="font-mono">idle</span> and <span className="font-mono">sampled</span> all render the
          baseline in muted, with no glyph. A sampled tile is indistinguishable from a healthy one, which is exactly the promise the ◌ state exists to keep.
        </Rule>
        <Demo label="PageState ×4" className="px-0 sm:px-0">
          <div className="grid gap-4 lg:grid-cols-2">
            <PageState state="loading" rows={4} />
            <PageState state="empty" title="No deploys yet" reason="This environment was created 4 minutes ago and has never been deployed." next={<Button size="sm" className="op-primary h-8 text-xs"><Rocket /> deploy main</Button>} />
            <PageState
              state="unconfigured"
              title="Session replay"
              missing="an S3 bucket for replay chunks"
              settingsHref="/settings"
              settingsLabel="configure storage"
              example={<div className="op-rows border text-xs"><div className="op-row flex items-center justify-between"><span className="font-mono">/checkout</span><span className="font-mono">2m 14s</span></div><div className="op-row flex items-center justify-between"><span className="font-mono">/cart</span><span className="font-mono">0m 41s</span></div></div>}
            />
            <PageState
              state="error"
              title="Could not load deploys"
              message="504 Gateway Timeout after 30s"
              resource="GET /api/projects/acme-storefront/deployments"
              retrying={retrying}
              onRetry={() => { setRetrying(true); window.setTimeout(() => setRetrying(false), 1200) }}
            />
          </div>
        </Demo>
        <Demo label="Button · pressed, disabled, pending">
          <div className="flex flex-wrap items-center gap-3">
            <Button size="sm" className="op-primary op-pressed h-8 text-xs"><Rocket /> deploy <Kbd keys={['⌘', '⏎']} className="ml-1 opacity-70" /></Button>
            <Button size="sm" className="op-primary h-8 text-xs" disabled><Rocket /> deploy</Button>
            <Button size="sm" className="op-primary h-8 text-xs" disabled><Loader2 className="animate-spin" /> deploying dep_91f…</Button>
          </div>
        </Demo>
        <Muted>A spinner is allowed here and nowhere else: inside a button it means "this action is running", not "this page is loading".</Muted>
        <Demo label="Picker · loading, error, empty">
          <div className="grid gap-3 md:grid-cols-3">
            <Picker value={null} onChange={() => undefined} options={[]} loading="branches from github.com/acme/storefront" placeholder="choose a branch" />
            <Picker value={null} onChange={() => undefined} options={[]} error="remote: Invalid username or password. fatal: Authentication failed for 'https://github.com/acme/storefront'" onRetry={() => undefined} allowCustom="use branch" placeholder="choose a branch" />
            <Picker value={pickerEmpty} onChange={setPickerEmpty} options={[]} placeholder="no images built yet" />
          </div>
        </Demo>
        <Demo label="Kbd · both platforms, forced">
          <div className="flex flex-wrap items-center gap-6 text-xs">
            <span className="flex items-center gap-2">macOS <Kbd keys={['⌘', 'K']} /> <Kbd keys={['⌘', 'S']} /> <Kbd keys="⏎" /></span>
            <span className="flex items-center gap-2">everything else <Kbd keys={['Ctrl', 'K']} /> <Kbd keys={['Ctrl', 'S']} /> <Kbd keys="Enter" /></span>
          </div>
        </Demo>
        <Muted>Both rows are written out literally. In real use <span className="font-mono">Kbd</span> takes <span className="font-mono">'⌘'</span> and swaps it for <span className="font-mono">Ctrl</span> off macOS, so a page can only ever show one of these.</Muted>
      </Block>

      <Block
        id="dark"
        title="Dark mode"
        rule={
          <>
            <p>The ink skin inverts through <span className="font-mono">.dark .operator.ink</span> — the <span className="font-mono">dark</span> class must be an <em>ancestor</em>, not on the skin element. There is no <span className="font-mono">.operator.ink.dark</span> rule in globals.css.</p>
            <p>The same ledger, twice. Nothing about the markup changes; only the token block resolves differently.</p>
          </>
        }
        api={`<div className="dark">
  <div className="operator ink v1 bg-background text-foreground">…</div>
</div>`}
      >
        <Demo label="light · dark" className="px-0 sm:px-0">
          <div className="grid min-w-0 gap-4 lg:grid-cols-2">
            <div className="min-w-0 border p-3"><p className="op-label mb-3">light</p><MiniLedger dense={false} /></div>
            <div className="dark min-w-0">
              <div className="operator ink v1 min-w-0 border bg-background p-3 text-foreground">
                <p className="op-label mb-3">dark</p>
                <MiniLedger dense={false} />
              </div>
            </div>
          </div>
        </Demo>
        <Demo label="the five glyph colours on dark" className="px-0 sm:px-0">
          <div className="dark min-w-0">
            <div className="operator ink v1 flex flex-wrap gap-x-6 gap-y-2 border bg-background p-4 text-sm text-foreground">
              {STATES.map((s) => <Status key={s} state={s} label={s} />)}
            </div>
          </div>
        </Demo>
        <Rule state="ok">Status colours are the same three hues in both skins; only paper and ink swap. <span className="font-mono">--op-inset</span> goes to pure black on dark so log panes read as recessed.</Rule>
        <Rule state="error">
          Dark mode has to be re-declared on every nested block, because the skin classes are re-applied inside the <span className="font-mono">dark</span> wrapper. A page that only sets
          <span className="font-mono"> dark</span> on <span className="font-mono">html</span> is fine; a page that mixes both, like this one, will keep tripping over it.
        </Rule>
      </Block>

      <Block
        id="density"
        title="Dense vs comfortable"
        rule={
          <>
            <p>Density is one attribute on the shell: <span className="font-mono">data-density</span> on the element that also carries <span className="font-mono">operator ink v1</span>. It moves <span className="font-mono">--row-h</span> and <span className="font-mono">--cell-px</span>, nothing else.</p>
            <p>The same four rows, twice. The row height below is measured off the live box, not written down.</p>
          </>
        }
        api={`<div data-density="dense" className="operator ink v1">
  --row-h: 1.75rem   (comfortable: 2.25rem)`}
      >
        <Demo label="comfortable · dense" className="px-0 sm:px-0">
          <div className="grid min-w-0 gap-4 lg:grid-cols-2">
            <div data-density="comfortable" className="operator ink v1 min-w-0 border p-3">
              <p className="mb-3 flex flex-wrap items-baseline gap-2"><span className="op-label">comfortable</span><RowHeight /></p>
              <MiniLedger dense={false} />
            </div>
            <div data-density="dense" className="operator ink v1 min-w-0 border p-3">
              <p className="mb-3 flex flex-wrap items-baseline gap-2"><span className="op-label">dense</span><RowHeight /></p>
              <MiniLedger dense />
            </div>
          </div>
        </Demo>
        <Rule state="ok">Below <span className="font-mono">md</span> the rule <span className="font-mono">height: auto; min-height: var(--row-h)</span> takes over, so density stops clipping content on phones and becomes padding only.</Rule>
        <Rule state="error">
          Density travels two ways at once: the CSS variable comes from <span className="font-mono">data-density</span>, but <span className="font-mono">Ledger</span> also takes a <span className="font-mono">dense</span> boolean prop that
          changes row padding. They can disagree, and nothing catches it — a <span className="font-mono">Ledger dense</span> inside a <span className="font-mono">data-density="comfortable"</span> shell renders a third layout.
        </Rule>
      </Block>

      <Block
        id="charts"
        title="Charts under stress"
        rule={
          <>
            <p>288 five-minute buckets over 24 hours, two series, six deploy markers inside forty minutes, and a three-hour sampled band. Then the two degenerate cases: a series that is flat zero, and a series that is flat except for one spike.</p>
            <p>Lines only. No fills, no animation, and the readout above the plot means the chart still answers a question on a touch screen where there is no hover.</p>
          </>
        }
        api={`<TimeChart data={288 points} series={[req, err]}
  markers={six} hot={hot} onHot={setHot}
  sampled={{ from, to, label }} />`}
      >
        <Demo label="288 points · two series · six markers · sampled band" className="px-0 sm:px-0">
          <TimeChart
            data={DAY}
            series={[{ key: 'req', name: 'requests' }, { key: 'err', name: '5xx' }]}
            markers={CLUSTER}
            hot={hot}
            onHot={setHot}
            sampled={{ from: '18:00', to: '21:00', label: 'sampled 1 in 4' }}
            height={200}
            unit="req/5m"
          />
          <ChartFooter>requests and 5xx · 5-minute buckets · {DAY.length} points · deploys dotted · retention 30d on self-hosted</ChartFooter>
        </Demo>
        <Muted>hot marker: {hot ?? 'none'} — hovering a marker here is what links a chart to a deploy row in the real console.</Muted>
        <Demo label="flat zero · a service that took no traffic all day" className="px-0 sm:px-0">
          <TimeChart data={FLAT} series={[{ key: 'hits', name: 'hits' }]} yTicks={[0, 1]} height={120} unit="hits" />
          <ChartFooter>zero is a value, not missing data · the line sits on the axis and the readout says 0</ChartFooter>
        </Demo>
        <Demo label="single spike · one 8.4s request in a flat day" className="px-0 sm:px-0">
          <TimeChart data={SPIKE} series={[{ key: 'p95', name: 'p95' }]} height={120} unit="ms" />
          <ChartFooter>the spike sets the y scale; the baseline flattens against the axis, which is the honest rendering</ChartFooter>
        </Demo>
        <Rule state="ok">No fills, no animation, no gradient. The readout above the plot shows the latest value until a point is hovered, so touch users get the number without a tooltip.</Rule>
        <Rule state="error">
          Six markers inside forty minutes overprint: <span className="font-mono">ReferenceLine</span> labels are placed <span className="font-mono">insideTopLeft</span> with no collision handling, so
          <span className="font-mono"> dep_91a</span>…<span className="font-mono">dep_91f</span> stack on top of each other and only the last one is readable. Clustered deploys are the normal case on a busy afternoon.
        </Rule>
        <Rule state="error">
          At 288 points the x-axis interval is derived as <span className="font-mono">data.length / 4</span>, which puts a tick every ~6 hours — fine — but the tooltip
          still walks every point on mouse move, and the marker labels are laid out per render. Below ~500px wide the axis labels collide with the sampled band's label.
        </Rule>
      </Block>

      <Block
        id="forms"
        title="Forms under stress"
        rule={
          <>
            <p>Three sections, nine fields, one Picker, one 300-character help text, one invalid input. Rendered twice: in a 360px box and at full width.</p>
            <p><span className="font-mono">Field</span> is a container query, so the narrow copy stacks label over control and the wide copy puts them on one row — with no viewport breakpoint involved and no props passed.</p>
          </>
        }
        api={`<Field label="…" help="…">   // @md:grid-cols-[160px_1fr]
<Settings sections={[…]} dirty onSave danger={<EchoDialog …/>} />`}
      >
        <Demo label="360px box · full width" className="px-0 sm:px-0">
          <div className="grid min-w-0 items-start gap-6 xl:grid-cols-[360px_minmax(0,1fr)]">
            <div className="w-full min-w-0 overflow-hidden border p-4 xl:w-[360px]">
              <p className="op-label mb-3">360px</p>
              <StressSettings id="preview-4821" />
            </div>
            <div className="min-w-0 border p-4">
              <p className="op-label mb-3">full width</p>
              <StressSettings id="production" />
            </div>
          </div>
        </Demo>
        <Rule state="ok">Same component, same props, two layouts. The container query is what makes a form reusable in a side panel and on a page without a variant prop.</Rule>
        <Rule state="error">
          <span className="font-mono">Settings</span>' sticky save bar is <span className="font-mono">-mx-4 sm:-mx-6</span>, which assumes it sits inside page padding. In a 360px box it bleeds past
          the container edge and is clipped by <span className="font-mono">overflow-hidden</span> here. The negative margin should come from the page, not the template.
        </Rule>
        <Rule state="error">
          The invalid field is marked <span className="font-mono">aria-invalid</span> and explains itself in <span className="font-mono">help</span>, but <span className="font-mono">Field</span> has no error slot: the message is styled as
          help text (muted, 11px) and reads as advice rather than a failure. Save stays enabled.
        </Rule>
      </Block>

      <Block
        id="banned"
        title="Banned gallery"
        rule={
          <>
            <p>The old look, once, small and greyed. Each item names what replaces it. These are the only imports from <span className="font-mono">@/components/ui</span> on this page that v1 forbids in new work.</p>
            <p>If you find yourself reaching for one of these, the replacement is in <span className="font-mono">src/components/op</span>.</p>
          </>
        }
        api={`// §13 Banned: cards as layout, badges, pill tabs,
// spinners as page state, blank empty states.`}
      >
        <div className="space-y-6 opacity-60">
          <Demo label="card with a shadow" className="px-0 sm:px-0">
            <Card className="max-w-sm shadow-md">
              <CardHeader className="pb-2"><CardTitle className="text-sm font-medium">Deployments</CardTitle></CardHeader>
              <CardContent className="text-xs text-muted-foreground">4 deploys today · 1 failed</CardContent>
            </Card>
            <div className="mt-2"><Rule state="error">Cards as layout. Replace with a bordered grid, or <span className="font-mono">MetricGrid</span> when the content is numbers. One <span className="font-mono">.op-raise</span> per screen, no shadows elsewhere.</Rule></div>
          </Demo>

          <Demo label="badge" className="px-0 sm:px-0">
            <div className="flex flex-wrap gap-2">
              <Badge>production</Badge>
              <Badge variant="secondary">preview</Badge>
              <Badge variant="destructive">failed</Badge>
            </div>
            <div className="mt-2"><Rule state="error">Badges. Replace with <span className="font-mono">Status</span>: a glyph and a word, so state survives greyscale and a colour-blind reader.</Rule></div>
          </Demo>

          <Demo label="pill tabs" className="px-0 sm:px-0">
            <Tabs value={tab} onValueChange={setTab}>
              <TabsList variant="pill">
                <TabsTrigger value="overview">Overview</TabsTrigger>
                <TabsTrigger value="deploys">Deploys</TabsTrigger>
                <TabsTrigger value="logs">Logs</TabsTrigger>
              </TabsList>
            </Tabs>
            <div className="mt-2"><Rule state="error">Pill tabs with a rounded active chip. Replace with the <span className="font-mono">Detail</span> tab strip: square, ink-bordered, number keys 1–9, scrolls horizontally on phones.</Rule></div>
          </Demo>

          <Demo label="spinner as page state" className="px-0 sm:px-0">
            <div className="flex h-24 items-center justify-center border"><Loader2 className="h-5 w-5 animate-spin text-muted-foreground" /></div>
            <div className="mt-2"><Rule state="error">A spinner standing in for a page. Replace with <span className="font-mono">PageState state="loading"</span>: skeleton rows in the shape of the real content, so the layout does not collapse and expand.</Rule></div>
          </Demo>

          <Demo label="empty placeholder" className="px-0 sm:px-0">
            <EmptyPlaceholder icon={Rocket} title="No deployments" description="Deploy your project to see it here." action={<Button size="sm" variant="outline" className="h-8 text-xs">Deploy</Button>} />
            <div className="mt-2"><Rule state="error">The older onboarding component. Replace with <span className="font-mono">PageState</span>: <span className="font-mono">empty</span> when there is genuinely nothing, <span className="font-mono">unconfigured</span> when operator setup is missing — the latter names what is missing, shows an example, and links to the settings page.</Rule></div>
          </Demo>
        </div>
      </Block>
    </DocPage>
  )
}
