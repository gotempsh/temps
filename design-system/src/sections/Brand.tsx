// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Fragment } from 'react'
import { Link } from 'react-router'
import { Block, Demo, DocPage, Rule } from '@/components/op-doc'
import { LogoMark, Wordmark } from '@/components/Logo'
import { Kbd, PageTitle, Phrase, Status, StatusLine, TimeChart, type State } from '@/components/op'
import { Button } from '@/components/ui/button'
import { ArrowUp, Pencil, RotateCcw, Terminal, X } from 'lucide-react'

/* ────────────────────────────────────────────────────────────────────────
   /brand — who Temps is, before any token. This page states the decided
   system (v5: paper + ink), not a survey of options. Everything here is
   fixed in docs/brand-guidelines.md and docs/design-system-handoff.md §3–§5;
   a rule changes by editing the doc and this page in the same PR.
   ──────────────────────────────────────────────────────────────────────── */


const TASTE: readonly (readonly [string, string])[] = [
  ['Where does the eye land?', 'Answer first, second, third before shipping. First is the lede, second the primary framed group, third the aside. Two things competing for first means one is the wrong tier.'],
  ['Shapes before type', 'Hierarchy is raised, framed, loose — in that order — and only then size and weight. One raise per page; frames around every group and every piece of content; loose only for prose.'],
  ['Edges align', 'Every block shares the page’s left and right edge with the title row and its actions. Nothing is capped narrower than the page. Too wide to read? A measure inside the frame, never a narrower frame.'],
  ['Proportion carries importance', 'The main column is wide because the thing matters most; the aside is 18rem and 11px because it is reference. Two equal columns say “peers” and are almost always wrong on a record.'],
  ['Icons say what, glyphs say how', 'The kind of an event or thing is an icon (inbox, send, mail-check; terminal, file, agent). Its state is a glyph and a word. A row of green dots is decoration; a row of icons is a story.'],
  ['Say it once', 'A fact lives in the title meta, the lede, or a row — one of them. When it would appear twice, the lower one goes.'],
  ['Actions belong to the title', 'Page actions sit right of the title on the same row. An action floating mid-page belongs to nothing.'],
  ['Show, do not hide', 'Two faithful renderings are a 2-view Segmented in the section action. Reference gets a smaller place, not a closed one: no collapsed sections, no tabs, inside a single record.'],
  ['One ledger per screen', 'A Ledger owns / j k ⏎ and the footer. Two kinds of record are two facets, a tab each — never two tables stacked. A second list that must share the page is a plain framed list of at most five rows linking to its facet.'],
  ['Whitespace between groups, not inside', 'Rows 8px, title-to-body 12px; the air is at the section rule (20px) and between columns (40px).'],
  ['Restraint is the accent', 'Ink on paper, one raise, one glyph colour per state, the hard shadow only, no radius, no gradients. A flat screen needs a better first shape, not more colour.'],
]

const TOC = [
  ['positioning', 'Positioning'],
  ['ai-native', 'AI-native, under policy'],
  ['decision', 'The decision'],
  ['wordmark', 'Wordmark and mark'],
  ['colour', 'Colour role'],
  ['type', 'Type and hierarchy'],
  ['moves', 'Signature moves'],
  ['taste', 'Taste'],
  ['voice', 'Voice and tone'],
  ['dos-donts', 'Do and don’t'],
] as const

const PILLARS = [
  {
    title: 'Operator, not tenant',
    body: 'The reader owns the machine this runs on. Copy talks to someone debugging their own box at a bad hour, with nobody to ask — not to a customer inside somebody else’s SaaS.',
  },
  {
    title: 'One binary, six tools',
    body: 'Deploys, analytics, replay, error tracking, uptime, email. Replacing a stack of subscriptions is the entire pitch, so no surface may look like it belongs to a different product.',
  },
  {
    title: 'Free to self-host',
    body: 'Cloud is the upsell, not the product. Nothing in the console reads as “upgrade to unlock” for capability self-host already has.',
  },
  {
    title: 'Self-driving, under your policy',
    body: 'The roadmap ends in autopilot: sense, understand, decide, act, learn. Agents, skills and MCP servers are how the product works, not a tab. Autonomy is earned per release and bounded per capability; the human is the governor.',
  },
]

const LOOP = [
  ['sense', 'Capture what users, code and infrastructure are doing.'],
  ['understand', 'Turn connected signals into cause, context and memory.'],
  ['decide', 'Choose the next move against a goal, budget and policy.'],
  ['act', 'Ship, fix, roll back, scale or experiment, with permission.'],
  ['learn', 'Measure the outcome and make the next decision better.'],
] as const

const AUTONOMY = [
  ['v0.1', 'stabilise', 'in progress', 'You operate the product. Temps makes the foundation dependable.', 'ok'],
  ['v0.2', 'observe', 'next', 'You decide what to improve. Temps tells you where and why.', 'idle'],
  ['v0.3', 'propose', 'next', 'Temps prepares the change. You review and approve it.', 'idle'],
  ['v1.0', 'autopilot', 'vision', 'You govern goals, policies, budgets and the level of autonomy.', 'idle'],
] as const

