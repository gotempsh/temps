// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import {
  ArrowRight,
  Bot,
  Check,
  ChevronDown,
  Database,
  Download,
  Menu,
  Star,
  Terminal,
  Video,
  X, Maximize2, Minimize2 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { LogoMark } from '@/components/Logo'
import { ConsoleV1 } from '@/sections/ConsoleV1'
import { PlatformLogo } from '@/components/platform-logos'
import { SystemMapSection } from '@/components/system-map-section'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   /v1-landing — temps.sh at its current depth (15 sections), drawn in the
   "paper and ink" direction of the v1 skin. Copy and section order follow
   the live page. Product screens use the same primitives the console does,
   and the Dashboard tab embeds the real v1 console shell.
   ──────────────────────────────────────────────────────────────────────── */

const NAV = ['Docs', 'Blog', 'Roadmap', 'Pricing', 'Managed', 'Enterprise', 'Security', 'Contact']

// Hero strip: the real marks of what gets replaced, same as the live hero.
const TOOLS = [
  ['Deploy', 'Vercel'], ['Errors', 'Sentry'], ['Analytics', 'PostHog'], ['Uptime', 'Pingdom'], ['Email', 'Resend'], ['Sandbox', 'Docker'], ['Traces', 'OpenTelemetry'],
] as const

const TOUR = ['Dashboard', 'AI Chat', 'AI Agent Sandbox', 'AI Gateway', 'Analytics', 'Error tracking', 'Tracing', 'Uptime'] as const
type Tour = (typeof TOUR)[number]


const DIFFERENT = [
  ['Everyone sees everything in one place.', 'Deployments, error tracking, analytics, and monitoring all in one dashboard your whole team can access. No more "did you check Sentry?" or "who has the Datadog login?"'],
  ['Deploy from any branch, any framework, any developer.', 'Git push from any team member triggers a build. Preview URLs for every pull request. Auto-detects your stack — no DevOps knowledge required.'],
  ['Know about errors before your users report them.', 'Full error tracking with stack traces and source maps. Sentry-compatible — existing integrations keep working. Instant alerts to Slack or Discord.'],
  ['See how real users move through your product.', 'Built-in analytics and session replay. No third-party scripts, no cookie compliance headaches, no data leaving your servers.'],
  ['Databases your team can provision in minutes.', 'PostgreSQL, Redis, MySQL, MongoDB — all managed through the same dashboard. No separate AWS RDS setup, no egress fees, no DBAs required.'],
  ['One bill that doesn’t grow with the team.', 'Add a developer, ship a feature, scale traffic — your Temps bill stays exactly where it was. No per-seat charges, no event-based pricing, no surprise overages.'],
]

const COUNT_SELF = ['A deploy tool', 'A self-hosted error tracker (Sentry, GlitchTip)', 'An OpenTelemetry backend (Grafana, SigNoz)', 'An uptime monitor (Uptime Kuma, Gatus)', 'An analytics stack (Plausible, PostHog)', 'A session replay tool (OpenReplay)', 'A database on the side, and its backups']

const GENUINE = [
  ['Errors, before your users report them', 'Stack trace, release, the session that hit it, the deploy that introduced it. One page.'],
  ['Where requests actually go slow', 'Traces with deploy markers on the axis. "Since this deploy" is the default comparison.'],
  ['Handing an error to an agent that can fix it', 'The agent sees the stack, the logs and the diff. It proposes; you confirm.'],
  ['Reusable debugging skills for the agent', 'Teach it once how your stack fails. It remembers across projects.'],
  ['Managed databases', 'Provision, back up, restore to a point in time, from the same console.'],
  ["Where your users' behavioral data lives", 'On your box. Analytics and replay never leave the server you own.'],
  ['What happens when the team grows', 'Nothing. Same binary, same bill.'],
]

// Same 12 as the live migrate-from strip (divides evenly at every breakpoint).
const MIGRATE = ['Coolify', 'Dokploy', 'CapRover', 'Portainer', 'Kamal', 'Kubernetes', 'Docker', 'Vercel', 'Netlify', 'Railway', 'Render', 'Fly.io']

const INFRA = ['Docker', 'PostgreSQL', 'Redis', 'MongoDB', 'MariaDB', 'OpenTelemetry', 'Scaleway', 'Node.js', 'Python', 'Go', 'PHP', 'Ruby']

const PRICING = [
  { name: 'Self-hosted', price: '$0', per: ' forever', hot: false, cta: 'Install', rows: ['MIT / Apache 2.0', 'Unlimited projects, deploys, users', 'Every feature. Nothing gated', 'Your server · a $5–10 VPS is enough', 'Backups to storage you control'] },
  { name: 'Starter', price: '$29', per: '/mo', hot: false, cta: 'Start trial', rows: ['10 GB telemetry / mo · fixed', '30-day retention', '50 GB backup storage', 'Nightly backups · 7-day PITR', '250 AI credits'] },
  { name: 'Team', price: '$99', per: '/mo', hot: true, cta: 'Start trial', rows: ['100 GB telemetry / mo · fixed', '90-day retention', '250 GB backup storage', 'Continuous WAL · 30-day PITR', '1,000 AI credits'] },
  { name: 'Business', price: '$299', per: '/mo', hot: false, cta: 'Start trial', rows: ['1 TB telemetry / mo, then $0.30/GB under a hard cap', '13-month retention', '1 TB backup storage', 'Continuous WAL · 90-day PITR', '5,000 AI credits'] },
  { name: 'Enterprise', price: 'Custom', per: '', hot: false, cta: 'Contact', rows: ['Everything in Business', 'SSO & SAML', 'Dedicated support engineer', 'Custom SLA & compliance', 'Negotiated retention and region'] },
]

