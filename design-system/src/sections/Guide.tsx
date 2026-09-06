// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import { Box, Cpu, Database, FileText, Rocket, Waypoints } from 'lucide-react'
import { useTheme } from 'next-themes'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import {
  Callout, Kbd, KeyValue, Lede, Num, ProjectMark, Section as OpSection, SectionTitle, Status, StatusLine,
  type State,
} from '@/components/op'
import { useDocToc, useShell } from '@/components/shell-context'
import { writeToClipboard } from '@/lib/clipboard'
import { bullets, headings, markedItems, plain, slice, slug, unique } from '@/lib/md'
import { cn } from '@/lib/utils'

import brandMd from '../../docs/brand-guidelines.md?raw'
import handoffMd from '../../docs/design-system-handoff.md?raw'
import auditMd from '../../docs/ux-audit-2026-09-06.md?raw'
import rulesMd from '../../docs/RULES.md?raw'

/* ────────────────────────────────────────────────────────────────────────
   /guide — the consolidated design-system guide. One page that reads the four
   markdown documents in `docs/` and puts them in the order a designer or
   engineer actually needs them. It renders inside the app's one shell
   (`src/components/Layout.tsx`): the shell draws the top bar, the section rail
   and the "on this page" rail, this file supplies the sections, the search
   index and the body.

   The documents stay the single source of truth. This page never copies
   their prose: it imports them with Vite's `?raw` and slices them at heading
   boundaries (`src/lib/md.ts`). Where the guide adds something the docs do
   not have, it is live — swatches read from the computed CSS variables, the
   type scale in its real classes, the five glyphs from `Status`, a primitive
   rendered beside the rule it illustrates — and those live blocks hang off a
   heading id through the LIVE map below, so an editor can see exactly where
   each one lands.

   The page obeys the rules it documents: ink skin, no cards, rules between
   sections, the `.op-*` type tiers, mono for values, tables framed with soft
   rules between rows, code on the inset tone, colour only on state glyphs.
   ──────────────────────────────────────────────────────────────────────── */

// ── document slices ────────────────────────────────────────────────────
// Every cut is an exact heading line, so a renamed heading empties the
// section instead of silently swallowing the next one.

const BRAND_POSITIONING = slice(brandMd, '## 0. What Temps is', '## 1. The direction')
const BRAND_DIRECTION = slice(brandMd, '## 1. The direction', '## 6. Taste')
const BRAND_TASTE = slice(brandMd, '## 6. Taste', "## 7. Do and don't")
const BRAND_DO_DONT = slice(brandMd, "## 7. Do and don't", '## 8. Where it lives')

const HANDOFF_RUN = slice(handoffMd, '## 0. How to run and look', '## 1. What this is')
const HANDOFF_WHAT = slice(handoffMd, '## 1. What this is', '## 4. Tokens')
const HANDOFF_TOKENS = slice(handoffMd, '## 4. Tokens', '## 5. Status vocabulary')
const HANDOFF_STATUS = slice(handoffMd, '## 5. Status vocabulary', '## 6. Components')
const HANDOFF_COMPONENTS = slice(handoffMd, '## 6. Components', '## 7. The three page templates')
const HANDOFF_TEMPLATES = slice(handoffMd, '## 7. The three page templates', '## 7b. Redesigned surfaces')
const HANDOFF_SURFACES = slice(handoffMd, '## 7b. Redesigned surfaces', '## 7c. Responsive rules')
const HANDOFF_KEYBOARD = slice(handoffMd, '## 9. Keyboard', '## 10. Plans as design input')
const HANDOFF_OPEN = slice(handoffMd, '## 15. Open items', '## 16. How to hand back')

/** Taste is the centrepiece: the intro paragraph, then one anchored entry per bullet. */
const TASTE_INTRO = BRAND_TASTE.split('\n').slice(1).join('\n').split('\n- ')[0].trim()
const TASTE_ENTRIES = bullets(BRAND_TASTE)
const TASTE_IDS = unique(TASTE_ENTRIES.map((b) => `taste--${slug(b.title)}`))

/** The audit's own markers: ⏳ deferred with a reason, ◐ partly done. */
const AUDIT_DEFERRED = markedItems(auditMd, '⏳')
const AUDIT_PARTIAL = markedItems(auditMd, '◐')

/** Slices the build-time sections lean on. */
const HANDOFF_LAYOUTS = slice(handoffMd, '**Layouts by data and operation.**', '**One axis per control.**')
const HANDOFF_AXIS = slice(handoffMd, '**One axis per control.**', '**Detail**: title, status line')
const HANDOFF_CHECKLIST = slice(handoffMd, '### Record page checklist', '## 7b. Redesigned surfaces')
const HANDOFF_RESPONSIVE = slice(handoffMd, '## 7c. Responsive rules', '## 8. Data rules')
const HANDOFF_BANNED = slice(handoffMd, '## 13. Banned', '## 14. File map')

/** The agent digest, without its own H1 (the page supplies the heading). */
const RULES_BODY = rulesMd.split('\n').slice(1).join('\n').trim()

// ── live blocks ────────────────────────────────────────────────────────

/** A labelled live example. Never a card: a label, then the thing. */
function Live({ label, children, className }: { label: string; children: ReactNode; className?: string }) {
  return (
    <div className={cn('my-5 min-w-0', className)}>
      <p className="op-label mb-2 text-muted-foreground">{label}</p>
      {children}
    </div>
  )
}

const STATES: readonly State[] = ['ok', 'warn', 'error', 'idle', 'sampled']
const STATE_MEANING: Record<State, string> = {
  ok: 'healthy, passing, deployed',
  warn: 'degraded, above threshold, expiring',
  error: 'failing, unreachable',
  idle: 'not deployed, not configured, nothing yet',
  sampled: 'head-sampled past the plan allowance',
}

/** The five glyphs, rendered by the component that owns them. */
function LiveStatus() {
  return (
    <Live label="rendered with <Status>">
      <ul className="op-rows border">
        {STATES.map((s) => (
          <li key={s} className="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-3 py-2 text-sm">
            <Status state={s} label={s} />
            <span className="ml-auto text-xs text-muted-foreground">{STATE_MEANING[s]}</span>
          </li>
        ))}
      </ul>
    </Live>
  )
}

/** The type tiers in their real classes, so the page is the specimen. */
const TIERS: readonly (readonly [string, string, string])[] = [
  ['op-display', '800', 'Own your deploys'],
  ['op-h1', '700', 'Everything the box does'],
  ['op-h2', '600', 'Deployments'],
  ['op-title', '700', 'Projects'],
  ['op-h3', '600', 'api-gateway'],
  ['op-lead', '400', 'Six projects, one failing health checks.'],
  ['op-label', '500', 'last deploy'],
]

function LiveTypeScale() {
  return (
    <Live label="the scale, in the real classes">
      <div className="op-rows border">
        {TIERS.map(([cls, weight, sample]) => (
          <div key={cls} className="min-w-0 px-3 py-3">
            <div className="flex flex-wrap items-baseline gap-x-3">
              <span className="font-mono text-[11px]">.{cls}</span>
              <span className="op-label text-muted-foreground">weight {weight}</span>
            </div>
            <div
              className={cn(cls, 'mt-1 min-w-0 break-words')}
              style={cls === 'op-display' ? { fontSize: '2.5rem' } : undefined}
            >
              {sample}
            </div>
          </div>
        ))}
      </div>
    </Live>
  )
}

/** Tokens whose value the guide reads back off the live skin rather than quoting. */
const LIVE_TOKENS: readonly (readonly [string, string])[] = [
  ['--background', 'paper'],
  ['--foreground', 'ink, and every border'],
  ['--muted', 'section tone, hover, sampled band'],
  ['--muted-foreground', 'secondary text, idle glyphs'],
  ['--border', 'equals --foreground'],
  ['--op-inset', 'log panes, code blocks'],
  ['--op-rule-soft', '16% ink; row dividers only'],
  ['--primary', 'filled buttons'],
  ['--ring', 'focus only'],
  ['--success', 'the ● glyph'],
  ['--warning', 'the ◐ glyph'],
  ['--destructive', 'the × glyph'],
  ['--chart-1', 'the plotted line'],
  ['--chart-2', 'the comparison line'],
]

/**
 * Swatches painted from the computed variables, not from a table of values
 * copied out of the CSS. Re-read when the theme changes, because dark is a
 * second set of values and a stale reading would be a lie.
 */
function LiveTokens() {
  const ref = useRef<HTMLDivElement>(null)
  const { resolvedTheme } = useTheme()
  const [values, setValues] = useState<Record<string, string>>({})

  useEffect(() => {
    const el = ref.current
    if (!el) return
    const cs = getComputedStyle(el)
    const next: Record<string, string> = {}
    for (const [name] of LIVE_TOKENS) next[name] = cs.getPropertyValue(name).trim() || '—'
    setValues(next)
  }, [resolvedTheme])

  return (
    <Live label={`read off the live skin · ${resolvedTheme === 'dark' ? 'dark' : 'light'}`}>
      <div ref={ref} className="op-rows border">
        {LIVE_TOKENS.map(([name, use]) => (
          <div key={name} className="flex min-w-0 items-center gap-3 px-3 py-2">
            <span
              aria-hidden
              className="h-5 w-5 shrink-0 border"
              style={{ backgroundColor: `var(${name})` }}
            />
            <span className="shrink-0 font-mono text-xs">{name}</span>
            <span className="truncate font-mono text-[11px] text-muted-foreground">{values[name] ?? '…'}</span>
            <span className="ml-auto hidden shrink-0 text-xs text-muted-foreground sm:block">{use}</span>
          </div>
        ))}
      </div>
    </Live>
  )
}