const VOCAB = [
  ['agent', 'a bounded run with a goal, a model, a permission mode and a workspace'],
  ['skill', 'a reusable instruction set an agent loads; has a source and a last run'],
  ['tool', 'one typed call: name, argument, state word, output'],
  ['MCP server', 'a source of tools, configured like a git provider'],
  ['proposal', 'a finding turned into a scoped change: evidence, impact, risk, verification'],
  ['approval', 'the human gate, inline, never a modal; once · session · deny'],
  ['run', 'the record of what happened: calls, cost, outcome, undo'],
  ['autonomy level', 'observe · propose · act with approval · autopilot, per capability'],
] as const

const PLANS = [
  ['Self-hosted', '$0', 'unlimited users', 'retention as configured', 'never sampled'],
  ['Starter', '$29', 'no per-seat fee', '30d retention · 10 GB/mo', '7d PITR'],
  ['Team', '$99', 'no per-seat fee', '90d retention · 100 GB/mo', '30d PITR'],
  ['Business', '$299', 'spend cap', '13 months · 1 TB/mo then $0.30/GB', '90d PITR'],
  ['Enterprise', 'contact', 'SSO, SAML, SLA', 'negotiated retention', 'audit + compliance'],
] as const

const DECISIONS = [
  {
    k: 'paper + ink',
    v: 'Warm off-white paper, near-black ink, every border ink.',
    why: 'The console is read at 2am by someone who needs to find the failing row, not admire a surface. Paper and ink give a table its structure for free: the frame is ink, the row dividers are 16% ink, and nothing else needs a colour. Dark mode inverts the same pair rather than introducing a second palette.',
  },
  {
    k: 'Geist + Geist Mono',
    v: 'One sans, one mono, tabular numerals on by default.',
    why: 'Geist has real 700 and 800 weights, which is what the hierarchy is built on, and its mono is legible at 11px in a log gutter. Two faces is the whole type system: language is sans, data is mono, and there is no third face for “personality”.',
  },
  {
    k: 'radius 0.25rem',
    v: 'Frozen in .operator.ink.v5. The live console is 0.5rem today.',
    why: 'At 0.5rem a dense 28px row reads as a stack of pills. At 0.25rem the control still looks intentional but the grid reads as a grid. This is the change that touches every control, so it is frozen here and tested on the settings pages before it lands in the console.',
  },
  {
    k: 'no accent in the console',
    v: 'Primary is ink. There is no accent switcher.',
    why: 'An accent axis is a Cloud feature nobody asked for, and once it ships it is in every screenshot forever. The console gets its emphasis from weight and from the single raised element per screen, so the only colours on a console page are the status glyphs.',
  },
  {
    k: 'signal on one CTA',
    v: 'oklch(0.64 0.21 32) on --primary, landing page only, one per viewport.',
    why: 'Vermilion is the one accent that cannot be mistaken for a status colour or for the focus ring. Cobalt collides with the ring hue, moss reads as “healthy” next to status green, violet reads as AI marketing. It lives on --primary and --primary-foreground only, so it appears on the download button and the closing .op-fill block, never on text, borders, icons or charts.',
  },
  {
    k: 'density comfortable',
    v: 'Two settings, comfortable default, d toggles, choice remembered.',
    why: 'Operators with forty projects want the dense row, but a trial user on day one does not know the row got shorter, only that the page looks cramped. Default to comfortable, show the d badge, and remember what they pick.',
  },
  {
    k: 'motion 100ms',
    v: 'transform, shadow and colour only. No entrance animation.',
    why: 'Motion is feedback that a press registered, not decoration. Anything longer than 100ms is a delay between the operator acting and the console answering, and animating a chart in makes the first frame of data a lie.',
  },
]

const STATUS_STATES: [State, string, string][] = [
  ['ok', 'healthy', 'passing checks, deployed, within threshold'],
  ['warn', 'degraded', 'above threshold, expiring, retrying'],
  ['error', 'failing', 'unreachable, crash-looping, build failed'],
  ['idle', 'not deployed', 'nothing yet, not configured, no traffic'],
  ['sampled', 'sampled', 'head-sampled past the plan allowance — never silently dropped'],
]

const HIERARCHY = [
  ['op-display', '800', 'Landing hero. One per page. Never in the console.', 'Stop paying for 7 SaaS tools.'],
  ['op-h1', '700', 'Major landing section title.', 'Temps imports your existing setup.'],
  ['op-title', '700', 'Console page title. The one 700 line on a console screen.', 'api-gateway'],
  ['op-h2', '600', 'Minor section or panel title. The console’s largest tier.', 'Where it runs'],
  ['op-h3', '600', 'Item title inside a grid or row.', 'Session replay'],
  ['op-lead', '400', 'The sentence under a title. Muted, one bold phrase at most.', 'One self-hosted Rust binary replaces your deployment platform and six other tools.'],
  ['op-label', '500', 'Eyebrow, column header, key badge. Uppercase, tracked.', 'self-hosted deploy tools stop at deploy'],
] as const