const LIMITS = [
  ['Telemetry past the monthly allowance', 'status line: telemetry sampled 1 in 4 since 14:00 · 10 GB allowance reached · chart marks the sampled window'],
  ['Backup storage past the included amount', 'backups keep running · settings shows 62 GB of 50 GB included · nothing is charged today'],
  ['AI credits used up', 'chat and agents show 0 of 250 credits · what a run would have cost · resets on the 1st'],
  ['Range beyond your retention', 'the 90d button stays visible and says: beyond 30d retention on Starter · Team keeps 90d'],
]

const FAQ = [
  ['What is the cheapest way to host Next.js in 2026?', 'A single VPS running Temps. One binary handles builds, routing, TLS, analytics and error tracking, so the only bill is the server.'],
  ['What is the best self-hosted Vercel alternative in 2026?', 'One that does not stop at deploy. Vercel-grade git-push workflow, plus the observability you would otherwise buy from three more vendors.'],
  ['How do I deploy Next.js without Vercel?', 'Connect the repository, pick a branch, push. Temps detects the framework, builds a container and routes a domain to it with TLS.'],
  ['What is a free self-hosted Sentry alternative?', 'Temps error tracking is Sentry-compatible: point the existing SDK at a Temps DSN and keep your integrations.'],
  ['Is Temps open source?', 'Yes. Apache-2.0 / MIT dual-licensed. Free to self-host forever. Temps Cloud is the managed option.'],
  ['Does Temps support Docker and all programming languages?', 'Anything that builds into a container. Presets for the common frameworks, a Dockerfile for everything else.'],
]

// ── Small building blocks ──────────────────────────────────────────────

function Section({ eyebrow, title, lead, children, className, tier = 'major', tone }: { eyebrow: string; title: string; lead?: string; children: ReactNode; className?: string; tier?: 'major' | 'minor'; tone?: 'muted' }) {
  return (
    <section data-tier={tier} data-tone={tone} className={cn('op-section border-t px-4 sm:px-8', className)}>
      <div className="mx-auto max-w-6xl">
        <p className="op-label">{eyebrow}</p>
        {tier === 'major' ? (
          <h2 className="op-h1 mt-3 max-w-3xl">{title}</h2>
        ) : (
          <h2 className="op-h2 mt-2 max-w-2xl">{title}</h2>
        )}
        {lead && <p className={cn('op-lead mt-4', tier === 'major' ? 'max-w-2xl' : 'max-w-xl text-base')}>{lead}</p>}
        <div className={tier === 'major' ? 'mt-10' : 'mt-6'}>{children}</div>
      </div>
    </section>
  )
}

function Glyph({ state, label }: { state: 'ok' | 'warn' | 'error' | 'idle'; label: string }) {
  const g = { ok: '●', warn: '◐', error: '×', idle: '○' }[state]
  const c = { ok: 'text-success', warn: 'text-warning', error: 'text-destructive', idle: 'text-muted-foreground' }[state]
  return <span className="inline-flex items-center gap-1.5 whitespace-nowrap"><span aria-hidden className={cn('w-3 text-center', c)}>{g}</span>{label}</span>
}

function Bars({ n = 40, seed = 1, className }: { n?: number; seed?: number; className?: string }) {
  return (
    <div className={cn('flex h-16 items-end gap-px', className)}>
      {Array.from({ length: n }, (_, i) => <span key={i} className="flex-1 bg-foreground/70" style={{ height: `${15 + Math.abs(Math.sin((i + seed) / 5)) * 80}%` }} />)}
    </div>
  )
}

// ── Product tour screens (ink mocks for the ones the console shell lacks) ──

function Frame({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex h-full w-full flex-col">
      <div className="flex h-9 items-center gap-2 border-b px-3 text-xs">
        <LogoMark size={14} /><span className="font-semibold">temps</span><span className="text-muted-foreground">/ {title}</span>
      </div>
      <div className="flex-1 p-4">{children}</div>
    </div>
  )
}