/** The shapes rule (§6 Taste, "shapes before type") drawn rather than described. */
function LiveShapes() {
  return (
    <Live label="raised · framed · loose">
      <div className="grid gap-4 sm:grid-cols-3">
        <div className="op-raise p-3 text-xs">
          <p className="op-label">raised</p>
          <p className="mt-1 text-muted-foreground">One per page. The lede.</p>
        </div>
        <div className="border p-3 text-xs">
          <p className="op-label">framed</p>
          <p className="mt-1 text-muted-foreground">Every group, every piece of content.</p>
        </div>
        <div className="p-3 text-xs">
          <p className="op-label">loose</p>
          <p className="mt-1 text-muted-foreground">Prose and notes only.</p>
        </div>
      </div>
    </Live>
  )
}

/** A fault, drawn by the component the rule names. */
function LiveCallout() {
  return (
    <Live label="rendered with <Callout>">
      <Callout
        state="error"
        title="GitHub connection expired"
        quote="401 Bad credentials (github.com/login/oauth)"
      >
        Auto-deploys for four projects are paused until the app is reconnected.
      </Callout>
    </Live>
  )
}

/* The palette group is the sharpest case for "an icon wherever it adds context":
   one list holding pages, projects and commands. Same six rows twice — the only
   difference is the 16px slot. */
const PALETTE_ROWS = [
  { group: 'pages', label: 'databases', icon: Database, meta: 'storage' },
  { group: 'pages', label: 'traces', icon: Waypoints, meta: 'observe' },
  { group: 'projects', label: 'api-gateway', icon: Box, meta: 'app · production', state: 'warn' as State },
  { group: 'projects', label: 'billing-worker', icon: Cpu, meta: 'worker · production', state: 'error' as State },
  { group: 'projects', label: 'docs', icon: FileText, meta: 'static · production', state: 'ok' as State },
  { group: 'commands', label: 'deploy api-gateway', icon: Rocket, meta: '⌘⏎' },
]

function PaletteRows({ icons }: { icons: boolean }) {
  return (
    <div className="op-rows border font-mono text-xs">
      {PALETTE_ROWS.map((r) => (
        <div key={r.label} className="flex items-center gap-2 px-3 py-1.5">
          <span aria-hidden className="w-3 shrink-0 text-center">{r.state ? <Status state={r.state} label="" /> : null}</span>
          {icons && <r.icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />}
          <span className="min-w-0 truncate">{r.label}</span>
          <span className="ml-auto shrink-0 text-[11px] text-muted-foreground">{r.meta}</span>
        </div>
      ))}
    </div>
  )
}

function LiveKindIcons() {
  return (
    <Live label="the same palette, without the slot and with it">
      <div className="grid gap-4 sm:grid-cols-2">
        <div>
          <p className="mb-2 min-h-8 text-[11px] text-muted-foreground">bare words: every row is the same shape, so the reader reads each one to find out what it is</p>
          <PaletteRows icons={false} />
        </div>
        <div>
          <p className="mb-2 min-h-8 text-[11px] text-muted-foreground">with the kind in a fixed 16px slot: page, app, worker, static site and command, told apart before reading</p>
          <PaletteRows icons />
        </div>
      </div>
    </Live>
  )
}

const GLYPH_VS_ICON = [
  ['icon', 'says what kind', 'queued · sent · delivered · terminal · file · agent'],
  ['glyph', 'says what state', '● ◐ × ○ ◌'],
] as const

function LiveGlyphs() {
  return (
    <Live label="the two vocabularies">
      <div className="op-rows border">
        {GLYPH_VS_ICON.map(([kind, says, examples]) => (
          <div key={kind} className="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-3 py-2 text-sm">
            <span className="op-label w-12 shrink-0">{kind}</span>
            <span>{says}</span>
            <span className="ml-auto font-mono text-xs text-muted-foreground">{examples}</span>
          </div>
        ))}
      </div>
      <div className="mt-3 flex flex-wrap gap-x-5 gap-y-2">
        {STATES.map((s) => (
          <Status key={s} state={s} label={s} />
        ))}
      </div>
    </Live>
  )
}

/** The keys, with the platform-aware badge that ships them. */
const KEYS: readonly (readonly [string[], string, string])[] = [
  [['⌘', 'K'], 'everywhere', 'command palette'],
  [['/'], 'ledger', 'focus the filter'],
  [['j'], 'ledger', 'move the cursor down (focus follows)'],
  [['k'], 'ledger', 'move the cursor up (focus follows)'],
  [['⏎'], 'ledger', 'open the focused row'],
  [['1'], 'detail', 'switch tab'],
  [['⌘', '⏎'], 'detail', 'primary action'],
  [['⌘', 'S'], 'settings', 'click the save button'],
  [['esc'], 'everywhere', 'close drawer, menu, dialog'],
]

function LiveKeys() {
  return (
    <Live label="rendered with <Kbd> — ⌘ becomes Ctrl off macOS">
      <div className="op-rows border">
        {KEYS.map(([keys, where, does]) => (
          <div key={keys.join('') + where} className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 text-sm">
            <Kbd keys={[...keys]} />
            <span className="op-label text-muted-foreground">{where}</span>
            <span className="ml-auto text-xs text-muted-foreground">{does}</span>
          </div>
        ))}
      </div>
    </Live>
  )
}

/**
 * The component catalogue as links into the reference page. A name with no
 * block on `/op-components` is plain text, never a link that goes nowhere
 * (brand §6, "a drawn control is a wired control").
 */
const COMPONENT_BLOCKS: readonly (readonly [string, string | null])[] = [
  ['Status · StatusLine · Phrase', 'status'],
  ['Num · Metric · MetricGrid', 'num'],
  ['PageState', 'page-state'],
  ['Kbd', 'kbd'],
  ['EchoDialog', 'echo'],
  ['TimeChart · RangePicker · ChartFooter', 'chart'],
  ['Ledger', 'ledger'],
  ['Detail · PageTitle · Segmented', 'detail'],
  ['Picker', 'picker'],
  ['Settings · Field', 'settings'],
  ['ProjectMark', 'mark'],
  ['Breakdown · Sparkline · Funnel · Flow', 'breakdown'],
  ['Callout', 'callout'],
  ['StatusStrip · ScoreRing · CalendarHeatmap · Live', 'strip'],
  ['Waterfall · StackTrace', 'trace'],
  ['LogLines · Stages · Histogram', 'logs'],
  ['Switch · Toggle', null],
  ['SecretValue', null],
  ['LogViewer · EmptyPlaceholder', null],
]

function LiveComponentIndex() {
  return (
    <Live label="every block on /op-components">
      <ul className="op-rows border">
        {COMPONENT_BLOCKS.map(([name, id]) => (
          <li key={name} className="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-3 py-2 text-sm">
            <span className="font-mono text-xs">{name}</span>
            {id ? (
              <Link to={`/op-components#${id}`} className="op-status ml-auto text-xs">
                /op-components#{id}
              </Link>
            ) : (
              <span className="ml-auto text-xs text-muted-foreground">no block yet</span>
            )}
          </li>
        ))}
      </ul>
    </Live>
  )
}

const TEMPLATE_LINKS = [
  ['Ledger', 'ledger', 'many records of one kind'],
  ['Detail', 'detail', 'one resource with facets'],
  ['Settings', 'settings', 'a configuration, saved once'],
] as const

function LiveTemplates() {
  return (
    <Live label="the three templates, live">
      <ul className="op-rows border">
        {TEMPLATE_LINKS.map(([name, id, what]) => (
          <li key={id} className="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-3 py-2 text-sm">
            <span className="font-mono text-xs">{name}</span>
            <span className="text-xs text-muted-foreground">{what}</span>
            <Link to={`/op-components#${id}`} className="op-status ml-auto text-xs">
              /op-components#{id}
            </Link>
          </li>
        ))}
      </ul>
    </Live>
  )
}

/** The five rules, with the three shapes they produce. */
function LiveFiveRules() {
  return (
    <Live label="paper, ink, one raise, colour only on a glyph">
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="op-raise p-3">
          <p className="op-label">the one raise</p>
          <p className="mt-1 text-sm text-muted-foreground">
            3px hard shadow, ink border. The thing the reader is meant to act on.
          </p>
        </div>
        <div className="border p-3">
          <p className="op-label">colour means status</p>
          <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1">
            {STATES.map((s) => (
              <Status key={s} state={s} label={s} />
            ))}
          </div>
        </div>
      </div>
    </Live>
  )
}

/**
 * Every surface in §7b, with the `?p=` view it is rendered at. Explicit, not
 * scraped out of the heading: the ids are typed here so a dead one is a
 * visible mistake rather than a click that goes nowhere.
 */
