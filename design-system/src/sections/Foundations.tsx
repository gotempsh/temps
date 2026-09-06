// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { CSSProperties, ReactNode } from 'react'
import { Link } from 'react-router'
import { Block, Demo, DocPage, Rule } from '@/components/op-doc'
import { Kbd, Num, Status, Section, Columns, Lede, KeyValue, Segmented, Timeline, type State } from '@/components/op'
import { Inbox, MailCheck, Send } from 'lucide-react'

/* ────────────────────────────────────────────────────────────────────────
   /foundations — what v5 is made of, before any component exists: the
   type hierarchy, the paper-and-ink token table, the five status states,
   density and rhythm, the frozen radius/border/motion decisions, the two
   faces, and the phone rules.

   Everything here is rendered live under the v5 skin and cites only
   classes and tokens that exist in src/globals.css. Where the handoff
   document and the code disagree, the code wins and the disagreement is
   noted in a muted line.
   ──────────────────────────────────────────────────────────────────────── */

const TOC = [
  ['type', 'Type'],
  ['paper-ink', 'Paper and ink'],
  ['colour', 'Colour is status'],
  ['density', 'Density and rhythm'],
  ['anatomy', 'Page anatomy'],
  ['radius', 'Radius, borders, motion'],
  ['fonts', 'Fonts'],
  ['responsive', 'Responsive'],
] as const

/** A discrepancy between the handoff document and what globals.css actually does. */
function Note({ children }: { children: ReactNode }) {
  return <p className="op-prose text-[11px] text-muted-foreground">{children}</p>
}

// ── type ───────────────────────────────────────────────────────────────

const TIERS: readonly (readonly [string, string, string, string])[] = [
  ['op-display', '800', 'Landing hero. One per page, never in the console.', 'Own your deploys'],
  ['op-h1', '700', 'Landing major section title.', 'Everything the box does'],
  ['op-h2', '600', 'Minor section or panel title. Largest tier in the console.', 'Deployments'],
  ['op-h3', '600', 'Item title in a grid, settings section title.', 'api-gateway'],
  ['op-title', '700', 'Console page title. The one 700 line on a screen.', 'Projects'],
  ['op-lead', '400', 'The sentence under a title. Muted.', 'Six projects, one failing health checks.'],
  ['op-label', '500', 'Eyebrow, column header, key badge. Uppercase, tracked.', 'last deploy'],
]

function TypeRow({ cls, weight, use, sample }: { cls: string; weight: string; use: string; sample: string }) {
  const capped = cls === 'op-display' ? ({ fontSize: '3.25rem' } as CSSProperties) : undefined
  return (
    <div className="min-w-0 border-t py-4 first:border-t-0">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="font-mono text-[11px]">.{cls}</span>
        <span className="op-label">weight {weight}</span>
      </div>
      <div className={`${cls} mt-2 min-w-0 break-words`} style={capped}>{sample}</div>
      <p className="op-prose mt-2 text-xs text-muted-foreground">{use}</p>
    </div>
  )
}

// ── paper and ink ──────────────────────────────────────────────────────

/** Tokens that describe the surface. `value` is the CSS var the swatch paints with. */
const SURFACE: readonly (readonly [string, string, string])[] = [
  ['--background', 'var(--background)', 'paper'],
  ['--foreground', 'var(--foreground)', 'ink, and every border'],
  ['--muted', 'var(--muted)', 'section tone, hover, sampled band'],
  ['--muted-foreground', 'var(--muted-foreground)', 'secondary text, idle glyphs'],
  ['--border', 'var(--border)', 'equals --foreground; 1px, everywhere'],
  ['--popover', 'var(--popover)', 'menus, dialogs, the picker list'],
  ['--op-inset', 'var(--op-inset)', 'command echo, log panes'],
  ['--op-rule-soft', 'var(--op-rule-soft)', '16% ink; ledger row dividers only'],
  ['--chart-1', 'var(--chart-1)', 'the plotted line: ink'],
  ['--chart-2', 'var(--chart-2)', 'the comparison line: mid grey'],
]