function Screen({ tab, view, setView }: { tab: Tour; view: string; setView: (v: string) => void }) {
  switch (tab) {
    case 'Dashboard':
      return <div className="flex min-h-[520px] w-full"><ConsoleV1 view={view} go={setView} /></div>
    case 'Analytics':
      return (
        <Frame title="analytics · acme-storefront">
          <p className="op-status text-sm"><Glyph state="ok" label="12.4k visitors · 24h" /> <span className="mx-2 text-muted-foreground">·</span> +8% vs yesterday <span className="mx-2 text-muted-foreground">·</span> 3.1% checkout conversion <span className="mx-2 text-muted-foreground">·</span> LCP 1.9s</p>
          <div className="mt-4 grid gap-px border bg-border md:grid-cols-[2fr_1fr]">
            <div className="bg-background p-3"><p className="op-label">visitors / hour</p><Bars n={48} seed={3} className="mt-2 h-28" /></div>
            <div className="op-rows bg-background">
              {[['/', '4,120'], ['/pricing', '1,880'], ['/docs', '1,204'], ['/checkout', '612'], ['/blog/nextjs', '388']].map(([p, v]) => <div key={p} className="flex h-8 items-center justify-between px-3 text-xs"><span className="font-mono">{p}</span><span className="font-mono tabular-nums">{v}</span></div>)}
            </div>
          </div>
        </Frame>
      )
    case 'Error tracking':
      return (
        <Frame title="errors · api-gateway">
          <p className="op-status text-sm"><Glyph state="warn" label="3 unresolved" /> <span className="mx-2 text-muted-foreground">·</span> 1 new since dep_91a <span className="mx-2 text-muted-foreground">·</span> 31 events · 12 users</p>
          <div className="op-rows mt-4 border text-xs">
            {[['×', "TypeError: cannot read properties of undefined (reading 'id')", 'AddressForm.tsx:88', '31', 'new · dep_91a'], ['◐', 'ECONNRESET upstream billing', 'router.rs:212', '9', '2d'], ['◐', 'ZodError: email invalid', 'signup.ts:41', '4', '5d']].map(([g, t, f, n, w]) => (
              <div key={t} className="grid h-9 grid-cols-[16px_1fr_160px_40px_90px] items-center gap-2 px-3">
                <span className={g === '×' ? 'text-destructive' : 'text-warning'}>{g}</span><span className="truncate">{t}</span><span className="truncate font-mono text-muted-foreground">{f}</span><span className="text-right font-mono tabular-nums">{n}</span><span className="text-right text-muted-foreground">{w}</span>
              </div>
            ))}
          </div>
          <p className="mt-3 text-[11px] text-muted-foreground">Sentry-compatible DSN · source maps uploaded on deploy · the session that hit it is one click away.</p>
        </Frame>
      )
    case 'Tracing':
      return (
        <Frame title="trace · POST /checkout · 412ms">
          <div className="op-rows border font-mono text-[11px]">
            {[['http POST /checkout', 0, 100, 'ok'], ['auth.verify', 2, 6, 'ok'], ['db SELECT cart', 8, 18, 'ok'], ['billing.charge (upstream)', 26, 58, 'warn'], ['db INSERT order', 86, 8, 'ok'], ['email.send', 94, 5, 'ok']].map(([n, s, w, st], i) => (
              <div key={i} className="grid h-7 grid-cols-[220px_1fr] items-center px-3">
                <span className={cn('truncate', i > 0 && 'pl-3')}>{n}</span>
                <span className="relative h-3 bg-muted"><span className={cn('absolute inset-y-0', st === 'warn' ? 'bg-warning' : 'bg-foreground')} style={{ left: `${s}%`, width: `${w}%` }} /></span>
              </div>
            ))}
          </div>
          <p className="mt-3 text-[11px] text-muted-foreground">The slow span is the upstream call. Deploy markers on the trace list say whether it started with dep_91a.</p>
        </Frame>
      )
    case 'Uptime':
      return (
        <Frame title="uptime · 4 monitors">
          <div className="op-rows border text-xs">
            {[['api.acme.sh', 'ok', '99.98%', '184ms'], ['acme.sh', 'ok', '100%', '92ms'], ['status.acme.sh', 'ok', '100%', '61ms'], ['billing.internal', 'error', '97.10%', '—']].map(([h, s, u, l]) => (
              <div key={h} className="grid h-9 grid-cols-[1fr_1fr_80px_70px] items-center gap-2 px-3">
                <span className="font-mono">{h}</span>
                <span className="grid grid-cols-[repeat(60,minmax(0,1fr))] gap-px">{Array.from({ length: 60 }, (_, i) => <span key={i} className={cn('h-3', s === 'error' && i > 52 ? 'bg-destructive' : 'bg-success')} />)}</span>
                <span className="text-right font-mono tabular-nums">{u}</span><span className="text-right font-mono tabular-nums text-muted-foreground">{l}</span>
              </div>
            ))}
          </div>
        </Frame>
      )
    case 'AI Chat':
      return (
        <Frame title="ask · api-gateway">
          <div className="op-rows border text-xs">
            <div className="p-3"><span className="op-label">you</span><p className="op-prose mt-1">why did error rate go up after the last deploy?</p></div>
            <div className="p-3"><span className="op-label">temps</span><p className="op-prose mt-1">dep_91a (main@9bc61c0, 41m ago) introduced <code>AddressForm.tsx:88</code> reading <code>address.id</code> before the address loads. 31 events, 12 users, all on Safari. Session sess_9f31c reproduces it at 00:41. Roll back to dep_90e, or open a fix?</p>
              <div className="mt-2 flex gap-2"><Button size="sm" variant="outline" className="h-7 text-xs">roll back</Button><Button size="sm" className="op-primary h-7 text-xs">open fix in sandbox</Button></div>
            </div>
          </div>
          <p className="mt-3 text-[11px] text-muted-foreground">Reads deploys, errors, traces and replays. Writes only after you confirm.</p>
        </Frame>
      )
    case 'AI Agent Sandbox':
      return (
        <Frame title="sandbox · fix/address-id">
          <div className="op-inset border p-3 font-mono text-[11px] leading-5">
            <p><span className="text-muted-foreground">$</span> temps sandbox open api-gateway --from dep_91a</p>
            <p className="text-muted-foreground">workspace ready · node 22 · repo at 9bc61c0</p>
            <p><span className="text-muted-foreground">agent&gt;</span> reading src/checkout/AddressForm.tsx</p>
            <p><span className="text-muted-foreground">agent&gt;</span> guard: <span className="text-success">+ if (!address) return null</span></p>
            <p><span className="text-muted-foreground">agent&gt;</span> tests: 41 passed</p>
            <p><span className="text-muted-foreground">agent&gt;</span> preview: https://fix-address-id.api-gateway.preview.acme.sh</p>
            <p><span className="text-muted-foreground">$</span> <span className="animate-pulse">▍</span></p>
          </div>
          <p className="mt-3 text-[11px] text-muted-foreground">Every agent run gets its own container with the real repo and a preview URL. Nothing touches production until you merge.</p>
        </Frame>
      )
    case 'AI Gateway':
      return (
        <Frame title="ai gateway · last 24h">
          <p className="op-status text-sm"><Glyph state="ok" label="18,204 requests" /> <span className="mx-2 text-muted-foreground">·</span> $41.20 <span className="mx-2 text-muted-foreground">·</span> p95 1.9s <span className="mx-2 text-muted-foreground">·</span> 0.3% errors</p>
          <div className="op-rows mt-4 border text-xs">
            {[['claude-sonnet-5', '11,020', '$28.10', '1.7s'], ['claude-haiku-4.5', '6,412', '$4.90', '0.6s'], ['gpt-5.6', '772', '$8.20', '2.4s']].map(([m, r, c, l]) => (
              <div key={m} className="grid h-8 grid-cols-[1fr_90px_80px_60px] items-center px-3"><span className="font-mono">{m}</span><span className="text-right font-mono tabular-nums">{r}</span><span className="text-right font-mono tabular-nums">{c}</span><span className="text-right font-mono tabular-nums text-muted-foreground">{l}</span></div>
            ))}
          </div>
          <p className="mt-3 text-[11px] text-muted-foreground">One key for every provider, per-project budgets, cost per deployment.</p>
        </Frame>
      )
  }
}