const SURFACE_LINKS: Record<string, readonly (readonly [string, string])[]> = {
  '7b-redesigned-surfaces-on-the-templates': [
    ['deploys · environments · variables', '/v1?p=api-gateway'],
    ['sandboxes', '/v1?p=sandboxes'],
    ['traces', '/v1?p=traces'],
    ['metrics', '/v1?p=metrics'],
  ],
  'backups-v1-p-backups': [['backups', '/v1?p=backups']],
  'git-providers-v1-p-git-git-id': [['git providers', '/v1?p=git'], ['a provider', '/v1?p=git%3A1']],
  'security-v1-p-security-scan-id': [['security', '/v1?p=security']],
  'errors-v1-p-errors-issue-id': [
    ['issues', '/v1?p=errors'],
    ['an issue', '/v1?p=issue%3Ai_4821'],
    ['store outage', '/v1?p=errors&fail=1'],
  ],
  'settings-v1-p-settings-settings-slug': [
    ['settings hub', '/v1?p=settings'],
    ['api keys', '/v1?p=settings%3Akeys'],
    ['builds', '/v1?p=settings%3Abuilds'],
  ],
  'uptime-monitor-v1-p-uptime-monitor-id-and-the-public-status-page': [
    ['uptime', '/v1?p=uptime'],
    ['a monitor', '/v1?p=monitor%3Amon_2'],
    ['public status page', '/status?project=acme-storefront'],
  ],
  'proxy-v1-p-proxy': [['proxy', '/v1?p=proxy']],
  'deployment-deploy-tag': [
    ['live deploy', '/v1?p=deploy%3Adep_91a'],
    ['failed build', '/v1?p=deploy%3Adep_92e'],
    ['building', '/v1?p=deploy%3Adep_92b'],
  ],
  'database-v1-p-databases-db-name': [
    ['databases', '/v1?p=databases'],
    ['a database', '/v1?p=db%3Aacme-pg'],
  ],
  'analytics-v1-p-analytics-event-name': [
    ['analytics', '/v1?p=analytics'],
    ['an event', '/v1?p=event%3Asignup'],
  ],
  'email-v1-p-email-email-id-domain-id': [
    ['email', '/v1?p=email'],
    ['a domain', '/v1?p=domain%3A3'],
  ],
  'nodes-settings-nodes-node-name-settings-cluster': [
    ['nodes', '/v1?p=settings%3Anodes'],
    ['a node', '/v1?p=node%3Ahetzner-3'],
    ['cluster', '/v1?p=settings%3Acluster'],
  ],
  'landing-system-map-landing-one-engine-at-the-center': [['the landing', '/landing']],
  'agent-conversation-agent': [['agent conversation', '/agent']],
}

function SurfaceLinks({ slugId }: { slugId: string }) {
  const links = SURFACE_LINKS[slugId]
  if (!links) return null
  return (
    <p className="mt-2 mb-3 flex flex-wrap items-baseline gap-x-4 gap-y-1 text-xs">
      {links.map(([label, to]) => (
        <Link key={to} to={to} className="op-status font-mono">
          {label} ↗
        </Link>
      ))}
    </p>
  )
}

/**
 * heading id → what to render under it. This is the whole "where does a live
 * block go" story: add an entry, it appears under that heading, nowhere else.
 */
const LIVE: Record<string, ReactNode> = {
  'start--3-the-five-rules': <LiveFiveRules />,
  'brand--2-hierarchy': <LiveTypeScale />,
  'tokens--4-tokens': <LiveTokens />,
  'status--5-status-vocabulary': <LiveStatus />,
  'components--6-components': <LiveComponentIndex />,
  'templates--7-the-three-page-templates': <LiveTemplates />,
  'keyboard--9-keyboard': <LiveKeys />,
}

/** taste entry id → the primitive that shows the rule instead of describing it. */
const TASTE_LIVE: Record<string, ReactNode> = {
  'taste--shapes-before-type': <LiveShapes />,
  'taste--icons-say-what-glyphs-say-how': <LiveGlyphs />,
  'taste--an-icon-wherever-it-adds-context': <LiveKindIcons />,
  'taste--a-fault-looks-like-a-fault': <LiveCallout />,
}

// ── markdown rendering ─────────────────────────────────────────────────

/** Documents the guide renders, and where a link to them lands in-page. */
const DOC_ANCHORS: Record<string, string | null> = {
  'brand-guidelines.md': '#brand',
  'design-system-handoff.md': '#start',
  'ux-audit-2026-09-06.md': '#open',
  'console-inventory.md': null,
  'design-system-answers.md': null,
  'operator-console-brief.md': null,
}

function textOf(node: ReactNode): string {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  if (typeof node === 'object' && 'props' in (node as { props?: unknown })) {
    return textOf((node as { props: { children?: ReactNode } }).props.children)
  }
  return ''
}

const HEADING_CLASS: Record<number, string> = {
  2: 'op-h2 mt-10 text-[1.25rem]',
  3: 'op-h3 mt-8',
  4: 'op-h3 mt-6 text-[0.9375rem]',
  5: 'op-h3 mt-5 text-sm',
  6: 'op-h3 mt-4 text-sm',
}

/**
 * The markdown components. `prefix` namespaces every heading id by section,
 * so `#taste--edges-align` is unambiguous and two documents can both have a
 * "Tests" heading without colliding.
 *
 * Source headings are shifted one level down (`##` renders as `<h3>`) so the
 * page's own h1/h2 stay above them and the outline never skips a level.
 */
function mdComponents(prefix: string): Components {
  const heading = (depth: number) =>
    function Heading({ children }: { children?: ReactNode }) {
      const text = plain(textOf(children))
      const id = `${prefix}--${slug(text)}`
      const rendered = Math.min(depth + 1, 6)
      const Tag = `h${rendered}` as 'h2' | 'h3' | 'h4' | 'h5' | 'h6'
      return (
        <>
          <Tag id={id} className={cn('group flex scroll-mt-16 items-baseline gap-2', HEADING_CLASS[rendered])}>
            <span className="min-w-0">{children}</span>
            <a
              href={`#${id}`}
              aria-label={`Link to “${text}”`}
              className="shrink-0 font-mono text-xs text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
            >
              #
            </a>
          </Tag>
          {prefix === 'surfaces' ? <SurfaceLinks slugId={slug(text)} /> : null}
          {LIVE[id] ?? null}
        </>
      )
    }

  return {
    h1: heading(1),
    h2: heading(2),
    h3: heading(3),
    h4: heading(4),
    h5: heading(5),
    h6: heading(6),
    p: ({ children }) => <p className="op-prose mt-3 max-w-[72ch] text-sm break-words">{children}</p>,
    ul: ({ children }) => (
      <ul className="mt-3 max-w-[72ch] list-disc space-y-1.5 pl-5 text-sm marker:text-muted-foreground">{children}</ul>
    ),
    ol: ({ children }) => (
      <ol className="mt-3 max-w-[72ch] list-decimal space-y-1.5 pl-5 text-sm marker:text-muted-foreground">{children}</ol>
    ),
    li: ({ children }) => <li className="op-prose break-words [&>p]:mt-0 [&>p]:max-w-none">{children}</li>,
    strong: ({ children }) => <strong className="font-semibold">{children}</strong>,
    em: ({ children }) => <em className="italic">{children}</em>,
    hr: () => <hr className="mt-8 border-t" />,
    blockquote: ({ children }) => (
      <blockquote className="mt-4 max-w-[72ch] border-l-2 pl-3 text-sm [&>p]:mt-0 [&>p+p]:mt-2">{children}</blockquote>
    ),
    code: ({ children }) => <code className="font-mono text-[0.875em]">{children}</code>,
    pre: ({ children }) => (
      <pre
        tabIndex={0}
        data-allow-overflow
        className="op-inset mt-4 overflow-auto border p-3 font-mono text-[11px] leading-5 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
      >
        {children}
      </pre>
    ),
    table: ({ children }) => (
      <div data-allow-overflow className="mt-4 overflow-x-auto" tabIndex={0}>
        <table className="w-full min-w-[34rem] border-separate border-spacing-0 border text-left text-xs">
          {children}
        </table>
      </div>
    ),
    thead: ({ children }) => <thead>{children}</thead>,
    tbody: ({ children }) => <tbody className="op-rows">{children}</tbody>,
    th: ({ children }) => <th scope="col" className="op-label border-b px-3 py-2 align-bottom">{children}</th>,
    td: ({ children }) => <td className="px-3 py-2 align-top break-words">{children}</td>,
    a: ({ href, children }) => {
      const h = href ?? ''
      if (h.startsWith('#')) return <a href={h} className="op-status">{children}</a>
      if (/^https?:/.test(h)) return <a href={h} target="_blank" rel="noreferrer" className="op-status">{children}</a>
      const doc = Object.keys(DOC_ANCHORS).find((d) => h.startsWith(d))
      if (doc !== undefined) {
        const to = DOC_ANCHORS[doc]
        return to ? (
          <a href={to} className="op-status">{children}</a>
        ) : (
          <span className="font-mono text-[0.875em] text-muted-foreground" title={`${doc} is not part of the guide`}>
            {children}
          </span>
        )
      }
      if (h.startsWith('/')) return <Link to={h} className="op-status">{children}</Link>
      return <span className="font-mono text-[0.875em]">{children}</span>
    },
  }
}