const CHART = Array.from({ length: 24 }, (_, i) => ({
  t: `${String(i).padStart(2, '0')}:00`,
  req: Math.round(420 + 520 * Math.max(0, Math.sin(((i - 6) / 24) * Math.PI * 2)) + (i > 15 ? 210 : 0)),
}))

const VOICE = [
  {
    bad: '“Something went wrong. Please try again.”',
    good: '“Deploy dep_91a failed: the build image is 4.2 GB, over the 4 GB limit. Reduce the image or raise the limit in Project settings → Build.”',
    note: 'Verdict first, then the resource, then the fix. An operator alone at 2am cannot act on “something”.',
  },
  {
    bad: '◐ 6 projects · × billing-worker failing · 4 deploys today · cert 6d',
    good: '× billing-worker is failing health checks.  +1 warning',
    note: 'The status line is a verdict, not a summary. Counts and facts belong in the page below; further problems collapse into “+N more” on the right.',
  },
  {
    bad: '“Redeploy the service to apply your changes.”',
    good: '“Redeploy to apply — the console runs temps service deploy api-gateway --env production.”',
    note: 'CLI verbs are echoed as temps <noun> <verb>, so the console teaches the command it is about to run and the operator can repeat it in a script.',
  },
]

const DOS = [
  'Give every landing page one 800 headline and every console screen one 700 title — nothing else at that size.',
  'Keep paper and ink for 95% of every surface; hierarchy comes from weight, spacing and ink rules.',
  'Let colour mean status, always through <Status>, always next to a glyph and a word.',
  'Style links as ink with a soft underline — the blue --ring is for focus, and only on focus.',
  'Let density read as competence: whitespace between sections, not inside tables.',
  'Write errors that name the resource and the fix, and say when telemetry is sampled instead of dropping it quietly.',
]

const DONTS = [
  'Add a second hue “for interest”. Colour is status, or the one landing accent on --primary.',
  'Put the accent on anything without a click target — text, icons, borders, charts, section backgrounds.',
  'Use a title at weight 500. Titles are 600–800, body 400, labels 500.',
  'Ship the stock shadcn card look, or use cards as layout. Grids with ink borders; one .op-raise per screen.',
  'Hide a feature because it is unconfigured or off-plan. Show it, say what is missing, link to the fix.',
  'Use a spinner as a page state, or leave an empty chart blank without saying which of the four reasons applies.',
]