function SwatchColumn({ label, dark }: { label: string; dark?: boolean }) {
  const body = (
    <div className="operator ink v4 v5 min-w-0 border bg-background text-foreground">
      <p className="op-label border-b px-3 py-2">{label}</p>
      <div className="op-rows">
        {SURFACE.map(([name, value, use]) => (
          <div key={name} className="flex min-w-0 items-center gap-3 px-3 py-2">
            <span
              aria-hidden
              className="h-6 w-8 shrink-0 border"
              style={{ background: value }}
            />
            <span className="min-w-0 flex-1">
              <span className="block truncate font-mono text-[11px]">{name}</span>
              <span className="block truncate text-[11px] text-muted-foreground">{use}</span>
            </span>
          </div>
        ))}
      </div>
    </div>
  )
  return dark ? <div className="dark min-w-0">{body}</div> : body
}

// ── colour is status ───────────────────────────────────────────────────

const STATES: readonly (readonly [State, string, string])[] = [
  ['ok', 'success', 'healthy, passing, deployed'],
  ['warn', 'warning', 'degraded, above threshold, expiring'],
  ['error', 'destructive', 'failing, unreachable'],
  ['idle', 'muted-foreground', 'not deployed, not configured, nothing yet'],
  ['sampled', 'muted-foreground', 'head-sampled past the plan allowance'],
]

// ── density and rhythm ─────────────────────────────────────────────────

const LEDGER_COLS = '1.4fr 1fr 90px'

function MiniLedger({ density }: { density: 'comfortable' | 'dense' }) {
  return (
    <div className="operator ink v4 v5 min-w-0 border bg-background text-foreground" data-density={density}>
      <div className="op-row op-cols grid grid-cols-[1fr_auto] items-center gap-2 border-b" style={{ '--cols': LEDGER_COLS } as CSSProperties}>
        <span className="op-label truncate">project</span>
        <span className="op-label hidden truncate md:block">status</span>
        <span className="op-label hidden truncate text-right md:block">requests</span>
      </div>
      <div className="op-rows">
        <div className="op-row op-cols grid grid-cols-[1fr_auto] items-center gap-2 text-sm" style={{ '--cols': LEDGER_COLS } as CSSProperties}>
          <span className="min-w-0 truncate font-medium">api-gateway</span>
          <Status state="warn" label="error rate above 0.5%" className="hidden min-w-0 truncate md:inline-flex" />
          <Num value={30800} className="hidden text-right md:block" />
          <Status state="warn" label="" className="md:hidden" />
        </div>
        <div className="op-row op-cols grid grid-cols-[1fr_auto] items-center gap-2 text-sm" style={{ '--cols': LEDGER_COLS } as CSSProperties}>
          <span className="min-w-0 truncate font-medium">docs</span>
          <Status state="ok" label="production" className="hidden min-w-0 truncate md:inline-flex" />
          <Num value={2210} className="hidden text-right md:block" />
          <Status state="ok" label="" className="md:hidden" />
        </div>
      </div>
    </div>
  )
}

// ── fonts ──────────────────────────────────────────────────────────────

const MONO_IS_MANDATORY: readonly (readonly [string, string])[] = [
  ['values and numbers', '30,800 · 184ms · 0.61% · –'],
  ['identifiers', 'dep_91a · sbx_9f3 · 3f9c1e7a8b2d4f60'],
  ['deploy tags and refs', 'main · v0.1.0 · temps/sandbox:node22'],
  ['commands', 'temps deploy promote v0.1.0 --to production'],
  ['page title meta', 'production · dep_91a · main'],
]

// ── responsive ─────────────────────────────────────────────────────────