function Md({ prefix, children }: { prefix: string; children: string }) {
  const components = useMemo(() => mdComponents(prefix), [prefix])
  return (
    <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
      {children}
    </ReactMarkdown>
  )
}

// ── the taste entries ──────────────────────────────────────────────────

function TasteEntries() {
  return (
    <div>
      {TASTE_ENTRIES.map((entry, i) => {
        const id = TASTE_IDS[i]
        return (
          <section key={id} className="mt-6 border-t pt-5 first:mt-4">
            <h3 id={id} className="op-h3 group flex scroll-mt-16 items-baseline gap-2">
              <span className="min-w-0">{entry.title}</span>
              <a
                href={`#${id}`}
                aria-label={`Link to “${entry.title}”`}
                className="shrink-0 font-mono text-xs text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
              >
                #
              </a>
            </h3>
            <Md prefix={`${id}-body`}>{entry.body}</Md>
            {TASTE_LIVE[id] ?? null}
          </section>
        )
      })}
    </div>
  )
}

// ── copy ───────────────────────────────────────────────────────────────

/**
 * Copy a block of text. The result is a word, not a colour: "copied" on
 * success, the reason on failure (a self-hosted box on plain http has no
 * clipboard API, and a checkmark over an empty clipboard is found later, by
 * pasting the wrong thing).
 */
function CopyText({ value, label = 'copy' }: { value: string; label?: string }) {
  const [said, setSaid] = useState<{ word: string; state: State } | null>(null)
  return (
    <button
      type="button"
      onClick={async () => {
        try {
          await writeToClipboard(value)
          setSaid({ word: 'copied', state: 'ok' })
        } catch {
          setSaid({ word: 'copy failed — select and copy by hand', state: 'error' })
        }
        window.setTimeout(() => setSaid(null), 2500)
      }}
      className="op-label flex h-6 shrink-0 items-center gap-1.5 border px-2 hover:bg-muted"
    >
      {said ? <Status state={said.state} label={said.word} /> : label}
    </button>
  )
}

/** A copyable snippet on the inset tone. Focusable, because it scrolls. */
function Snippet({ title, code }: { title: string; code: string }) {
  return (
    <div className="mt-4 min-w-0 border">
      <div className="flex items-center gap-3 border-b px-3 py-1.5">
        <span className="op-label min-w-0 truncate">{title}</span>
        <span className="ml-auto">
          <CopyText value={code} />
        </span>
      </div>
      <pre
        tabIndex={0}
        data-allow-overflow
        className="op-inset overflow-auto p-3 font-mono text-[11px] leading-5 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
      >
        {code}
      </pre>
    </div>
  )
}

// ── signature ──────────────────────────────────────────────────────────

/**
 * The traits that make a screen unmistakably Temps rather than another
 * dashboard. Every one is a line already in `brand-guidelines.md` §1–§6;
 * what the guide adds is the rendered example beside it and the wrong
 * version in words, which the documents describe but never draw.
 */
type Trait = { title: string; rule: string; wrong: string; demo: ReactNode }

const SIGNATURE: readonly Trait[] = [
  {
    title: 'Paper and ink, and nothing between them',
    rule: 'Warm off-white paper, near-black ink, dark mode inverting the same pair. No grey scaffolding, no third surface colour.',
    wrong: 'A grey card floating on a white page with a blue link in it — three surfaces and a hue, none of which mean anything.',
    demo: (
      <div className="grid grid-cols-2 gap-3 text-xs sm:grid-cols-4">
        {(['--background', '--foreground', '--muted', '--op-inset'] as const).map((t) => (
          <div key={t} className="border">
            <span aria-hidden className="block h-9 border-b" style={{ backgroundColor: `var(${t})` }} />
            <span className="block px-2 py-1 font-mono text-[10px]">{t}</span>
          </div>
        ))}
      </div>
    ),
  },
  {
    title: 'Square everything, one hard shadow',
    rule: 'Radius 0.25rem, 1px ink borders, and a single 3px hard shadow (`.op-raise`) on the one element the reader is meant to act on.',
    wrong: 'Rounded-xl panels with a soft blurred drop shadow, repeated eight times down the page.',
    demo: (
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="border p-3 text-xs">
          <p className="op-label">framed</p>
          <p className="mt-1 text-muted-foreground">1px ink. Every group and every piece of content.</p>
        </div>
        <div className="op-raise p-3 text-xs">
          <p className="op-label">raised</p>
          <p className="mt-1 text-muted-foreground">3px hard shadow. Once per screen.</p>
        </div>
      </div>
    ),
  },
  {
    title: 'The mark sits with the name',
    rule: "A project's mark is 16px in a row, list or palette and 24px beside a page title, and appears nowhere else. Unknown marks are a monogram in ink.",
    wrong: 'A logo tile in a hero, or an unfetched mark filled with a random colour so "unknown" looks like a brand.',
    demo: (
      <ul className="op-rows border text-sm">
        {[
          ['acme-storefront', 'production'],
          ['api-gateway', 'production'],
          ['billing-worker', 'staging'],
        ].map(([name, env]) => (
          <li key={name} className="flex items-center gap-2 px-3 py-2">
            <ProjectMark name={name} />
            <span className="min-w-0 truncate">{name}</span>
            <span className="ml-auto font-mono text-xs text-muted-foreground">{env}</span>
          </li>
        ))}
      </ul>
    ),
  },
  {
    title: 'Every page carries one verdict',
    rule: 'One worst-state sentence under 60 characters, at most one link. Inside the shell it collapses into the header as a glyph and a count; counts and "fine" things never appear in it.',
    wrong: '`◐ 6 projects · × billing-worker failing · 4 deploys today · cert 6d` strung across the page as a status bar.',
    demo: (
      <StatusLine state="error" sticky={false} more={{ label: '+1 warning' }}>
        billing-worker is failing health checks.
      </StatusLine>
    ),
  },
  {
    title: 'A record opens with a Lede',
    rule: 'State glyph plus one word, one muted sentence, then four to six mono facts. It is the one raised block on the page and the first shape the eye finds.',
    wrong: 'A headline sentence with no facts, and the facts the reader wanted repeated down in the aside.',
    demo: (
      <Lede
        state="ok"
        word="live"
        facts={[
          { k: 'commit', v: '9bc61c0' },
          { k: 'built in', v: '2m 41s' },
          { k: 'error rate', v: '0.61%', state: 'warn' },
          { k: 'promoted', v: '10h ago' },
        ]}
      >
        dep_91a · production · main
      </Lede>
    ),
  },
  {
    title: 'Five glyphs, and colour only beside one',
    rule: 'Colour appears through `Status` only: glyph, word, tone, in that order. A tone with nothing beside it is decoration at best and a wrong guess at worst.',
    wrong: 'A bare red number, an amber word, and a legend in the footer that buys them back.',
    demo: (
      <div className="flex flex-wrap gap-x-5 gap-y-2">
        {STATES.map((s) => (
          <Status key={s} state={s} label={s} />
        ))}
      </div>
    ),
  },
  {
    title: 'Rules between sections, frames around groups, never cards',
    rule: 'An ink rule separates sections, a soft rule separates rows, a frame encloses a group or a piece of content. Nothing else draws a line.',
    wrong: 'Each block in its own shadowed card, so the page is a pile of correct parts with no order.',
    demo: (
      <div>
        <OpSection title="What happened" meta="3 · last 10h ago">
          <KeyValue
            rows={[
              { k: 'queued', v: '10h ago' },
              { k: 'delivered', v: '10h ago · ses-eu' },
            ]}
          />
        </OpSection>
        <OpSection title="Headers" meta="8 fields">
          <KeyValue rows={[{ k: 'message-id', v: '<a1@temps.sh>' }]} />
        </OpSection>
      </div>
    ),
  },
  {
    title: 'Mono for values, sans for words',
    rule: 'Numbers, ids, branches, sizes and windows are mono and tabular, with the unit after the value in muted. Prose is Geist Sans. A number that matters is one tier larger than its label.',
    wrong: 'A column of proportional numerals that do not line up, with the unit glued to the value in the same weight.',
    demo: (
      <div className="op-rows border text-sm">
        {[
          ['requests', 30800, ''],
          ['p95', 184, 'ms'],
          ['error rate', '0.61', '%'],
        ].map(([label, value, unit]) => (
          <div key={String(label)} className="flex items-baseline gap-3 px-3 py-2">
            <span className="op-label">{label}</span>
            <span className="ml-auto text-base">
              <Num value={value as number | string} unit={(unit as string) || undefined} />
            </span>
          </div>
        ))}
      </div>
    ),
  },
  {
    title: 'Sentences over labels',
    rule: 'A section is a 600/14 title with its count or one fact in mono beside it. `.op-label` is for column headers, field names and key badges — never for the title of a section.',
    wrong: 'A page of 10px uppercase eyebrows: no hierarchy, and nothing to read before reading everything.',
    demo: (
      <div className="border p-3">
        <SectionTitle title="What happened" meta="3 events · last 14m ago" />
        <p className="op-prose mt-2 text-xs text-muted-foreground">
          Not <span className="op-label">what happened</span> in 10px uppercase.
        </p>
      </div>
    ),
  },
  {
    title: 'The keyboard is an entry point, never the only one',
    rule: 'Every key has a visible badge and a live handler; every badge has a button behind it. Keys are ignored while an input has focus, and the cursor moves DOM focus with it.',
    wrong: 'A `Kbd` badge drawn next to an action nothing binds — a drawn control the reader spends trust on before finding out.',
    demo: (
      <div className="flex flex-wrap items-center gap-x-5 gap-y-2 text-xs">
        {(
          [
            [['/'], 'filter'],
            [['j'], 'down'],
            [['k'], 'up'],
            [['⏎'], 'open'],
            [['⌘', 'S'], 'save'],
          ] as const
        ).map(([keys, what]) => (
          <span key={what} className="flex items-center gap-1.5">
            <Kbd keys={[...keys]} />
            <span className="text-muted-foreground">{what}</span>
          </span>
        ))}
      </div>
    ),
  },
]