// ── Page ────────────────────────────────────────────────────────────────


export function InkLandingV1Page({ full = false }: { /** Render without the sandbox layout: the landing as it would ship. Route `/landing`. */ full?: boolean }) {
  const [menu, setMenu] = useState(false)
  const [tab, setTab] = useState<Tour>('Dashboard')
  const [view, setView] = useState('projects')
  const [team, setTeam] = useState<string | null>(null)
  const [faq, setFaq] = useState<number | null>(0)
  const SAVINGS: Record<string, string> = { 'Just me': '$3.1k', '2–5': '$9.8k', '6–15': '$31k', '16+': '$74k' }

  return (
    <div className={full ? 'operator ink v1 min-h-screen' : 'operator ink v1 -m-4 sm:-m-6 lg:-m-8'} data-accent="signal">
      {/* Frozen: one accent (signal), on the primary CTA only. No switcher. */}
      {/* Sandbox control, not part of the landing: toggles the chrome-free route. */}
      <Link to={full ? '/v1-landing' : '/landing'} aria-label={full ? 'Exit full screen' : 'Full screen'} title={full ? 'back to the sandbox page' : 'the landing alone, no sandbox chrome'} className="fixed bottom-4 right-4 z-40 inline-flex h-8 w-8 items-center justify-center border bg-background text-foreground shadow-sm hover:bg-muted [&_svg]:h-3.5 [&_svg]:w-3.5">{full ? <Minimize2 /> : <Maximize2 />}</Link>
      {/* Nav */}
      <header className="sticky top-0 z-30 grid grid-cols-[auto_1fr_auto] border-b bg-background">
        <a href="#" className="flex h-12 items-center gap-2 border-r px-4"><LogoMark size={20} /><span className="text-sm font-semibold">Temps</span></a>
        <nav className="hidden items-stretch lg:flex">
          {NAV.map((n, i) => (
            <a key={n} href="#" className={cn('flex items-center px-4 text-sm hover:bg-foreground hover:text-background', i > 0 && 'border-l')}>
              {n}{n === 'Managed' && <span className="ml-2 border px-1.5 text-[10px] uppercase tracking-[0.1em]">beta</span>}
            </a>
          ))}
        </nav>
        <div className="flex items-stretch">
          <a href="#" className="flex items-center gap-2 border-l px-4 text-sm hover:bg-foreground hover:text-background"><Star className="h-4 w-4" /> <span className="font-mono text-xs">712</span></a>
          <a href="#" className="bg-foreground text-background flex items-center px-4 text-sm">Download</a>
          <button type="button" aria-label={menu ? 'Close menu' : 'Menu'} aria-expanded={menu} onClick={() => setMenu((m) => !m)} className="flex items-center border-l px-3 lg:hidden">{menu ? <X className="h-4 w-4" /> : <Menu className="h-4 w-4" />}</button>
        </div>
        {menu && (
          <nav className="col-span-3 grid border-t lg:hidden">
            {NAV.map((n) => <a key={n} href="#" onClick={() => setMenu(false)} className="flex h-11 items-center border-b px-4 text-sm last:border-b-0">{n}</a>)}
          </nav>
        )}
      </header>

      {/* Hero */}
      <section className="px-4 pb-10 pt-14 sm:px-8 lg:pt-20">
        <div className="mx-auto max-w-6xl text-center">
          <p className="op-label">Self-hosted deploy tools stop at deploy</p>
          <h1 className="op-display mx-auto mt-5 max-w-4xl">Stop paying for 7 SaaS tools.</h1>

          {/* Tool strip → temps */}
          <div className="mx-auto mt-8 flex max-w-3xl flex-wrap items-center justify-center gap-3">
            <div className="grid grid-cols-4 border sm:grid-cols-7">
              {TOOLS.map(([name, mark], i) => (
                <div key={name} className={cn('flex h-16 w-20 flex-col items-center justify-center gap-1.5', i % 4 !== 0 && 'border-l', i >= 4 && 'border-t', 'sm:border-t-0', i > 0 && 'sm:border-l')}>
                  <span className="flex h-5 w-5 items-center justify-center [&_svg]:h-5 [&_svg]:w-5"><PlatformLogo name={mark} /></span><span className="op-label">{name}</span>
                </div>
              ))}
            </div>
            <ArrowRight className="h-5 w-5 text-muted-foreground" />
            <div className="bg-foreground text-background flex h-16 items-center gap-2 px-5"><LogoMark size={22} variant="dark" /><span className="text-xl font-semibold">temps</span></div>
          </div>

          <p className="op-lead mx-auto mt-8 max-w-2xl">One self-hosted Rust binary replaces your deployment platform, analytics, error tracking, session replay, uptime monitoring, transactional email and AI sandboxes. <strong>No per-seat pricing, no usage bills.</strong></p>
          <div className="mt-8 flex flex-wrap items-center justify-center gap-3">
            <Button size="lg" className="op-primary h-11 text-sm"><Download /> Download for macOS</Button>
            <a href="#" className="text-sm underline underline-offset-4">All downloads</a>
          </div>
          <p className="mt-6 text-xs text-muted-foreground">or install from the command line</p>
          <div className="mx-auto mt-2 flex max-w-xl items-center justify-between gap-2 border bg-background px-3 py-2 font-mono text-sm">
            <span className="truncate"><span className="text-muted-foreground">$ </span>curl -fsSL https://temps.sh/deploy.sh | bash</span>
            <CopyButton value="curl -fsSL https://temps.sh/deploy.sh | bash" minimal label="Copy install command" className="h-6 w-6 shrink-0" />
          </div>
        </div>
      </section>

      {/* Product tour: 8 tabs, same as the live page */}
      <section className="px-4 pb-14 sm:px-8">
        <div className="mx-auto max-w-6xl">
          <div className="flex flex-wrap border">
            {TOUR.map((t, i) => (
              <button key={t} type="button" onClick={() => setTab(t)} className={cn('flex h-9 items-center px-3 text-xs sm:px-4', i > 0 && 'border-l', tab === t ? 'bg-foreground text-background' : 'hover:bg-muted')}>{t}</button>
            ))}
          </div>
          <div className="op-raise mt-3 flex min-h-[520px] overflow-hidden bg-background">
            <Screen tab={tab} view={view} setView={setView} />
          </div>
          <p className="op-prose mt-3 text-center text-sm text-muted-foreground">All your projects, visitors, and status in one place. Drawn with the console's own components, not a screenshot.</p>
        </div>
      </section>

      {/* Migrate from */}
      <Section eyebrow="already running somewhere else?" title="Temps imports your existing setup — apps, databases with their data, domains, and environment variables." lead="Point it at what you have. Importers read the config, provision the equivalent, copy the data, and leave DNS for you to cut over when you are ready.">
        <div className="grid grid-cols-4 gap-px border bg-border sm:grid-cols-6 lg:grid-cols-12">
          {MIGRATE.map((n) => (
            <a key={n} href="#" className="group flex h-20 flex-col items-center justify-center gap-2 bg-background px-2 text-center text-[11px] hover:bg-muted">
              <span className="flex h-6 w-6 items-center justify-center grayscale group-hover:grayscale-0 [&_svg]:h-6 [&_svg]:w-6"><PlatformLogo name={n} /></span>
              <span className="truncate">{n}</span>
            </a>
          ))}
        </div>
        <div className="mt-4 flex flex-wrap items-center gap-x-6 gap-y-1 text-xs text-muted-foreground">
          <span className="inline-flex items-center gap-1"><Check className="h-3.5 w-3.5 text-foreground" /> apps and build settings</span>
          <span className="inline-flex items-center gap-1"><Check className="h-3.5 w-3.5 text-foreground" /> databases with their data</span>
          <span className="inline-flex items-center gap-1"><Check className="h-3.5 w-3.5 text-foreground" /> domains and certificates</span>
          <span className="inline-flex items-center gap-1"><Check className="h-3.5 w-3.5 text-foreground" /> environment variables</span>
          <a href="#" className="inline-flex items-center gap-1 underline underline-offset-4">import guide <ArrowRight className="h-3 w-3" /></a>
        </div>
      </Section>

      {/* Engine: the live SystemMapSection from temps-landing, verbatim */}
      <section className="border-t">
        <SystemMapSection />
      </section>

      {/* Self-hosted */}
      <Section tier="minor" eyebrow="where it runs" title="Self-hosted, on a server you already own." lead="Run the whole platform on any Linux box — a VPS, bare metal, or your homelab. One binary, nothing to license, nothing metered.">
        <div className="grid border md:grid-cols-2">
          <ul className="op-rows text-sm">
            {['Apache 2.0 / MIT dual-licensed, free forever', 'Any VPS — Hetzner, DigitalOcean, bare metal, or your own hardware', 'No usage-based billing, ever', 'Plain Docker containers and Postgres underneath — no proprietary format to migrate off of'].map((l) => <li key={l} className="flex items-start gap-3 px-4 py-3"><Check className="mt-0.5 h-4 w-4 shrink-0" /><span className="op-prose">{l}</span></li>)}
          </ul>
          <div className="op-inset flex flex-col justify-between border-t p-4 font-mono text-xs leading-6 md:border-l md:border-t-0">
            <div>
              <p><span className="text-muted-foreground">$</span> curl -fsSL https://temps.sh/deploy.sh | bash</p>
              <p className="text-muted-foreground">→ temps 0.1.0 installed · postgres ready · proxy on :443</p>
              <p><span className="text-muted-foreground">$</span> temps serve</p>
              <p className="text-success">● console at https://temps.your-server.sh</p>
            </div>
            <a href="#" className="mt-4 inline-flex items-center gap-1 text-sm underline underline-offset-4">Try it yourself — quickstart guide <ArrowRight className="h-3.5 w-3.5" /></a>
          </div>
        </div>
      </Section>

      {/* Migration audit */}
      <Section tone="muted" eyebrow="for teams considering temps" title="See if a free migration audit makes sense for your team." lead="The biggest reason teams don't switch isn't price — it's migration risk. For teams paying $750+/mo on cloud + developer SaaS, we run a free 30-minute call to map every line of your bill to a Temps equivalent and sketch the path.">
        <div className="grid gap-6 lg:grid-cols-[1fr_1fr]">
          <div className="op-rows border">
            <div className="flex h-9 items-center justify-between px-4"><span className="op-label">what the call covers · 30 min · screen share</span></div>
            {[['Audit your current stack.', 'Walk us through Vercel, Sentry, LogRocket, Datadog, your databases — whatever you’re paying for. We map every line of your bill to a Temps equivalent.'], ['Get a real number back.', 'You leave the call with an exact monthly savings figure for your team size, traffic, and feature usage. No marketing math — your actual line items.'], ['Get a migration plan.', 'We sketch the path: which service moves first, where DNS cuts over, how to dual-run during the swap. Yours to take, even if you self-host and never speak to us again.']].map(([t, d], i) => (
              <div key={t} className="grid grid-cols-[32px_1fr] gap-3 p-4"><span className="font-mono text-xs text-muted-foreground">0{i + 1}</span><div><p className="text-sm font-medium">{t}</p><p className="op-prose mt-1 text-sm text-muted-foreground">{d}</p></div></div>
            ))}
          </div>
          <div className="op-raise self-start bg-background">
            <div className="flex h-9 items-center justify-between border-b px-4"><span className="op-label">free savings estimate</span><span className="font-mono text-xs text-muted-foreground">1/3</span></div>
            <div className="p-4">
              <p className="op-label">potential savings</p>
              <p className="mt-1 font-mono text-4xl tabular-nums">{team ? SAVINGS[team] : '—'}<span className="text-sm text-muted-foreground"> / year</span></p>
              <p className="op-prose mt-1 text-xs text-muted-foreground">{team ? `Typical for a team of ${team.toLowerCase()} on cloud + developer SaaS.` : 'Three quick questions to see your number.'}</p>
              <p className="mt-6 text-sm font-medium">How many developers will use Temps?</p>
              <div className="mt-2 grid grid-cols-2 border">
                {['Just me', '2–5', '6–15', '16+'].map((t, i) => (
                  <button key={t} type="button" onClick={() => setTeam(t)} className={cn('h-10 text-sm', i % 2 === 1 && 'border-l', i >= 2 && 'border-t', team === t ? 'bg-foreground text-background' : 'hover:bg-muted')}>{t}</button>
                ))}
              </div>
              <Button className="op-primary mt-4 h-9 w-full text-sm" disabled={!team}>Next <ArrowRight /></Button>
            </div>
          </div>
        </div>
        <div className="mt-6 border p-4 text-sm">
          <p className="font-medium">This call is for you if…</p>
          <ul className="mt-2 grid gap-2 md:grid-cols-2">
            {['You’re a team paying $750+/mo across cloud + developer SaaS.', 'You’ve already eyed self-hosted alternatives but stalled on migration risk.', 'You want predictable infrastructure costs as the team grows next year.', 'You need someone to look at your specific stack — not generic docs.'].map((l) => <li key={l} className="flex items-start gap-2 text-muted-foreground"><Check className="mt-0.5 h-4 w-4 shrink-0 text-foreground" /><span className="op-prose">{l}</span></li>)}
          </ul>
        </div>
      </Section>

      {/* What's actually different */}
      <Section eyebrow="no adjectives" title="Here’s what’s actually different" lead="Not a nicer dashboard. Categories most self-hosted deploy tools don’t have at all.">
        <div className="grid gap-px border bg-border md:grid-cols-2 lg:grid-cols-3">
          {DIFFERENT.map(([t, d], i) => (
            <div key={t} className="bg-background p-5">
              <p className="font-mono text-xs text-muted-foreground">0{i + 1}</p>
              <h3 className="op-h3 mt-2">{t}</h3>
              <p className="op-prose mt-2 text-sm text-muted-foreground">{d}</p>
            </div>
          ))}
        </div>
      </Section>

      {/* Count what you're running */}
      <Section tone="muted" eyebrow="the honest math" title="A deploy tool isn’t a platform. Count what you’re actually running.">
        <div className="grid border md:grid-cols-2">
          <div className="p-5">
            <p className="op-label">deploying self-hosted usually means running</p>
            <ul className="op-rows mt-3 text-sm">{COUNT_SELF.map((l, i) => <li key={l} className="flex items-center gap-3 py-2"><span className="font-mono text-xs text-muted-foreground">{String(i + 1).padStart(2, '0')}</span><span className="op-prose">{l}</span></li>)}</ul>
            <p className="mt-3 font-mono text-sm">7 things · 7 logins · 7 upgrade cycles</p>
          </div>
          <div className="bg-foreground text-background flex flex-col justify-between border-t p-5 md:border-l md:border-t-0">
            <div>
              <p className="op-label !text-current opacity-70">with temps</p>
              <ul className="mt-3 text-sm"><li className="flex items-center gap-3 py-2"><Check className="h-4 w-4" /> Deploy, errors, tracing, uptime, analytics, AI sandbox</li></ul>
            </div>
            <p className="mt-6 font-mono text-4xl tabular-nums">1 thing<span className="text-base opacity-70"> · 1 login · 1 binary to upgrade</span></p>
          </div>
        </div>
      </Section>

      {/* Genuinely different */}
      <Section tier="minor" eyebrow="because it is one process" title="Where Temps is genuinely different">
        <div className="op-rows border">
          {GENUINE.map(([t, d], i) => (
            <div key={t} className="grid gap-2 px-5 py-4 md:grid-cols-[40px_1fr_1.4fr] md:items-baseline">
              <span className="font-mono text-xs text-muted-foreground">0{i + 1}</span>
              <h3 className="op-h3">{t}</h3>
              <p className="op-prose text-sm text-muted-foreground">{d}</p>
            </div>
          ))}
        </div>
      </Section>

      {/* Infra */}
      <Section tier="minor" tone="muted" eyebrow="underneath" title="The same infrastructure your team already trusts.">
        <div className="grid grid-cols-4 gap-px border bg-border sm:grid-cols-6 lg:grid-cols-12">
          {INFRA.map((n) => (
            <div key={n} className="group flex h-20 flex-col items-center justify-center gap-2 bg-background px-2 text-center text-[11px]">
              <span className="flex h-6 w-6 items-center justify-center grayscale group-hover:grayscale-0 [&_svg]:h-6 [&_svg]:w-6"><PlatformLogo name={n} /></span>
              <span className="truncate">{n}</span>
            </div>
          ))}
        </div>
      </Section>

      {/* AI agent */}
      <Section eyebrow="ai, where the data is" title="Not a chat window bolted onto a dashboard. An agent that can actually see what broke." lead="It reads the deploy, the error, the trace and the replay because they are in the same process. It proposes a fix in a sandbox with a preview URL. You confirm.">
        <div className="op-raise flex min-h-[420px] overflow-hidden bg-background"><Screen tab="AI Chat" view={view} setView={setView} /></div>
        <div className="mt-4 flex flex-wrap gap-4 text-xs text-muted-foreground">
          <span className="inline-flex items-center gap-1"><Bot className="h-3.5 w-3.5" /> propose-then-confirm writes</span>
          <span className="inline-flex items-center gap-1"><Terminal className="h-3.5 w-3.5" /> isolated sandbox per run</span>
          <span className="inline-flex items-center gap-1"><Video className="h-3.5 w-3.5" /> replay attached to every error</span>
          <span className="inline-flex items-center gap-1"><Database className="h-3.5 w-3.5" /> reads your data, never leaves your server</span>
        </div>
      </Section>

      {/* One binary */}
      <section className="op-fill border-t px-4 py-16 sm:px-8">
        <div className="mx-auto max-w-6xl">
          <p className="op-label !text-current opacity-70">the whole point</p>
          <h2 className="op-h1 mt-3 max-w-3xl">One binary. Every feature. Nothing bolted on.</h2>
          <div className="mt-8 flex flex-wrap items-center gap-3">
            <Button size="lg" className="h-11 border border-current bg-transparent text-sm text-current hover:bg-background/10"><Download /> Download for macOS</Button>
            <span className="font-mono text-sm opacity-80">$ curl -fsSL https://temps.sh/deploy.sh | bash</span>
          </div>
        </div>
      </section>

      {/* Pricing: from temps-landing/public/pricing.md. Self-host is the product; Cloud is the upsell. */}
      <Section eyebrow="pricing" title="Free to self-host. Cloud when you want someone else to hold the backups." lead="Temps Cloud does not gate any self-hosted feature. It takes telemetry retention, offsite backups and AI credits off your own server. No per-seat fees on any plan.">
        <div className="grid border md:grid-cols-5">
          {PRICING.map((t, i) => (
            <div key={t.name} className={cn('flex flex-col p-5', i > 0 && 'border-t md:border-l md:border-t-0', t.hot && 'bg-muted')}>
              <p className="op-label">{t.name}{t.hot && ' · most popular'}</p>
              <p className="mt-2 text-2xl font-semibold tracking-tight">{t.price}<span className="text-sm font-normal text-muted-foreground">{t.per}</span></p>
              <ul className="op-rows mt-4 flex-1 text-xs [&>li]:py-1.5">
                {t.rows.map((r) => <li key={r}>{r}</li>)}
              </ul>
              <Button size="sm" className={cn('mt-4 h-9 w-full text-xs', i === 0 ? 'op-primary' : 'border bg-transparent text-foreground hover:bg-foreground hover:text-background')} variant={i === 0 ? 'default' : 'outline'}>{t.cta}</Button>
            </div>
          ))}
        </div>
        <p className="mt-3 text-xs text-muted-foreground">Backup-storage and AI-credit overage are not billed today: no meter exists, nothing beyond the included amount is charged. The only usage charge anywhere is Business telemetry past 1 TB, and only after you set a hard monthly cap.</p>
      </Section>

      {/* What the console does at a limit: the promise pricing makes, shown as UI */}
      <Section tier="minor" tone="muted" eyebrow="when you hit a limit" title="The console says so. It never silently drops anything.">
        <div className="op-rows border bg-background text-sm">
          {LIMITS.map(([limit, shows]) => (
            <div key={limit} className="grid gap-2 px-4 py-3 md:grid-cols-[1fr_2fr]">
              <span className="font-medium">{limit}</span>
              <span className="font-mono text-xs"><span aria-hidden className="text-muted-foreground">◌ </span>{shows}</span>
            </div>
          ))}
        </div>
      </Section>

      {/* FAQ */}
      <Section tier="minor" eyebrow="questions" title="Frequently asked questions">
        <div className="op-rows border">
          {FAQ.map(([q, a], i) => (
            <div key={q}>
              <button type="button" onClick={() => setFaq(faq === i ? null : i)} className="flex w-full items-center justify-between gap-4 px-5 py-4 text-left text-sm font-medium hover:bg-muted" aria-expanded={faq === i}>
                <span>{q}</span><ChevronDown className={cn('h-4 w-4 shrink-0 text-muted-foreground', faq === i && 'rotate-180')} />
              </button>
              {faq === i && <p className="op-prose border-t px-5 py-4 text-sm text-muted-foreground">{a}</p>}
            </div>
          ))}
        </div>
      </Section>

      {/* Footer */}
      <footer className="grid gap-6 border-t px-4 py-8 text-xs text-muted-foreground sm:px-8 md:grid-cols-[1fr_auto] md:items-center">
        <div className="flex flex-wrap gap-x-6 gap-y-2">{['Docs', 'Changelog', 'GitHub', 'Discord', 'Security', 'Status', 'Pricing', 'Enterprise'].map((l) => <a key={l} href="#" className="hover:text-foreground">{l}</a>)}</div>
        <div className="flex items-center gap-3"><span className="font-mono">v0.1.0</span><span>·</span><Link to="/v1" className="underline underline-offset-4">console v1</Link></div>
      </footer>
    </div>
  )
}