export function BrandPage() {
  return (
    <DocPage
      eyebrow="brand · who temps is, before any token"
      intro={
        <>
          The decided system, not a survey of options: paper and ink, Geist and Geist Mono,
          radius 0.25rem, one accent on one button on the landing page, and nothing else.
          Foundations is the spec sheet; this page is why the spec sheet says what it says.
          Source of truth: <code className="font-mono">docs/brand-guidelines.md</code> and{' '}
          <code className="font-mono">docs/design-system-handoff.md</code> §3–§5.
        </>
      }
      toc={TOC}
    >
      <Block
        id="positioning"
        title="Positioning"
        rule={
          <>
            <p>
              A self-hosted platform that replaces six-plus paid SaaS tools with a single Rust
              binary (deploys, analytics, replay, error tracking, uptime, databases, email), and
              then runs an improvement loop over them. The roadmap calls it the road to
              self-driving products: stabilise, observe, propose, improve. Agents, skills, MCP
              servers and scheduled automation are the product, not a feature tab.
            </p>
            <p>
              The primary reader is an operator debugging their own box, alone. The secondary
              reader is that same operator justifying Temps to a team. The landing page talks
              to the second one, the console to the first; when they conflict, the console
              wins. Nothing in the console exists to impress.
            </p>
          </>
        }
      >
        <Demo label="the four pillars">
          <div className="grid gap-px border sm:grid-cols-2 lg:grid-cols-4">
            {PILLARS.map((p) => (
              <div key={p.title} className="min-w-0 p-3">
                <p className="op-h3">{p.title}</p>
                <p className="op-prose mt-1 text-xs text-muted-foreground">{p.body}</p>
              </div>
            ))}
          </div>
        </Demo>

        <Demo label="the plan ladder, as design input">
          <div className="op-rows border text-sm">
            {PLANS.map(([name, price, seats, retention, extra]) => (
              <div key={name} className="grid min-w-0 gap-x-3 gap-y-0.5 px-3 py-2 sm:grid-cols-[9rem_5rem_minmax(0,1fr)]">
                <span className="min-w-0 font-medium">{name}</span>
                <span className="min-w-0 font-mono text-xs tabular-nums text-muted-foreground sm:text-sm">{price}</span>
                <span className="min-w-0 text-xs text-muted-foreground">
                  {seats} · {retention} · {extra}
                </span>
              </div>
            ))}
          </div>
          <p className="op-prose mt-3 text-xs text-muted-foreground">
            Pricing is a design constraint, not a marketing page: because retention and ingest
            differ per plan, every time axis states its retention horizon, ranges past it are
            struck through rather than hidden, and telemetry past the allowance is shown as{' '}
            <Status state="sampled" label="sampled" className="text-xs" /> on the chart, in the
            chart footer and in the status line — visible, never silently dropped.
          </p>
        </Demo>
      </Block>

      <Block
        id="ai-native"
        title="AI-native, under policy"
        rule={
          <>
            <p>
              AI is an operator, so it is held to operator rules. An agent's work is a ledger
              of typed tool calls with state words, not a chat bubble. A finding carries
              evidence, confidence, impact and a verification plan or it is not shown. A
              write is proposed, previewed with redacted parameters, and approved inline.
            </p>
            <p>
              Autonomy is a control, not a mood: set per capability, in words, with a budget,
              an override and a kill switch. Evidence before adjectives: no sparkles, no
              gradients, no wand. The reference surface is{' '}
              <Link to="/agent" className="underline underline-offset-4">/agent</Link>.
            </p>
          </>
        }
        api={`<Tool name="run_command" arg="bun test" state="approval-requested" />
<Picker value="propose" options={AUTONOMY} />   observe · propose · act with approval · autopilot
<Proposal what evidence confidence impact risk verify />`}
      >
        <Demo label="the loop the product runs · from the roadmap">
          <div className="grid gap-px border sm:grid-cols-5">
            {LOOP.map(([k, v], i) => (
              <div key={k} className="min-w-0 p-3">
                <p className="font-mono text-[10px] text-muted-foreground">0{i + 1}</p>
                <p className="op-h3">{k}</p>
                <p className="op-prose mt-1 text-xs text-muted-foreground">{v}</p>
              </div>
            ))}
          </div>
        </Demo>

        <Demo label="autonomy is earned per release, and always says who does what">
          <div className="op-rows border text-sm">
            {AUTONOMY.map(([v, level, status, who, state]) => (
              <div key={v} className="grid min-w-0 grid-cols-[3rem_6rem_minmax(0,1fr)] items-baseline gap-x-3 px-3 py-2 sm:grid-cols-[3rem_6rem_6.5rem_minmax(0,1fr)]">
                <span className="font-mono text-[11px] text-muted-foreground">{v}</span>
                <span className="font-medium">{level}</span>
                <span className="hidden sm:block"><Status state={state} label={status} /></span>
                <span className="col-span-3 text-xs text-muted-foreground sm:col-span-1">{who}</span>
              </div>
            ))}
          </div>
        </Demo>

        <Demo label="the proposal block · same shape every time, or it is only a finding">
          <div className="op-raise border p-3 text-sm">
            <p className="flex items-baseline gap-2"><span className="op-label">proposal</span><span className="font-medium">Guard normalize() for pickup orders</span><span className="ml-auto font-mono text-[11px] text-muted-foreground">propose · needs approval</span></p>
            <dl className="mt-2 grid gap-y-1 text-xs sm:grid-cols-[8rem_minmax(0,1fr)]">
              <dt className="op-label">evidence</dt><dd><a href="#" onClick={(e) => e.preventDefault()}>err_4f21</a> · 31 events since <a href="#" onClick={(e) => e.preventDefault()}>dep_91a</a> · stack at address.ts:14</dd>
              <dt className="op-label">confidence</dt><dd>high · one cause, one caller</dd>
              <dt className="op-label">impact</dt><dd className="font-mono">−31 errors/day · baseline 7d</dd>
              <dt className="op-label">risk · blast radius</dt><dd>low · api-gateway production · pickup orders only</dd>
              <dt className="op-label">verification</dt><dd>checkout suite green · error group closes within 24h or auto-rollback</dd>
            </dl>
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <Button size="sm" className="op-primary h-7 text-xs">approve <Kbd keys="Y" className="ml-1 opacity-70" /></Button>
              <Button size="sm" variant="outline" className="h-7 text-xs">edit</Button>
              <Button size="sm" variant="outline" className="h-7 text-xs">deny <Kbd keys="N" className="ml-1 opacity-70" /></Button>
              <span className="ml-auto text-[11px] text-muted-foreground">prepared in a sandbox; production untouched until you say so</span>
            </div>
          </div>
        </Demo>

        <Demo label="vocabulary · these words, no synonyms">
          <dl className="grid gap-x-4 gap-y-1 border p-3 text-xs sm:grid-cols-[9rem_minmax(0,1fr)]">
            {VOCAB.map(([k, v]) => <Fragment key={k}><dt className="font-mono font-medium">{k}</dt><dd className="text-muted-foreground">{v}</dd></Fragment>)}
          </dl>
        </Demo>

        <Demo label="before / after · AI copy">
          <div className="border p-3 text-sm">
            <p className="flex items-baseline gap-2"><span className="text-destructive">×</span><span>“✨ AI-powered insights find and fix issues automatically.”</span></p>
            <p className="mt-1 flex items-baseline gap-2"><span className="text-success">●</span><span>“Found in 31 events since dep_91a. Proposed a guard in address.ts with a test; the checkout suite is green. Approve to open the PR.”</span></p>
            <p className="mt-2 text-xs text-muted-foreground">An operator's log, not a promise. What ran, what it found, what it wants to do, what proves it worked.</p>
          </div>
        </Demo>
      </Block>

      <Block
        id="decision"
        title="The decision"
        rule={
          <>
            <p>
              These are frozen. They do not get reopened without a written reason, and a change
              means editing <code className="font-mono">brand-guidelines.md</code> and this page
              in the same pull request.
            </p>
            <p>
              Honest caveat: this is the design system, not the shipped console. The live
              console (<code className="font-mono">temps/web</code>) still ships the old
              Vercel-derived palette, 0.5rem radius, 2,265 Tailwind palette literals and three
              empty-state implementations. The ink tokens land behind{' '}
              <code className="font-mono">.operator.ink</code> on the shell and nothing changes
              until that class is set (handoff §11).
            </p>
          </>
        }
        api={`root  class="operator ink v4 v5"        console: no accent
root  class="operator ink v4 v5"        landing: data-accent="signal"
      data-density="dense"              optional, remembered
--radius            0.25rem
--primary           ink · landing signal oklch(0.64 0.21 32)
transition-duration 100ms`}
      >
        <Demo label="what was decided, and why">
          <div className="op-rows border">
            {DECISIONS.map((d) => (
              <div key={d.k} className="grid min-w-0 gap-x-4 gap-y-1 p-3 sm:grid-cols-[10rem_minmax(0,1fr)]">
                <div className="min-w-0">
                  <p className="op-label">{d.k}</p>
                  <p className="mt-1 text-xs text-muted-foreground">{d.v}</p>
                </div>
                <p className="op-prose min-w-0 text-sm">{d.why}</p>
              </div>
            ))}
          </div>
        </Demo>

        <Demo label="why not the Vercel blue we started from">
          <div className="space-y-2">
            <Rule state="error">
              The console’s palette was labelled “Vercel-inspired” in its own comments and used
              Vercel’s blue and red verbatim. A cropped screenshot could have been anybody’s
              dashboard, which is the opposite of a brand.
            </Rule>
            <Rule state="ok">
              Blue survives in exactly one place: <code className="font-mono">--ring</code>,{' '}
              <code className="font-mono">oklch(0.59 0.2032 256.82)</code>, drawn on focus only.
              It is never a fill, a link colour or a border.
            </Rule>
            <Rule state="ok">
              Emphasis is weight, not colour: an 800 headline and an ink fill outrank anything a
              hue could do, and they still work in a screenshot, in print and for a reader who
              cannot tell red from green.
            </Rule>
            <Rule state="ok">
              Borders are ink because a 1px ink frame separates a table from the page without
              spending a grey. Greys are for secondary text; structure is ink.
            </Rule>
          </div>
        </Demo>
      </Block>

      <Block
        id="wordmark"
        title="Wordmark and mark"
        rule={
          <>
            <p>
              The real mark, copied from <code className="font-mono">temps-landing/public/logo/</code>,
              not invented here. Two fixed-colour SVGs: a dark badge with a light glyph for paper,
              and the inverse for ink. The badge never tracks CSS custom properties — choosing the
              variant is the whole of the theming.
            </p>
            <p>
              <code className="font-mono">Logo.tsx</code> documents no clear-space or minimum-size
              constants, so the practical rule holds: the mark is never smaller than 20px, never
              recoloured, never stretched, and always has at least its own badge height of clear
              space on every side. In the nav it sits at 20px, in a wordmark at 28px.
            </p>
          </>
        }
        api={`<LogoMark size={32} />                 follows the app theme
<LogoMark size={20} variant="dark" />  on an ink surface
<Wordmark markSize={28} />             mark + "temps", weight 700`}
      >
        <div className="grid gap-4 sm:grid-cols-2">
          <Demo label="on paper">
            <div className="flex flex-col items-start gap-4 border p-4">
              <Wordmark markSize={32} textClassName="text-2xl" variant="light" />
              <div className="flex items-end gap-3">
                <LogoMark size={32} variant="light" />
                <LogoMark size={24} variant="light" />
                <LogoMark size={20} variant="light" />
              </div>
              <p className="font-mono text-[11px] text-muted-foreground">32 · 24 · 20 (floor)</p>
            </div>
          </Demo>
          <Demo label="on ink">
            <div className="bg-foreground text-background flex flex-col items-start gap-4 border p-4">
              <Wordmark markSize={32} textClassName="text-2xl" variant="dark" />
              <div className="flex items-end gap-3">
                <LogoMark size={32} variant="dark" />
                <LogoMark size={24} variant="dark" />
                <LogoMark size={20} variant="dark" />
              </div>
              <p className="font-mono text-[11px] opacity-70">32 · 24 · 20 (floor)</p>
            </div>
          </Demo>
        </div>
        <div className="space-y-2">
          <Rule state="error">
            Rendering the light-surface icon on a dark nav. The live landing nav does exactly
            this and the mark loses contrast; the sidebar and hero already swap correctly.
          </Rule>
          <Rule state="error">
            Recolouring the badge to the accent, putting the mark inside a coloured circle, or
            shipping it under 20px where the glyph closes up.
          </Rule>
        </div>
      </Block>

      <Block
        id="colour"
        title="Colour role"
        rule={
          <>
            <p>
              Four surface tokens carry the whole console: paper{' '}
              <code className="font-mono">--background</code>, ink{' '}
              <code className="font-mono">--foreground</code> (which is also every border),{' '}
              <code className="font-mono">--muted</code> for section tone and hover, and{' '}
              <code className="font-mono">--muted-foreground</code> for secondary text.
            </p>
            <p>
              Beyond those, colour means status and nothing else. Green, amber and red only ever
              arrive through <code className="font-mono">&lt;Status&gt;</code>, which pairs them
              with a glyph and a word, so the state survives a greyscale screenshot and a reader
              who cannot distinguish the hues.
            </p>
          </>
        }
        api={`--background        paper
--foreground        ink, and every border
--muted             section tone, hover, sampled band
--op-rule-soft      16% ink, ledger row dividers only
--ring              oklch(0.59 0.2032 256.82) — focus only
--primary           ink; landing accent oklch(0.64 0.21 32)`}
      >
        <Demo label="paper, ink, muted">
          <div className="grid gap-px border sm:grid-cols-3">
            <div className="min-w-0 p-3">
              <div className="h-12 border bg-background" />
              <p className="op-label mt-2">paper</p>
              <p className="mt-1 text-xs text-muted-foreground">oklch(0.975 0.004 95)</p>
            </div>
            <div className="min-w-0 p-3">
              <div className="bg-foreground h-12 border" />
              <p className="op-label mt-2">ink</p>
              <p className="mt-1 text-xs text-muted-foreground">oklch(0.13 0 0) · also every border</p>
            </div>
            <div className="min-w-0 p-3">
              <div className="h-12 border bg-muted" />
              <p className="op-label mt-2">muted</p>
              <p className="mt-1 text-xs text-muted-foreground">oklch(0.94 0.005 95) · tone, hover, sampled band</p>
            </div>
          </div>
        </Demo>

        <Demo label="the five status glyphs">
          <div className="op-rows border text-sm">
            {STATUS_STATES.map(([state, label, meaning]) => (
              <div key={state} className="grid min-w-0 gap-x-3 px-3 py-2 sm:grid-cols-[10rem_minmax(0,1fr)]">
                <Status state={state} label={label} />
                <span className="min-w-0 text-xs text-muted-foreground">{meaning}</span>
              </div>
            ))}
          </div>
        </Demo>

        <Demo label="the landing accent — once, on the primary CTA">
          <div className="operator ink v4 v5 flex flex-wrap items-center gap-3 border p-4" data-accent="signal">
            <Button className="op-primary h-10 text-sm">Download for macOS</Button>
            <span className="text-xs text-muted-foreground">
              signal · oklch(0.64 0.21 32) on <code className="font-mono">--primary</code>, landing only
            </span>
          </div>
        </Demo>

        <div className="space-y-2">
          <Rule state="error">Blue fills — a blue button, badge or banner. The ring hue has area only as a 2px outline on focus.</Rule>
          <Rule state="error">A second hue anywhere. One accent, on <code className="font-mono">--primary</code>, at most one filled element per viewport.</Rule>
          <Rule state="error">Gradients, glows and tinted section backgrounds. The only permitted background change between sections is <code className="font-mono">data-tone="muted"</code>.</Rule>
          <Rule state="error">Status by colour alone — a bare red dot, a green row tint, a coloured badge with no word. Glyph and word, always.</Rule>
          <Rule state="error">Charts in colour. Lines are ink on paper; the deploy marker is a dotted ink line and the sampled window is a muted band.</Rule>
        </div>
      </Block>

      <Block
        id="type"
        title="Type and hierarchy"
        rule={
          <>
            <p>
              Weight is the signal. Seven tiers, each with a fixed weight, so a reader knows
              which level they are on before reading a word. The failure this fixes: everything
              at weight 500, so every line competed and nothing led.
            </p>
            <p>
              One 800 line per landing page, one 700 line per console screen. The console never
              uses <code className="font-mono">.op-display</code> or{' '}
              <code className="font-mono">.op-h1</code>; its title is{' '}
              <code className="font-mono">.op-title</code> and its largest section tier is{' '}
              <code className="font-mono">.op-h2</code>.
            </p>
            <p>
              Language is sans; data is mono, tabular, and one tier larger than its label.
              IDs, hashes, branch names, durations and percentages are mono wherever they appear.
            </p>
          </>
        }
        api={`.op-display  800  landing hero, one per page
.op-h1       700  landing major section
.op-title    700  console page title, one per screen
.op-h2       600  panel title — console maximum
.op-h3       600  item title in a grid
.op-lead     400  the sentence under a title, muted
.op-label    500  eyebrow, column header, key badge`}
      >
        <Demo label="the scale, rendered">
          <div className="op-rows border">
            {HIERARCHY.map(([cls, weight, role, sample]) => (
              <div key={cls} className="grid min-w-0 items-baseline gap-x-4 gap-y-1 px-3 py-3 sm:grid-cols-[7rem_3rem_minmax(0,1fr)]">
                <code className="min-w-0 font-mono text-[11px] text-muted-foreground">.{cls}</code>
                <span className="font-mono text-[11px] tabular-nums text-muted-foreground">{weight}</span>
                <div className="min-w-0">
                  <p className={cls} style={cls === 'op-display' ? { fontSize: '3.25rem' } : undefined}>{sample}</p>
                  <p className="mt-1 text-xs text-muted-foreground">{role}</p>
                </div>
              </div>
            ))}
          </div>
        </Demo>

        <div className="space-y-2">
          <Rule state="ok">Titles are 600–800, body is 400, labels are 500. There is no 500 title.</Rule>
          <Rule state="ok">A title’s lead is muted, and bold inside a lead is reserved for the one phrase that changes the reader’s mind.</Rule>
          <Rule state="error">Two things at the same largest size. If two things are the biggest, neither leads.</Rule>
        </div>
      </Block>

      <Block
        id="moves"
        title="Signature moves"
        rule={
          <>
            <p>
              These are what make a Temps screen recognisable at a glance, and every product
              surface uses at least one. The first two are data patterns rather than styling,
              deliberately: a palette can be copied in an afternoon, “every chart knows when you
              deployed” cannot.
            </p>
          </>
        }
        api={`<StatusLine state more={{ label: '+1 warning' }} />
<TimeChart markers={[{ id: 'dep_91a', x: '16:00' }]} />
<Kbd keys={['⌘', '⏎']} />       ⌘ becomes Ctrl off macOS
<PageTitle title meta />`}
      >
        <Demo label="status line — one glyph, one sentence, at most one link">
          <div className="border px-4 sm:px-6">
            <StatusLine state="error" sticky={false} more={{ label: '+1 warning' }}>
              <Phrase>billing-worker</Phrase> is failing health checks.
            </StatusLine>
          </div>
          <p className="op-prose mt-2 text-xs text-muted-foreground">
            The glyph is the worst state on the page, the sentence is under 60 characters, and
            everything else collapses into the muted “+1 warning” on the right, which opens the
            list sorted by attention. No counts, no facts, nothing that is fine.
          </p>
        </Demo>

        <Demo label="deploy markers — on every time axis">
          <div className="border p-3">
            <TimeChart
              data={CHART}
              series={[{ key: 'req', name: 'requests' }]}
              markers={[{ id: 'dep_91a', x: '09:00' }, { id: 'dep_92c', x: '16:00' }]}
              unit="req/min"
              height={140}
            />
            <p className="mt-2 font-mono text-[11px] text-muted-foreground">30d retention · Starter</p>
          </div>
        </Demo>

        <Demo label="typed confirmation — the name you are about to destroy, copyable, right before the input">
          <div className="border p-3 text-sm">
            <p>Delete <span className="font-mono">api-gateway</span> and its 3 environments.</p>
            <div className="mt-2 flex items-center gap-2">
              <span className="inline-flex h-8 items-center gap-1 border bg-muted pl-2 pr-1 font-mono text-xs">api-gateway <span className="inline-flex h-6 w-6 items-center justify-center text-muted-foreground">⧉</span></span>
              <span className="flex h-8 flex-1 items-center border px-2 font-mono text-xs text-muted-foreground">api-gateway</span>
            </div>
          </div>
        </Demo>

        <div className="grid gap-4 sm:grid-cols-2">
          <Demo label="key badges — accelerators, never the only door">
            <div className="flex flex-wrap items-center gap-4 border p-3 text-sm">
              <span className="inline-flex items-center gap-2">Deploy <Kbd keys={['⌘', '⏎']} /></span>
              <span className="inline-flex items-center gap-2">Filter <Kbd keys="/" /></span>
              <span className="inline-flex items-center gap-2">Density <Kbd keys="d" /></span>
            </div>
          </Demo>
          <Demo label="page title + meta">
            <div className="border p-3">
              <PageTitle title="api-gateway" meta="production · dep_91a · 41m ago" crumbs={[{ label: 'platform' }, { label: 'Projects' }]} className="pt-0" />
            </div>
          </Demo>
        </div>

        <Demo label="icons describe the action, words carry the state">
          <div className="space-y-2 border p-3 text-sm">
            <div className="op-inset flex items-center gap-2 py-1.5 pl-3 pr-1.5">
              <span className="min-w-0 flex-1 truncate">Also update the docs page for the address form</span>
              <span className="flex shrink-0 items-center gap-0.5">
                <span title="Edit" className="inline-flex h-7 w-7 items-center justify-center text-muted-foreground"><Pencil className="h-3.5 w-3.5" /></span>
                <span title="Send now" className="inline-flex h-7 w-7 items-center justify-center bg-foreground text-background"><ArrowUp className="h-3.5 w-3.5" /></span>
                <span title="Remove" className="inline-flex h-7 w-7 items-center justify-center text-muted-foreground"><X className="h-3.5 w-3.5" /></span>
              </span>
            </div>
            <div className="flex items-center gap-2 text-xs">
              <Terminal className="h-3.5 w-3.5 text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate font-mono"><span className="font-medium">$</span> bun test src/checkout</span>
              <span className="shrink-0 font-mono text-[11px] text-muted-foreground">done · 2.1s</span>
            </div>
            <div className="flex items-center gap-1 text-[11px] text-muted-foreground">
              <span className="inline-flex h-6 items-center gap-1 px-1.5"><RotateCcw className="h-3 w-3" /> retry</span>
              <span className="inline-flex h-6 items-center gap-1 px-1.5 text-foreground">✓ copied</span>
            </div>
          </div>
          <p className="op-prose mt-2 text-xs text-muted-foreground">
            A control shows what it does (pencil, arrow, ×); a row shows what it is (terminal,
            file, agent). State is a glyph and a word, never an icon. Action icons are 14px,
            muted until hover, titled, at the end of the row they act on. No heading over a row
            whose position already explains it.
          </p>
        </Demo>

        <Demo label="five states, including sampled">
          <div className="flex flex-wrap gap-x-6 gap-y-2 border p-3 text-sm">
            {STATUS_STATES.map(([state, label]) => (
              <Status key={state} state={state} label={label} />
            ))}
          </div>
          <p className="op-prose mt-2 text-xs text-muted-foreground">
            <code className="font-mono">sampled</code> exists because the pricing page promises
            that past the allowance telemetry is head-sampled and the console says so. That
            promise is a UI contract, not a footnote.
          </p>
        </Demo>
      </Block>

      <Block
        id="taste"
        title="Taste"
        rule={
          <>
            <p>
              The other sections say what a page is made of. Taste is the judgement calls that
              decide whether the result reads as one considered application or a pile of correct
              parts. They are written down so every page, by every hand, makes them the same way.
            </p>
          </>
        }
        api={`before shipping a page, answer:
  1st  the lede (one raise)
  2nd  the primary framed group
  3rd  the aside (18rem, 11px)

edges align · say it once · icons say what, glyphs say how`}
      >
        <Demo label="the calls">
          <ol className="op-rows border text-sm">
            {TASTE.map(([t, d], i) => (
              <li key={t} className="grid gap-x-4 gap-y-1 px-3 py-2.5 sm:grid-cols-[2rem_15rem_minmax(0,1fr)]">
                <span className="font-mono text-[11px] text-muted-foreground">{String(i + 1).padStart(2, '0')}</span>
                <span className="font-medium">{t}</span>
                <span className="op-prose text-xs text-muted-foreground">{d}</span>
              </li>
            ))}
          </ol>
        </Demo>
      </Block>

      <Block
        id="voice"
        title="Voice and tone"
        rule={
          <>
            <p>
              Direct, technical, specific. No exclamation points, no “Oops!”, no marketing
              adjectives on functional UI. The reader has no support channel, so every message
              is the support channel.
            </p>
            <p>
              The rules: verdict first; name the resource and the fix; never “something went
              wrong”; no counts in the status line; CLI verbs echoed as{' '}
              <code className="font-mono">temps &lt;noun&gt; &lt;verb&gt;</code>; numbers in
              mono, with the unit after the value and an en dash for nothing.
            </p>
          </>
        }
      >
        {VOICE.map((v) => (
          <Demo key={v.bad} label="before / after">
            <div className="space-y-2 border p-3">
              <Rule state="error">{v.bad}</Rule>
              <Rule state="ok">{v.good}</Rule>
              <p className="op-prose text-xs text-muted-foreground">{v.note}</p>
            </div>
          </Demo>
        ))}
      </Block>

      <Block
        id="dos-donts"
        title="Do and don’t"
        rule={
          <p>
            The short form. Everything here follows from the five rules in handoff §3; the
            banned list in §13 is the enforceable version of the right-hand column.
          </p>
        }
      >
        <div className="grid gap-6 sm:grid-cols-2">
          <div className="min-w-0 space-y-2">
            <p className="op-label">do</p>
            {DOS.map((d) => <Rule key={d} state="ok">{d}</Rule>)}
          </div>
          <div className="min-w-0 space-y-2">
            <p className="op-label">don’t</p>
            {DONTS.map((d) => <Rule key={d} state="error">{d}</Rule>)}
          </div>
        </div>
      </Block>
    </DocPage>
  )
}