const SIGNATURE_IDS = unique(SIGNATURE.map((t) => `signature--${slug(t.title)}`))

function SignatureSection() {
  return (
    <div>
      <p className="op-prose max-w-[72ch] text-sm">
        Ten traits. A screen that has all ten is recognisably Temps before a word of it is read; a screen
        missing three of them is another dashboard. Each is a line from{' '}
        <a href="#brand" className="op-status">brand-guidelines.md §1–§6</a>, rendered here with the primitive
        that produces it and the version it replaces.
      </p>
      {SIGNATURE.map((trait, i) => {
        const id = SIGNATURE_IDS[i]
        return (
          <section key={id} className="mt-8 border-t pt-6">
            <h3 id={id} className="op-h3 group flex scroll-mt-16 items-baseline gap-2">
              <span className="min-w-0">
                <span className="mr-2 font-mono text-xs text-muted-foreground">{i + 1}</span>
                {trait.title}
              </span>
              <a
                href={`#${id}`}
                aria-label={`Link to “${trait.title}”`}
                className="shrink-0 font-mono text-xs text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
              >
                #
              </a>
            </h3>
            <div className="mt-2 [&>p]:mt-0">
              <Md prefix={`${id}-rule`}>{trait.rule}</Md>
            </div>
            <div className="mt-4 min-w-0">{trait.demo}</div>
            <div className="mt-4 flex max-w-[72ch] items-start gap-2">
              <span aria-hidden className="mt-0.5 w-3 shrink-0 text-center text-sm text-destructive">×</span>
              <div className="min-w-0 flex-1 [&>p]:mt-0 [&>p]:text-muted-foreground">
                <Md prefix={`${id}-wrong`}>{trait.wrong}</Md>
              </div>
            </div>
          </section>
        )
      })}
    </div>
  )
}

// ── build a screen ─────────────────────────────────────────────────────

const LEDGER_SNIPPET = `import { Ledger, PageState, StatusLine, Status } from '@temps-sdk/op'

<Ledger
  title="Projects"
  meta="6 · production"
  status={<StatusLine state="error" more={{ label: '+1 warning' }}>
    billing-worker is failing health checks.
  </StatusLine>}
  columns={['name', { label: 'environment', key: 'env' }, 'last deploy', 'state']}
  grid="minmax(6rem,2fr) minmax(90px,max-content) 140px 90px"
  rows={rows}            // { id, cells, mobile, sort, onOpen } — mobile carries the row's action
  total={total}
  filter={q} onFilter={setQ}          // omit both and no filter box is drawn
  hint="needs attention first, then last deploy"
  dense={dense}
  page={{ page, pageSize: 20, total, onPage }}
  state={rows.length === 0 ? <PageState state="empty" … /> : undefined}
/>`

const RECORD_SNIPPET = `import { Detail, Columns, Lede, Section, KeyValue, Timeline, StatusLine } from '@temps-sdk/op'

<Detail
  title="acme-storefront"                       // 1. title + meta place the record
  meta="dep_91a · production · main"
  status={<StatusLine state="warn">error rate 0.61% since dep_91a.</StatusLine>}   // 2. the verdict
  actions={<Button className="op-primary">roll back</Button>}
  lede={<Lede state="ok" word="live" facts={[   // 3. the one raise: word + 4–6 facts
    { k: 'commit', v: '9bc61c0' },
    { k: 'built in', v: '2m 41s' },
    { k: 'error rate', v: '0.61%', state: 'warn' },
    { k: 'promoted', v: '10h ago' },
  ]}>dep_91a · production · main</Lede>}
>
  <Columns>                                     {/* 4. main column, then the 18rem aside at xl */}
    <div>
      <Section title="Build" action={<Segmented …/>}>…the thing itself…</Section>
      <Section title="What happened" meta="7 · last 10h ago"><Timeline items={events} /></Section>
    </div>
    <div>
      <Section title="Identifiers" meta="6"><KeyValue rows={facts} compact /></Section>
    </div>
  </Columns>
</Detail>`

const SETTINGS_SNIPPET = `import { Settings, Field, EchoDialog, StatusLine } from '@temps-sdk/op'

<Settings
  title="Build & deploy"
  meta="acme-storefront · production"
  status={<StatusLine state="ok">Nothing to do: last build 2m 41s.</StatusLine>}
  sections={[{ title: 'general', body: (
    <Field label="auto-deploy branch" help="applies to the next push">
      <Picker value={branch} onChange={setBranch} options={BRANCHES} allowCustom="use branch" />
    </Field>
  ) }]}
  dirty={dirty}
  onSave={save}                                  // ⌘S clicks this button
  danger={<EchoDialog … />}                      // the only action in the danger zone
/>`

const TOOL_SNIPPET = `// A tool page (terminal, replay player, data browser) is not a template:
// the shell goes around it. Keep the outside conventional and the tool inside a frame.

<Detail title="sbx_7f21" meta="temps/sandbox:node22 · fsn1" status={<StatusLine …/>}
        lede={<Lede state="ok" word="running" facts={[…]} />}>
  <Columns>
    <div>
      <Section title="Terminal">
        <div className="op-inset border">{/* the embedded tool, unstyled by us */}</div>
      </Section>
    </div>
    <div><Section title="Identity" meta="5"><KeyValue rows={facts} compact /></Section></div>
  </Columns>
</Detail>`

const STATES_TO_COVER: readonly (readonly [string, string])[] = [
  ['loading', '`PageState state="loading"` — skeleton rows. Never a spinner as the page.'],
  ['empty', '`PageState state="empty"` — title, the reason, the next step.'],
  ['unconfigured', '`PageState state="unconfigured"` — what is missing, an example of what it will show, a typed link to the settings page that fixes it.'],
  ['error', '`PageState state="error"` — the message, the resource, a retry. Quote the other system verbatim.'],
  ['sampled', 'The `◌` glyph in the status line, the band on the chart, the note in the chart footer.'],
  ['phone (390)', 'Actions scroll sideways at natural width; ledger rows render `mobile` with the primary action in it; no horizontal document scroll.'],
  ['dark', 'A second set of token values, not a re-skin. A contrast pair that passes in light can fail in dark.'],
]

const FACT_PLACES: readonly (readonly [string, string])[] = [
  ['title meta', 'What places the record: id · project · environment, and the one fact that names it.'],
  ['lede facts', 'The four to six values the reader wants without scrolling.'],
  ['a section row', 'Everything the reader came to read, in the main column.'],
  ['the aside', 'What is left after the meta and the Lede: reference they did not come for. Never a repeat.'],
]

function BuildScreenSection() {
  return (
    <div>
      <p className="op-prose max-w-[72ch] text-sm">
        Four steps. Pick the template from what the data is and what the reader does with it, paste the
        skeleton, cover the states, then put each fact in exactly one place.
      </p>

      <h3 id="build--1-pick-the-template" className="op-h3 mt-8 scroll-mt-16 border-t pt-6">
        1. Pick the template
      </h3>
      <Md prefix="build-layouts">{HANDOFF_LAYOUTS}</Md>
      <Md prefix="build-axis">{HANDOFF_AXIS}</Md>

      <h3 id="build--2-paste-the-skeleton" className="op-h3 mt-10 scroll-mt-16 border-t pt-6">
        2. Paste the skeleton
      </h3>
      <p className="op-prose mt-2 max-w-[72ch] text-sm text-muted-foreground">
        All four import from <code className="font-mono">@temps-sdk/op</code>. Put{' '}
        <code className="font-mono">operator ink v1</code> on the root element and import{' '}
        <code className="font-mono">@temps-sdk/op/op.css</code> before any other rule.
      </p>
      <Snippet title="ledger — many records of one kind" code={LEDGER_SNIPPET} />
      <Snippet title="record — title + meta → status → lede → columns → sections" code={RECORD_SNIPPET} />
      <Snippet title="settings — sections, sticky save bar, danger zone" code={SETTINGS_SNIPPET} />
      <Snippet title="tool page — the shell goes around the embedded tool" code={TOOL_SNIPPET} />

      <h3 id="build--3-cover-the-states" className="op-h3 mt-10 scroll-mt-16 border-t pt-6">
        3. Cover the states
      </h3>
      <p className="op-prose mt-2 max-w-[72ch] text-sm text-muted-foreground">
        Seven, every time. A self-hosted reader debugs alone: a state that fails silently is a state they
        discover by restarting.
      </p>
      <ul className="op-rows mt-4 border">
        {STATES_TO_COVER.map(([state, what]) => (
          <li key={state} className="min-w-0 px-3 py-2 text-sm">
            <span className="op-label">{state}</span>
            <span className="op-prose mt-0.5 block text-xs text-muted-foreground">
              <Md prefix={`build-state-${slug(state)}`}>{what}</Md>
            </span>
          </li>
        ))}
      </ul>

      <h3 id="build--4-put-each-fact-in-one-place" className="op-h3 mt-10 scroll-mt-16 border-t pt-6">
        4. Put each fact in one place
      </h3>
      <ul className="op-rows mt-4 border">
        {FACT_PLACES.map(([place, what]) => (
          <li key={place} className="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-3 py-2 text-sm">
            <span className="op-label w-24 shrink-0">{place}</span>
            <span className="op-prose min-w-0 flex-1 text-xs text-muted-foreground">{what}</span>
          </li>
        ))}
      </ul>
      <p className="mt-4 flex max-w-[72ch] items-start gap-2 text-sm">
        <span aria-hidden className="w-3 shrink-0 text-center text-destructive">×</span>
        <span className="op-prose min-w-0 text-muted-foreground">
          A fact said twice is the failure <code className="font-mono">scripts/audit-records.mjs</code>{' '}
          fails the build on. If it is in the meta or the Lede, it is not a row in the aside.
        </span>
      </p>
    </div>
  )
}