const PHONE_RULES: readonly string[] = [
  'Ledger rows hide their cells below md and render `mobile` instead — and the mobile node carries the row’s primary action, because a phone user cannot reach a desktop-only cell.',
  'Rows are fixed height on desktop (--row-h) and grow with their content below 768px. Never rely on the desktop height for multi-line content.',
  'Tab strips, segmented controls and range pickers scroll horizontally with .op-scroll-x instead of wrapping. Key badges in tabs hide below sm.',
  'Action groups wrap and go full width below sm: w-full sm:w-auto sm:ml-auto. Never ml-auto alone on three or more buttons.',
  'Grids using .op-cols collapse to grid-cols-[1fr_auto] below md. Mark every secondary cell hidden md:block and fold what matters into the first cell as a second line.',
  'The trace waterfall keeps its bar on phones, full width under the span name, with the duration on the name line.',
  'The status line stays one line and truncates. The quiet tail is the first thing lost, which is correct.',
]

export function FoundationsPage() {
  return (
    <DocPage
      eyebrow="foundations · what v5 is made of"
      intro={<>
        The layer under every component: type hierarchy, paper-and-ink tokens, the five status
        states, density, and the frozen decisions about radius, borders and motion. Applied by
        putting <span className="font-mono">operator ink v4 v5</span> on a root element — this page
        included. The components built on top are on{' '}
        <Link to="/op-components" className="underline underline-offset-4">/op-components</Link>;
        assembled into a console on <Link to="/v5" className="underline underline-offset-4">/v5</Link>.
      </>}
      toc={TOC}
    >
      <Block
        id="type"
        title="Type"
        api={`.op-display  800   landing hero, one per page
.op-h1       700   landing section title
.op-h2       600   panel title · console max
.op-h3       600   item title
.op-title    700   console page title
.op-lead     400   the sentence under it
.op-label    500   eyebrow · column header
body         400   everything else
font-mono          values · ids · commands`}
        rule={<>
          <p>Weight is the signal. A reader should know what tier they are on without measuring anything, so each tier has one fixed weight and there is no title at weight 500.</p>
          <p>One 800-weight line per landing page and one 700-weight line per console screen. If two things are the biggest, neither is.</p>
          <Rule state="ok">One <code>.op-display</code> on the landing, one <code>.op-title</code> on a console screen, everything below at 600 or 400.</Rule>
          <Rule state="error">A second hero-sized headline, or a section title set at weight 500 so it &ldquo;does not shout&rdquo;.</Rule>
        </>}
      >
        <Demo label="the hierarchy, live">
          <div className="min-w-0">
            {TIERS.map(([cls, weight, use, sample]) => (
              <TypeRow key={cls} cls={cls} weight={weight} use={use} sample={sample} />
            ))}
            <div className="min-w-0 border-t py-4">
              <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
                <span className="font-mono text-[11px]">body</span>
                <span className="op-label">weight 400</span>
              </div>
              <p className="op-prose mt-2 text-sm">Whitespace is spent between sections, not inside tables. Body copy is the sans face at 400.</p>
              <p className="op-prose mt-2 text-xs text-muted-foreground">Everything that is not a title, a label or a value.</p>
            </div>
            <div className="min-w-0 border-t py-4">
              <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
                <span className="font-mono text-[11px]">font-mono</span>
                <span className="op-label">tabular</span>
              </div>
              <p className="mt-2 font-mono text-sm tabular-nums">30,800 · 184ms · 0.61% · dep_91a</p>
              <p className="op-prose mt-2 text-xs text-muted-foreground">Values, ids, deploy tags and commands. Never a paragraph.</p>
            </div>
          </div>
        </Demo>
        <Note>
          <span className="font-mono">.op-display</span> is capped here at 3.25rem so the block fits the page; its real size is{' '}
          <span className="font-mono">clamp(2.75rem, 8vw, 6rem)</span>. The handoff type table (§4) omits{' '}
          <span className="font-mono">.op-title</span>; it exists in globals.css at 1.25rem / 700 and §7 describes it, so it is documented here.
        </Note>
      </Block>

      <Block
        id="paper-ink"
        title="Paper and ink"
        api={`--background       paper
--foreground       ink = --border
--muted            tone · hover · sampled band
--muted-foreground secondary text
--popover          menus and dialogs
--op-inset         log and echo panes
--op-rule-soft     16% ink, row dividers
--chart-1/2        ink · mid grey`}
        rule={<>
          <p>Warm off-white paper, near-black ink, and nothing else structural. Dark mode inverts the same pair rather than introducing a grey scale.</p>
          <p>Every border is ink at 1px. The one exception is the row divider inside a ledger, which uses <code>--op-rule-soft</code> so a dense table does not read as a grid of boxes.</p>
          <Rule state="ok">Borders and whitespace separate surfaces. One <code>.op-raise</code> per screen — the thing the reader is meant to act on.</Rule>
          <Rule state="error">Cards as layout, drop shadows for depth, or a grey hairline that is neither ink nor <code>--op-rule-soft</code>.</Rule>
        </>}
      >
        <Demo label="tokens · light and dark">
          <div className="grid min-w-0 gap-4 sm:grid-cols-2">
            <SwatchColumn label="light" />
            <SwatchColumn label="dark" dark />
          </div>
        </Demo>
        <Demo label="the one raised element">
          <div className="op-raise min-w-0 p-3">
            <p className="op-h3">deploy dep_91a</p>
            <p className="op-prose mt-1 text-sm text-muted-foreground">Raised means &ldquo;act on this&rdquo;. A second one on the same screen means neither is.</p>
          </div>
        </Demo>
        <Demo label="inset pane">
          <pre className="op-inset min-w-0 overflow-auto border p-3 font-mono text-[11px] leading-5">{`$ temps deploy promote v0.1.0 --to production
→ resolving v0.1.0
→ production is now dep_91a`}</pre>
        </Demo>
        <Note>
          <span className="font-mono">--op-inset</span> and <span className="font-mono">--op-rule-soft</span> are not registered in the{' '}
          <span className="font-mono">@theme inline</span> block, so there is no <span className="font-mono">bg-op-inset</span> utility.
          Use the <span className="font-mono">.op-inset</span> / <span className="font-mono">.op-rows</span> classes, or the raw var.
        </Note>
      </Block>

      <Block
        id="colour"
        title="Colour is status"
        api={`<Status state="warn" label="above 0.5%" />

ok      ●  --success
warn    ◐  --warning
error   ×  --destructive
idle    ○  --muted-foreground
sampled ◌  --muted-foreground`}
        rule={<>
          <p>Green, amber and red appear only through <code>Status</code>, always as a glyph next to a word. A coloured dot with no word, or a coloured number with no glyph, is not a state — it is decoration that a colour-blind operator cannot read.</p>
          <p><code>sampled</code> exists because pricing promises telemetry past the allowance is head-sampled and &ldquo;the console says so; it is never silently dropped&rdquo;. That promise is a UI contract.</p>
          <p>The landing accent <code>signal</code> lives on <code>--primary</code> via <code>data-accent</code> and appears once per viewport, on the primary call to action. It never appears in the console.</p>
          <Rule state="ok">Glyph plus word, through the real <code>Status</code> component, ranked by <code>STATE_RANK</code>.</Rule>
          <Rule state="error">A bare coloured dot, a red number with no state, a second hue &ldquo;for interest&rdquo;, or the accent anywhere in the console.</Rule>
        </>}
      >
        <Demo label="the five states">
          <div className="min-w-0 border">
            <div className="op-rows">
              {STATES.map(([state, token, meaning]) => (
                <div key={state} className="flex min-w-0 flex-wrap items-baseline gap-x-4 gap-y-1 px-3 py-2 text-sm">
                  <Status state={state} label={state} className="w-24 shrink-0" />
                  <span className="font-mono text-[11px] text-muted-foreground">{token}</span>
                  <span className="op-prose min-w-0 flex-1 text-xs text-muted-foreground">{meaning}</span>
                </div>
              ))}
            </div>
          </div>
        </Demo>
        <Demo label="in a sentence, which is the only place colour belongs">
          <p className="min-w-0 text-sm">
            <Status state="error" label="billing-worker is failing health checks." />
          </p>
        </Demo>
        <Demo label="the landing accent · once per viewport, landing only">
          <div className="operator ink v4 v5 min-w-0 border bg-background p-4" data-accent="signal">
            <p className="op-label">data-accent=&quot;signal&quot;</p>
            <div className="mt-3 flex flex-wrap items-center gap-3">
              <button type="button" className="op-primary inline-flex h-8 items-center gap-2 bg-primary px-3 text-sm text-primary-foreground">
                Deploy <Kbd keys={['⌘', '⏎']} />
              </button>
              <span className="op-prose text-xs text-muted-foreground">Only <code>--primary</code> and <code>--primary-foreground</code> change. Status colours and the focus ring are untouched.</span>
            </div>
          </div>
        </Demo>
        <Note>
          globals.css defines four accents (<span className="font-mono">signal</span>, <span className="font-mono">moss</span>,{' '}
          <span className="font-mono">cobalt</span>, <span className="font-mono">violet</span>) and sets no default; the decision recorded in the
          handoff is <span className="font-mono">signal</span>, and the console sets no <span className="font-mono">data-accent</span> at all.
        </Note>
      </Block>

      <Block
        id="density"
        title="Density and rhythm"
        api={`data-density="comfortable"  --row-h: 2.25rem
data-density="dense"        --row-h: 1.75rem

.op-row     height: var(--row-h)
.op-rows    children split by --op-rule-soft
.op-sticky        pinned status line
.op-sticky-bottom pinned save bar
.op-section       landing only · 5rem, minor 3.5rem
.op-block         console section · see Page anatomy`}
        rule={<>
          <p>Dense by default. Whitespace is spent between sections, not inside tables — a table that breathes is a table the operator has to scroll.</p>
          <p>Two settings, set with <code>data-density</code> on the shell, toggled with <Kbd keys="d" /> and remembered. <code>--row-h</code> is the only thing that changes; padding follows through <code>--cell-px</code>.</p>
          <p>Spacing in use: <code>gap-2</code> within a row, <code>space-y-6</code> between blocks on a screen, between landing sections from <code>.op-section</code> (5rem major, 3.5rem minor). Inside a console page the rhythm is the one fixed in <a href="#anatomy">Page anatomy</a>.</p>
          <Rule state="ok">The status line sticks under the header with <code>.op-sticky</code>; the settings save bar sticks to the bottom with <code>.op-sticky-bottom</code>, so the verdict and the commit are never scrolled away.</Rule>
          <Rule state="error">Padding a table to make it &ldquo;calmer&rdquo;, or a save button that only exists at the bottom of a long form.</Rule>
        </>}
      >
        <Demo label="comfortable · --row-h 2.25rem">
          <MiniLedger density="comfortable" />
        </Demo>
        <Demo label="dense · --row-h 1.75rem">
          <MiniLedger density="dense" />
        </Demo>
        <Demo label="sticky surfaces">
          <div className="min-w-0 space-y-3 text-sm">
            <p className="op-prose text-muted-foreground"><code className="font-mono">.op-sticky</code> — top 0, z 20, painted with <code className="font-mono">--background</code> so rows scroll under it.</p>
            <p className="op-prose text-muted-foreground"><code className="font-mono">.op-sticky-bottom</code> — bottom 0, z 20. The save bar that <Kbd keys={['⌘', 'S']} /> clicks, so pressed and disabled states stay honest.</p>
          </div>
        </Demo>
        <Note>
          <span className="font-mono">--row-h</span> and <span className="font-mono">data-density</span> live on the{' '}
          <span className="font-mono">.operator.ink.v4</span> block, not v5 — v5 layers on top of v4 and both classes are always set together.
        </Note>
      </Block>

      <Block
        id="anatomy"
        title="Page anatomy"
        api={`scale        4 · 8 · 12 · 16 · 20 · 24 · 32 px
tiers        title 700/20 · lede 600/18 + glyph · section 600/14
             row event word 500 · everything else 400 muted

<Lede state word>                the one .op-raise on the page
<Columns>                        .op-halves  main + 18rem aside at xl, max 76rem;
                                 below xl the aside stacks behind an ink rule
<Section title meta? action?>    .op-block  h2 600 14px + one body (mt 12px)
.op-block + .op-block            ink rule · 20px above · 20px below
<KeyValue rows compact?>         dl.op-kv framed · key 11rem muted (compact: key over value, 11px)
<Timeline items>                 ol.op-timeline framed · icon rail · label 500 · note · time right
.op-kv / .op-timeline > * + *    soft rule (--op-rule-soft)`}
        rule={<>
          <p>Hierarchy is made of shapes before it is made of type. The <code>Lede</code> is the one raised block on the page, so the eye lands there first; framed groups below it are the next things it can find; the type tiers (five, each at its own size) rank what is inside them. A page whose sections are all loose text at the same weight has no hierarchy no matter how the fonts are set.</p>
          <p>A record page is the thing itself first (content, with a 2-view <code>Segmented</code> when it has two faithful renderings), then what happened to it (a <code>Timeline</code>), and reference facts in the aside as a compact <code>KeyValue</code>. Each is a <code>Section</code>: a title at 600/14 and exactly one body.</p>
          <p>An event is drawn by an icon that says what kind of event it was, never by a coloured dot. The dot only says fine/not fine; the icon says what. Colour on the icon is reserved for failure and not-real. The page owns the vocabulary so the same event is always the same icon.</p>
          <Rule state="ok">Every distance comes from the scale: 8 inside a row, 12 between a title and its body, 20 around a section rule. Ink rules separate sections, soft rules separate rows, frames enclose groups, the one raise marks the lede. Nothing else draws a line.</Rule>
          <Rule state="error">Loose rows stretched across a wide screen, a bold line used as a heading, green dots as event markers, tabs or collapsed sections inside one record, or a page whose parts are all the same weight so nothing says where to start.</Rule>
        </>}
      >
        <Demo label="a record page · Lede → Columns(main: Content, Events · aside: Headers)">
          <div className="operator ink v4 v5 min-w-0 space-y-4 border bg-background p-4 text-sm">
            <h1 className="op-title">Your order shipped</h1>
            <Lede state="ok" word="delivered" facts={[{ k: 'to', v: 'dana@example.com', mono: true }, { k: 'from', v: 'orders@acme.sh', mono: true }, { k: 'provider', v: 'ses-eu', mono: true }, { k: 'took', v: '1.2s', mono: true }]}>09:41:07 · to dana@example.com · via ses</Lede>
            <Columns>
              <div>
                <Section title="Content" action={<Segmented options={[['html', 'html'], ['text', 'text']] as const} value="html" onChange={() => {}} className="h-7 [&>button]:h-7" />}>
                  <div className="border bg-background p-4"><p className="font-semibold">Your order shipped</p><p className="mt-2 text-muted-foreground">Track it at acme.sh/orders/48211.</p></div>
                </Section>
                <Section title="Events" meta="3 · last 09:41:07">
                  <Timeline items={[
                    { t: '09:41:02', label: 'queued', icon: <Inbox />, state: 'idle', note: 'accepted from api-gateway' },
                    { t: '09:41:03', label: 'sent', icon: <Send />, note: 'ses · eu-west-1 · 250 OK' },
                    { t: '09:41:07', label: 'delivered', icon: <MailCheck />, note: 'mx1.example.com' },
                  ]} />
                </Section>
              </div>
              <div>
                <Section title="Headers" meta="3">
                  <KeyValue compact rows={[
                    { k: 'message-id', v: '<9ea0.acme@mail.acme.sh>', copy: '<9ea0.acme@mail.acme.sh>' },
                    { k: 'from', v: 'orders@acme.sh' },
                    { k: 'dkim', v: 'pass', state: 'ok' },
                  ]} />
                </Section>
              </div>
            </Columns>
          </div>
        </Demo>
        <Note>
          The rules are CSS sibling selectors in globals.css (<span className="font-mono">.op-block + .op-block</span>,{' '}
          <span className="font-mono">.op-kv &gt; * + *</span>), so the first section and the first row never get one and no component takes a{' '}
          <span className="font-mono">first</span> prop. <span className="font-mono">.op-section</span> is the landing class and is unrelated.
        </Note>
      </Block>

      <Block
        id="radius"
        title="Radius, borders, motion"
        api={`--radius: 0.25rem      frozen
border: 1px solid var(--border)
.op-raise    3px hard shadow, no blur
.op-primary  2px shadow, translates on press

focus-visible:outline-2
focus-visible:-outline-offset-2
focus-visible:outline-ring

transition-duration: 100ms`}
        rule={<>
          <p>Radius is frozen at 0.25rem and set twice — once on <code>.operator.ink</code>, again on <code>.operator.ink.v5</code> — so a v5 root cannot inherit anything softer. Selects are squared off entirely.</p>
          <p>Borders are 1px ink. Depth is a hard offset shadow with no blur, and only <code>.op-raise</code> and <code>.op-primary</code> have one.</p>
          <p>Focus is a 2px inset outline in <code>--ring</code>, drawn inside the control so it never shifts layout: <code>focus-visible:outline-2 -outline-offset-2 outline-ring</code>. Tab into the button below to see it.</p>
          <p>Motion is 100ms and limited to transform, box-shadow, background-color and colour. Charts have no animation at all, and nothing has an entrance.</p>
          <Rule state="ok">Instant state changes, a hard shadow on the one thing to act on, focus drawn inside the control.</Rule>
          <Rule state="error">Blurred shadows, an entrance animation, an animated chart, or a focus style that moves the layout.</Rule>
        </>}
      >
        <Demo label="radius · one value, everywhere">
          <div className="flex min-w-0 flex-wrap items-end gap-4">
            {[['--radius', 'var(--radius)'], ['select', '0'], ['.op-raise', 'var(--radius)']].map(([label, r]) => (
              <div key={label} className="min-w-0">
                <div className="h-12 w-20 border bg-muted" style={{ borderRadius: r }} />
                <p className="mt-1 font-mono text-[11px]">{label}</p>
              </div>
            ))}
          </div>
        </Demo>
        <Demo label="depth · hard offset, no blur">
          <div className="flex min-w-0 flex-wrap items-center gap-6">
            <div className="op-raise flex h-12 w-32 items-center justify-center text-xs">.op-raise</div>
            <button type="button" className="op-primary inline-flex h-8 items-center bg-primary px-3 text-sm text-primary-foreground">.op-primary</button>
          </div>
        </Demo>
        <Demo label="focus · 2px inset outline, tab to it">
          <div className="flex min-w-0 flex-wrap items-center gap-3">
            <button type="button" className="inline-flex h-8 items-center border px-3 text-sm hover:bg-muted focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring">
              focusable
            </button>
            <span className="op-prose text-xs text-muted-foreground">The ring hue is the only blue in the system, and it exists on focus only.</span>
          </div>
        </Demo>
        <Note>
          Two things in the code do not match the prose. The base <span className="font-mono">.operator</span> block sets{' '}
          <span className="font-mono">transition-duration: 0s !important</span>; <span className="font-mono">.operator.ink</span> raises it back to
          100ms, so under this skin motion is 100ms as documented. And the shared shadcn{' '}
          <span className="font-mono">Button</span> primitive still focuses with an offset ring (
          <span className="font-mono">focus-visible:ring-2 ring-offset-2</span>), not the inset outline the op components use — the inset outline is
          the pattern to follow in new work.
        </Note>
      </Block>

      <Block
        id="fonts"
        title="Fonts"
        api={`--font-sans: 'Geist', …
--font-mono: 'Geist Mono', …

font-feature-settings: 'tnum' 1
.op-prose  → back to the sans face`}
        rule={<>
          <p>Two faces: Geist for everything a person reads as language, Geist Mono for everything a person reads as data. Tabular numerals are on for the whole skin, so a column of figures does not shimmer as it updates.</p>
          <p>Mono is mandatory wherever the reader compares or copies a string. It is banned for paragraphs — <code>.op-prose</code> exists to force wrapping copy back to the sans face.</p>
          <Rule state="ok">Mono for values, ids, deploy tags, commands and key badges; sans for every sentence.</Rule>
          <Rule state="error">A mono paragraph, or a metric set in the sans face so the digits jump between renders.</Rule>
        </>}
      >
        <Demo label="where mono is mandatory">
          <div className="min-w-0 border">
            <div className="op-rows">
              {MONO_IS_MANDATORY.map(([what, sample]) => (
                <div key={what} className="flex min-w-0 flex-col gap-1 px-3 py-2 sm:flex-row sm:items-baseline sm:gap-4">
                  <span className="op-label w-40 shrink-0">{what}</span>
                  <span className="min-w-0 truncate font-mono text-xs tabular-nums">{sample}</span>
                </div>
              ))}
              <div className="flex min-w-0 flex-col gap-1 px-3 py-2 sm:flex-row sm:items-baseline sm:gap-4">
                <span className="op-label w-40 shrink-0">key badges</span>
                <span className="flex min-w-0 flex-wrap items-center gap-2">
                  <Kbd keys={['⌘', 'K']} /><Kbd keys="/" /><Kbd keys={['j', 'k']} /><Kbd keys="d" />
                </span>
              </div>
            </div>
          </div>
        </Demo>
        <Demo label="and where it is not">
          <p className="op-prose min-w-0 text-sm">
            This sentence is Geist. An all-mono paragraph is the fatigue risk the brief calls out; labels and data are not.
          </p>
        </Demo>
      </Block>

      <Block
        id="responsive"
        title="Responsive"
        api={`.op-scroll-x   tab strips, segmented, ranges
.op-cols       grid-template-columns from md
@media (max-width: 767px)
  .op-row { height: auto }`}
        rule={<>
          <p>Verified at 390 and 1440 wide on every v5 screen with a scrollWidth check. The phone is not a narrower desktop: a row that hides its cells has to fold the action it hid into what is left.</p>
          <Rule state="ok">Every desktop-only cell is <code>hidden md:block</code>, and what mattered in it is folded into the first cell as a second line.</Rule>
          <Rule state="error">A row action that only exists in a column the phone does not render.</Rule>
        </>}
      >
        <Demo label="the phone rules">
          <ol className="op-rows min-w-0 border">
            {PHONE_RULES.map((rule, i) => (
              <li key={i} className="flex min-w-0 gap-3 px-3 py-2">
                <span className="w-4 shrink-0 font-mono text-[11px] text-muted-foreground">{i + 1}</span>
                <span className="op-prose min-w-0 text-xs">{rule}</span>
              </li>
            ))}
          </ol>
        </Demo>
        <Demo label="the mini ledger above, folded">
          <div className="max-w-[320px]">
            <MiniLedger density="comfortable" />
          </div>
          <p className="op-prose mt-2 max-w-[320px] text-xs text-muted-foreground">
            Under md the <code className="font-mono">.op-cols</code> grid collapses to <code className="font-mono">1fr auto</code>: the name keeps the
            row, the glyph keeps the verdict, the rest folds away.
          </p>
        </Demo>
      </Block>
    </DocPage>
  )
}