// ── do / don't ─────────────────────────────────────────────────────────

/**
 * Pairs from the 2026-09-06 UX audit and brand §6. Each one is a real finding
 * on a real screen, why it stopped the surface reading as Temps, and what the
 * fix looked like — so the same mistake is recognisable before it ships.
 */
type Pair = { title: string; found: string; broke: string; fix: string; source: string }

const PAIRS: readonly Pair[] = [
  {
    title: 'A drawn control that changes nothing',
    found: 'Kbd badges for N / P / T in five files with nothing bound to them; a RangePicker that stored a range and recomputed no data while the footer said "showing 7d".',
    broke: 'The reader spends their trust on the control before they find out. A promise the chrome makes and the data layer never keeps is worse than an absent feature.',
    fix: 'Bind it, make it plain text, or delete it. Badges without handlers were removed; paged sort now requires controlled `sort`/`onSort` so the footer cannot claim the set.',
    source: 'audit 4, 1, 3 · brand §6 "a drawn control is a wired control"',
  },
  {
    title: 'Colour with no word beside it',
    found: 'Two chart series in `--chart-1/2` hues with a hand-written muted legend; a "captured" state reusing the `◌` sampled glyph; a state tone next to the word "protected".',
    broke: 'Colour is the system\'s only signal for state. Spend it on anything else and the one red cell that matters stops reading as red.',
    fix: 'Anything carrying state carries `Status`: glyph, word, tone, in that order. A legend explains a chart\'s lines; it does not license a bare tone.',
    source: 'audit 30, 31, 33 · brand §6 "a legend does not license colour"',
  },
  {
    title: 'Below md the console lost function, not layout',
    found: 'Row actions (send test, activate, reconnect) lived only in desktop `cells`; the ledger `hint` carrying live/pause and the selection notice was `hidden md:block`; the agent workspace Picker was `md:flex`.',
    broke: 'A phone reader saw a silently filtered list with no way to change it. "Responsive" that removes the verb is not responsive.',
    fix: 'The `mobile` node carries the row\'s primary action; `hint` renders at all widths and never takes interactive children; the blast-radius control stays.',
    source: 'audit 8, 9, 10 · brand §6 "a phone loses width, never function"',
  },
  {
    title: 'Confirmation by which control was built first',
    found: '⌘⏎ deployed to production from anywhere with no confirm, while a rollback needed a typed id. Deactivating the last mail provider — outbound mail stops — was one click and a toast; turning tracking off needed "delete tracking" typed.',
    broke: 'Typed confirmation is the system\'s signal for irreversible loss. Spent on reversible edits, it stops meaning anything where it matters.',
    fix: 'Confirm by consequence. `EchoDialog` for irreversible loss only; everything else asks in ink and says how to undo.',
    source: 'audit 6 · brand §7 "don\'t make a confirmation red because it needs approval"',
  },
  {
    title: 'A cursor that painted without moving focus',
    found: 'Ledger rows had no `tabIndex`; `role="listbox"` wrapped the header, the empty row and the footer, so `aria-activedescendant` was inert and only `j`/`k` reached a row.',
    broke: 'The reader saw a marked row, pressed ⏎, and landed on whatever actually had focus. A cursor that is not the focus is a lie about where you are.',
    fix: 'Roving `tabindex`, `role="button"` rows, the listbox roles removed, the footer outside the list. The cursor moves DOM focus with it.',
    source: 'audit 12 · handoff §9 "the cursor is the focus"',
  },
  {
    title: 'Tabs on a single record',
    found: 'The email record was three tabs — events, headers, content. The domain, sandbox and scan records still are.',
    broke: 'The reader opens a record because something went wrong and needs all three at once to see what. Hiding two behind clicks makes them hunt.',
    fix: 'One page in reading order: content, then what happened, then reference in the aside. Two faithful renderings of the same thing are a 2-view Segmented, never a tab.',
    source: 'audit 23 · handoff §7, rule 6',
  },
  {
    title: 'A fault drawn as information',
    found: 'Failures rendered as a raised note in ink with a label above them, inside the page\'s other frames.',
    broke: 'A raised ink note reads as information, and information is not what a 401 is. A frame inside two frames is the third border in a row and the eye stops reading them.',
    fix: '`Callout`: the × and the title in the state tone, a 2px left rule, the other system\'s words quoted verbatim on the inset tone, one sentence of what it costs, then the action.',
    source: 'brand §6 "a fault looks like a fault"',
  },
  {
    title: 'A fact said twice',
    found: 'A record shipped with a Lede that had no facts, a meta that was only the id, and a verdict repeating the Lede word ("Delivered 3h ago" under a Lede saying "delivered").',
    broke: 'Three rules broken at once, and nothing caught it. Repetition reads as a page assembled from parts rather than written.',
    fix: 'The eight-rule record checklist, now enforced: dev warnings from `Lede` and `Detail`, `scripts/audit-records.mjs` in `bun run lint`, and the e2e suite.',
    source: 'handoff §7 record page checklist',
  },
]

const PAIR_IDS = unique(PAIRS.map((p) => `dodont--${slug(p.title)}`))

function DoDontSection() {
  return (
    <div>
      <p className="op-prose max-w-[72ch] text-sm">
        Eight pairs from the audit of 2026-09-06 and brand §6. Each is what was found, why it broke
        recognition, and what the fix was. The pattern behind most of them: the documents' most specific
        rules are the ones that get broken.
      </p>
      {PAIRS.map((pair, i) => {
        const id = PAIR_IDS[i]
        return (
          <section key={id} className="mt-8 border-t pt-6">
            <h3 id={id} className="op-h3 group flex scroll-mt-16 items-baseline gap-2">
              <span className="min-w-0">{pair.title}</span>
              <a
                href={`#${id}`}
                aria-label={`Link to “${pair.title}”`}
                className="shrink-0 font-mono text-xs text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
              >
                #
              </a>
            </h3>
            <dl className="mt-3 max-w-[72ch] space-y-3 text-sm">
              <div>
                <dt className="op-label text-muted-foreground">found</dt>
                <dd className="mt-0.5 [&>p]:mt-0">
                  <Md prefix={`${id}-found`}>{pair.found}</Md>
                </dd>
              </div>
              <div>
                <dt className="op-label text-muted-foreground">why it broke recognition</dt>
                <dd className="mt-0.5 [&>p]:mt-0 [&>p]:text-muted-foreground">
                  <Md prefix={`${id}-broke`}>{pair.broke}</Md>
                </dd>
              </div>
              <div>
                <dt className="op-label text-muted-foreground">the fix</dt>
                <dd className="mt-0.5 flex items-start gap-2">
                  <span aria-hidden className="mt-0.5 w-3 shrink-0 text-center text-success">●</span>
                  <div className="min-w-0 flex-1 [&>p]:mt-0">
                    <Md prefix={`${id}-fix`}>{pair.fix}</Md>
                  </div>
                </dd>
              </div>
            </dl>
            <p className="mt-3 font-mono text-[11px] text-muted-foreground">{pair.source}</p>
          </section>
        )
      })}
    </div>
  )
}

// ── before you ship ────────────────────────────────────────────────────

const SHIP_COMMANDS: readonly (readonly [string, string])[] = [
  ['bun run lint', 'tsc --noEmit plus scripts/audit-records.mjs. Must be clean before any hand-back.'],
  ['bun run audit:records', 'The record-recipe audit alone, when you want the failure without the type check.'],
  ['bun run e2e', 'The whole Playwright suite (~1 min): overflow at 390 and 1440, keyboard, drop focus, axe in light and dark, visual baselines.'],
  ['bunx playwright test e2e/a11y.spec.ts', 'Just axe. The KNOWN map at the top of the file is empty and must stay empty.'],
  ['bun run e2e:update', 'Rewrite visual baselines — then look at the diff before committing it. Only from a quiet dev server.'],
]

const DEV_WARNINGS: readonly (readonly [string, string])[] = [
  ['[record recipe] Lede … has N facts', 'Fewer than three facts. A lede with only a sentence is a headline. Move the values the reader wants out of the aside and into `facts`.'],
  ['[record recipe] Detail with a lede has no meta', 'The meta places the record (id · project · environment). Without it the aside has to, and the aside is reference, not identity.'],
  ['[record recipe] Detail with a lede has no status', 'Every record has a verdict, and "Nothing to do: …" with the fact that proves it is a verdict.'],
  ['[record recipe] Detail "…" is a record page … with no lede', 'A verdict, a meta line and `Columns`, but nothing raised: the eye lands on the tabs. Add the Lede.'],
]

const SHIP_CHECKS: readonly (readonly [string, string])[] = [
  ['no dead controls', 'Every filter filters, every Segmented switches, every Kbd badge has a handler, every link has a typed destination. `href="#"` with a `preventDefault` is not one of the three endings.'],
  ['colour only beside a glyph and a word', 'Nothing carries a state tone on its own. No Tailwind palette literal, no hex, no second hue.'],
  ['one raise, no cards', 'Exactly one `.op-raise` on the screen. Frames around groups, ink rules between sections.'],
  ['390 and 1440', 'No horizontal document scroll at either width. Actions scroll sideways, ledger rows render `mobile` with the primary action.'],
  ['light and dark', 'Dark is a second set of token values. Look at the screen in both.'],
  ['axe clean', 'No new serious or critical violation, in either theme.'],
  ['the doc changed too', 'If you changed a rule, it changed in brand-guidelines.md, design-system-handoff.md, docs/RULES.md and on the reference page in the same commit.'],
]

function BeforeShipSection() {
  return (
    <div>
      <p className="op-prose max-w-[72ch] text-sm">
        Two lists and two commands. The eight record rules below are the only ones a script can check; the
        rest is what a reviewer will ask.
      </p>

      <h3 id="ship--the-eight-record-rules" className="op-h3 mt-8 scroll-mt-16 border-t pt-6">
        The eight record rules, and how they are enforced
      </h3>
      <Md prefix="ship-checklist">{HANDOFF_CHECKLIST}</Md>

      <h3 id="ship--what-a-reviewer-asks" className="op-h3 mt-10 scroll-mt-16 border-t pt-6">
        What a reviewer asks
      </h3>
      <ul className="op-rows mt-4 border">
        {SHIP_CHECKS.map(([check, what]) => (
          <li key={check} className="min-w-0 px-3 py-2 text-sm">
            <span className="flex items-baseline gap-2">
              <span aria-hidden className="w-3 shrink-0 text-center text-muted-foreground">○</span>
              <span className="font-medium">{check}</span>
            </span>
            <div className="pl-5 [&>p]:mt-0.5 [&>p]:text-xs [&>p]:text-muted-foreground">
              <Md prefix={`ship-check-${slug(check)}`}>{what}</Md>
            </div>
          </li>
        ))}
      </ul>

      <h3 id="ship--run-it" className="op-h3 mt-10 scroll-mt-16 border-t pt-6">
        Run it
      </h3>
      <ul className="op-rows mt-4 border">
        {SHIP_COMMANDS.map(([cmd, what]) => (
          <li key={cmd} className="min-w-0 px-3 py-2">
            <span className="flex items-center gap-3">
              <code className="font-mono text-xs">{cmd}</code>
              <span className="ml-auto">
                <CopyText value={cmd} />
              </span>
            </span>
            <span className="op-prose mt-0.5 block text-xs text-muted-foreground">{what}</span>
          </li>
        ))}
      </ul>

      <h3 id="ship--what-the-dev-warnings-mean" className="op-h3 mt-10 scroll-mt-16 border-t pt-6">
        What the dev warnings mean
      </h3>
      <p className="op-prose mt-2 max-w-[72ch] text-sm text-muted-foreground">
        These print to the browser console in dev only. They name the rule and the section, and they are the
        cheapest of the three gates — the audit script and the review are the other two.
      </p>
      <ul className="op-rows mt-4 border">
        {DEV_WARNINGS.map(([warning, what]) => (
          <li key={warning} className="min-w-0 px-3 py-2">
            <code className="block font-mono text-[11px] break-words">{warning}</code>
            <div className="[&>p]:mt-0.5 [&>p]:text-xs [&>p]:text-muted-foreground">
              <Md prefix={`ship-warn-${slug(warning).slice(0, 24)}`}>{what}</Md>
            </div>
          </li>
        ))}
      </ul>

      <h3 id="ship--banned" className="op-h3 mt-10 scroll-mt-16 border-t pt-6">
        Banned outright
      </h3>
      <Md prefix="ship-banned">{HANDOFF_BANNED}</Md>

      <h3 id="ship--responsive" className="op-h3 mt-10 scroll-mt-16 border-t pt-6">
        Responsive rules the sweep checks
      </h3>
      <Md prefix="ship-responsive">{HANDOFF_RESPONSIVE}</Md>
    </div>
  )
}

// ── rules for agents ───────────────────────────────────────────────────

function RulesForAgents() {
  return (
    <div className="mt-10 border-t pt-6">
      <h3 id="tooling--rules-for-agents" className="op-h3 scroll-mt-16 flex flex-wrap items-baseline gap-3">
        Rules for agents
        <span className="ml-auto">
          <CopyText value={rulesMd} label="copy docs/RULES.md" />
        </span>
      </h3>
      <p className="op-prose mt-2 max-w-[72ch] text-sm text-muted-foreground">
        <code className="font-mono">docs/RULES.md</code> is the compact, imperative digest of the two source
        documents, written to be pasted into a coding agent's context before it builds a screen. It is
        rendered here so it can be read and reviewed, and it is not authoritative:{' '}
        <a href="#brand" className="op-status">brand-guidelines.md</a> and the handoff win, and a change to a
        rule changes all three in one commit.
      </p>
      <Md prefix="tooling-rules">{RULES_BODY}</Md>
    </div>
  )
}

// ── sections ───────────────────────────────────────────────────────────

type Section = {
  id: string
  label: string
  /** Where the words come from, said on the page so the next editor knows what to edit. */
  source: string
  body: ReactNode
  /** Rendered markdown; its headings become search-index entries and the "on this page" list. */
  md: string[]
  /** Headings this section writes itself rather than rendering from a document. */
  extra?: readonly (readonly [string, string])[]
}

const SECTIONS: readonly Section[] = [
  {
    id: 'start',
    label: 'Start here',
    source: 'design-system-handoff.md §1–§3 · brand-guidelines.md §0',
    md: [HANDOFF_WHAT, BRAND_POSITIONING],
    body: (
      <>
        <Md prefix="start">{HANDOFF_WHAT}</Md>
        <Md prefix="start">{BRAND_POSITIONING}</Md>
      </>
    ),
  },
  {
    id: 'signature',
    label: 'Signature',
    source: 'brand-guidelines.md §1–§6, rendered with the primitives',
    md: [],
    extra: SIGNATURE.map((t, i) => [SIGNATURE_IDS[i], t.title] as const),
    body: <SignatureSection />,
  },
  {
    id: 'build',
    label: 'Build a screen',
    source: 'design-system-handoff.md §7 (layouts, axes) · skeletons for @temps-sdk/op',
    md: [],
    extra: [
      ['build--1-pick-the-template', '1. Pick the template'],
      ['build--2-paste-the-skeleton', '2. Paste the skeleton'],
      ['build--3-cover-the-states', '3. Cover the states'],
      ['build--4-put-each-fact-in-one-place', '4. Put each fact in one place'],
    ],
    body: <BuildScreenSection />,
  },
  {
    id: 'ship',
    label: 'Before you ship',
    source: 'design-system-handoff.md §7 checklist, §7c, §13 · the run commands from §0',
    // The slices here render under their own prefixes (`ship-checklist`, …)
    // so the section writes its own index entries below instead.
    md: [],
    extra: [
      ['ship--the-eight-record-rules', 'The eight record rules'],
      ['ship--what-a-reviewer-asks', 'What a reviewer asks'],
      ['ship--run-it', 'Run it'],
      ['ship--what-the-dev-warnings-mean', 'What the dev warnings mean'],
      ['ship--banned', 'Banned outright'],
      ['ship--responsive', 'Responsive rules the sweep checks'],
    ],
    body: <BeforeShipSection />,
  },
  {
    id: 'brand',
    label: 'Brand',
    source: 'brand-guidelines.md §1–§5',
    md: [BRAND_DIRECTION],
    body: <Md prefix="brand">{BRAND_DIRECTION}</Md>,
  },
  {
    id: 'taste',
    label: 'Taste',
    source: "brand-guidelines.md §6 (one anchor per rule) and §7",
    md: [BRAND_DO_DONT],
    body: (
      <>
        <Md prefix="taste-intro">{TASTE_INTRO}</Md>
        <TasteEntries />
        <div className="mt-10 border-t pt-6">
          <Md prefix="taste">{BRAND_DO_DONT}</Md>
        </div>
      </>
    ),
  },
  {
    id: 'dodont',
    label: 'Do / Don’t',
    source: 'ux-audit-2026-09-06.md · brand-guidelines.md §6',
    md: [],
    extra: PAIRS.map((p, i) => [PAIR_IDS[i], p.title] as const),
    body: <DoDontSection />,
  },
  {
    id: 'tokens',
    label: 'Tokens',
    source: 'design-system-handoff.md §4 · swatches read live',
    md: [HANDOFF_TOKENS],
    body: <Md prefix="tokens">{HANDOFF_TOKENS}</Md>,
  },
  {
    id: 'status',
    label: 'Status vocabulary',
    source: 'design-system-handoff.md §5 · glyphs rendered live',
    md: [HANDOFF_STATUS],
    body: <Md prefix="status">{HANDOFF_STATUS}</Md>,
  },
  {
    id: 'templates',
    label: 'Templates & the record recipe',
    source: 'design-system-handoff.md §7, incl. the enforced checklist',
    md: [HANDOFF_TEMPLATES],
    body: <Md prefix="templates">{HANDOFF_TEMPLATES}</Md>,
  },
  {
    id: 'components',
    label: 'Components',
    source: 'design-system-handoff.md §6 · each name links to /op-components',
    md: [HANDOFF_COMPONENTS],
    body: <Md prefix="components">{HANDOFF_COMPONENTS}</Md>,
  },
  {
    id: 'keyboard',
    label: 'Keyboard',
    source: 'design-system-handoff.md §9 · badges rendered live',
    md: [HANDOFF_KEYBOARD],
    body: <Md prefix="keyboard">{HANDOFF_KEYBOARD}</Md>,
  },
  {
    id: 'surfaces',
    label: 'Surfaces',
    source: 'design-system-handoff.md §7b · each surface links to its view',
    md: [HANDOFF_SURFACES],
    body: <Md prefix="surfaces">{HANDOFF_SURFACES}</Md>,
  },
  {
    id: 'tooling',
    label: 'Tooling',
    source: 'design-system-handoff.md §0 · docs/RULES.md',
    md: [HANDOFF_RUN],
    extra: [['tooling--rules-for-agents', 'Rules for agents']],
    body: (
      <>
        <Md prefix="tooling">{HANDOFF_RUN}</Md>
        <RulesForAgents />
      </>
    ),
  },
  {
    id: 'open',
    label: 'Open questions',
    source: 'design-system-handoff.md §15 · ux-audit-2026-09-06.md ⏳ and ◐ items',
    md: [HANDOFF_OPEN],
    body: (
      <>
        <Md prefix="open">{HANDOFF_OPEN}</Md>
        <h3 id="open--deferred-in-the-ux-audit" className="op-h3 mt-10 scroll-mt-16">
          Deferred in the UX audit (⏳)
        </h3>
        <Md prefix="open-deferred">{AUDIT_DEFERRED}</Md>
        <h3 id="open--partly-done-in-the-ux-audit" className="op-h3 mt-8 scroll-mt-16">
          Partly done in the UX audit (◐)
        </h3>
        <Md prefix="open-partial">{AUDIT_PARTIAL}</Md>
      </>
    ),
  },
]

// ── search index ───────────────────────────────────────────────────────

type Entry = { id: string; text: string; section: string; sectionLabel: string; kind: 'heading' | 'taste' }

const INDEX: Entry[] = SECTIONS.flatMap((s) => {
  // Document headings first, then the headings the section writes itself:
  // that is the order they appear in, so "on this page" reads down the page.
  const fromMd = [
    ...s.md.flatMap((md) =>
      headings(md).map((h) => ({
        id: `${s.id}--${slug(h.text)}`,
        text: h.text,
        section: s.id,
        sectionLabel: s.label,
        kind: 'heading' as const,
      })),
    ),
    ...(s.extra ?? []).map(([id, text]) => ({
      id,
      text,
      section: s.id,
      sectionLabel: s.label,
      kind: 'heading' as const,
    })),
  ]
  if (s.id !== 'taste') return fromMd
  return [
    ...TASTE_ENTRIES.map((b, i) => ({
      id: TASTE_IDS[i],
      text: b.title,
      section: 'taste',
      sectionLabel: s.label,
      kind: 'taste' as const,
    })),
    ...fromMd,
  ]
})

/** Body text for each taste rule, so a search for a word inside a rule finds it. */
const TASTE_BODY = new Map(TASTE_ENTRIES.map((b, i) => [TASTE_IDS[i], b.body.toLowerCase()]))

function search(q: string): Entry[] {
  const needle = q.trim().toLowerCase()
  if (!needle) return []
  return INDEX.filter(
    (e) => e.text.toLowerCase().includes(needle) || (TASTE_BODY.get(e.id)?.includes(needle) ?? false),
  ).slice(0, 60)
}

// ── page ───────────────────────────────────────────────────────────────

/**
 * The guide's section list, id and label only: what the shell's left rail
 * draws. Exported from here so there is one list, and the numbering in the
 * rail is the numbering of the sections below.
 */
export const GUIDE_NAV: readonly { id: string; label: string }[] = SECTIONS.map(({ id, label }) => ({ id, label }))

/**
 * Which section a hash names. Takes the hash rather than reading it so the
 * shell, which tracks `hashchange` itself, can ask the same question.
 */
export function sectionFromHash(hash?: string): string {
  const source = hash ?? (typeof window === 'undefined' ? '' : window.location.hash)
  const raw = decodeURIComponent(source.replace(/^#/, ''))
  const id = raw.split('--')[0]
  return SECTIONS.some((s) => s.id === id) ? id : SECTIONS[0].id
}

export function GuidePage() {
  const [current, setCurrent] = useState<string>(() => sectionFromHash())
  // One filter box for the whole app: the shell owns it and `/` focuses it,
  // the guide reads the same string to search headings and taste rules.
  const { query: q, setQuery: setQ } = useShell()

  // The hash is the only source of truth for "which section": every rail
  // link, prev/next and heading anchor is a real href, so back and forward
  // work and a deep link opens the right section scrolled to the heading.
  useEffect(() => {
    const apply = () => {
      setQ('')
      setCurrent(sectionFromHash())
      const raw = decodeURIComponent(window.location.hash.replace(/^#/, ''))
      if (raw.includes('--')) {
        requestAnimationFrame(() => document.getElementById(raw)?.scrollIntoView({ block: 'start' }))
      } else {
        window.scrollTo(0, 0)
      }
    }
    apply()
    window.addEventListener('hashchange', apply)
    return () => window.removeEventListener('hashchange', apply)
  }, [])

  const index = SECTIONS.findIndex((s) => s.id === current)
  const section = SECTIONS[index] ?? SECTIONS[0]
  const prev = SECTIONS[index - 1]
  const next = SECTIONS[index + 1]
  const results = useMemo(() => search(q), [q])
  const onThisPage = useMemo(
    () => INDEX.filter((e) => e.section === section.id).slice(0, 40),
    [section.id],
  )

  // The right rail is the shell's; the guide tells it what this section holds.
  useDocToc(onThisPage)

  return (
    <>
      {q.trim() ? (
        <div className="min-w-0">
          <h2 className="op-h2 text-[1.25rem]">
            {results.length} {results.length === 1 ? 'match' : 'matches'} for “{q.trim()}”
          </h2>
          <p className="op-prose mt-2 max-w-[72ch] text-sm text-muted-foreground">
            Headings and taste rules. Press <Kbd keys="esc" /> in the box to clear it.
          </p>
          {results.length === 0 ? (
            <p className="mt-4 text-sm text-muted-foreground">
              Nothing matches. The guide indexes headings and the 23 taste rules, not every sentence.
            </p>
          ) : (
            <ul className="op-rows mt-4 border">
              {results.map((r) => (
                <li key={`${r.section}-${r.id}`}>
                  <a
                    href={`#${r.id}`}
                    onClick={() => setQ('')}
                    className="flex flex-wrap items-baseline gap-x-3 gap-y-1 px-3 py-2 text-sm hover:bg-muted"
                  >
                    <span className="min-w-0">{r.text}</span>
                    <span className="op-label ml-auto shrink-0 text-muted-foreground">
                      {r.kind === 'taste' ? 'taste rule' : r.sectionLabel}
                    </span>
                  </a>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : (
        <article className="min-w-0">
          <p className="op-label text-muted-foreground">
            {index + 1} of {SECTIONS.length}
          </p>
          <h2 className="op-h2 mt-1 text-[1.5rem]">{section.label}</h2>
          <p className="op-prose mt-1 max-w-[72ch] font-mono text-xs text-muted-foreground">
            source · {section.source}
          </p>
          <div className="mt-6 min-w-0">{section.body}</div>

          <nav
            aria-label="Previous and next section"
            className="mt-12 flex flex-wrap items-baseline justify-between gap-4 border-t pt-4 text-sm"
          >
            {prev ? (
              <a href={`#${prev.id}`} className="op-status">
                ← {prev.label}
              </a>
            ) : (
              <span />
            )}
            {next ? (
              <a href={`#${next.id}`} className="op-status ml-auto">
                {next.label} →
              </a>
            ) : (
              <span />
            )}
          </nav>
        </article>
      )}
    </>
  )
}
