// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { ArrowUpCircle, Bell, Database, Gauge, Globe, Hammer, Hourglass, Key, KeyRound, Network, Puzzle, RefreshCw, Route, Server, ShieldCheck, Timer, Users, UsersRound, type LucideIcon } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Callout, EchoDialog, Field, Ledger, PageTitle, Phrase, Picker, SecretValue, Section, Settings, Status, StatusLine, GLYPH, GLYPH_CLASS,
  type LedgerRow, type State,
} from '@/components/op'
import { Toggle } from './ConsoleV1Admin'
import type { Notify } from './ConsoleV1Observe'
import { NodesLedger, ClusterPage } from './ConsoleV1Nodes'

/**
 * Settings, organised by what the operator is doing rather than by which
 * row of the settings table a page writes. Five groups, fourteen pages
 * (from twenty): what was three "monitoring" pages is Store · Retention ·
 * Alerts; rate limiting and IP rules are one page; registry and build
 * limits are one page; Version is Updates and says so; Plugins and Nodes
 * are the fleet. Every page carries its verdict, and every field says when
 * it takes effect: now · next request · restart. The hub shows each page's
 * current value so nobody opens a page to find out whether it is set.
 */

type Effect = 'now' | 'next request' | 'restart'
const EFFECT: Record<Effect, State> = { now: 'ok', 'next request': 'idle', restart: 'warn' }
export const eff = (e: Effect, more?: string) => `${more ? more + ' · ' : ''}takes effect: ${e}`

type Page = { slug: string; /** What kind of thing the page is about: a mark in a fixed slot so the hub scans by kind without reading. State stays with the glyph. */ icon: LucideIcon; title: string; group: string; value: string; state?: State; note?: string }
export const SETTINGS_GROUPS: { group: string; why: string; pages: Page[] }[] = [
  { group: 'instance', why: 'set at install, rarely again', pages: [
    { slug: 'domain', icon: Globe, title: 'Domain & TLS', group: 'instance', value: 'temps.acme.sh · Let\'s Encrypt production', state: 'error', note: 'no ACME contact email: renewals will fail' },
    { slug: 'updates', icon: ArrowUpCircle, title: 'Updates', group: 'instance', value: 'v0.1.0 · stable · self-update on', state: 'ok', note: 'up to date' },
    { slug: 'builds', icon: Hammer, title: 'Builds & registry', group: 'instance', value: '2 concurrent · no limits · registry off', state: 'warn', note: 'concurrency 4 pending restart' },
    { slug: 'timeouts', icon: Timer, title: 'Timeouts', group: 'instance', value: 'max 600s · http none · sse none · ws none' },
  ] },
  { group: 'access', why: 'who can do what', pages: [
    { slug: 'users', icon: Users, title: 'Users', group: 'access', value: '6 users · 2 admins · 1 invited' },
    { slug: 'teams', icon: UsersRound, title: 'Teams', group: 'access', value: '2 teams · 3 projects assigned' },
    { slug: 'signin', icon: KeyRound, title: 'Sign-in', group: 'access', value: 'password + Google SSO · console open to any IP', state: 'warn', note: 'admin gate off' },
    { slug: 'keys', icon: Key, title: 'API keys', group: 'access', value: '4 active · 1 expires in 6d', state: 'warn', note: 'ci-deploy expires in 6d' },
  ] },
  { group: 'edge', why: 'what the proxy does to every request', pages: [
    { slug: 'headers', icon: ShieldCheck, title: 'Security headers', group: 'edge', value: 'strict preset · HSTS 1y · frame deny' },
    { slug: 'traffic', icon: Gauge, title: 'Traffic rules', group: 'edge', value: 'rate limit off · 2 blocked IPs · 1 allowed range' },
    { slug: 'routes', icon: Route, title: 'Custom routes', group: 'edge', value: '3 routes · 1 without TLS', state: 'warn', note: 'legacy.acme.sh has no certificate' },
  ] },
  { group: 'data', why: 'where telemetry goes and how long it stays', pages: [
    { slug: 'store', icon: Database, title: 'Store', group: 'data', value: 'TimescaleDB · scrape 15s · 12 services' },
    { slug: 'retention', icon: Hourglass, title: 'Retention', group: 'data', value: 'raw 7d · hourly 90d · daily 2y · logs 14d' },
    { slug: 'alerts', icon: Bell, title: 'Alerts', group: 'data', value: 'email + slack · disk at 80% · 2 rules' },
  ] },
  { group: 'fleet', why: 'the machines and code this instance runs', pages: [
    { slug: 'nodes', icon: Server, title: 'Nodes', group: 'fleet', value: '3 nodes · hetzner-3 offline 4m · hetzner-1 at 91% memory', state: 'error', note: 'hetzner-3 offline' },
    { slug: 'cluster', icon: Network, title: 'Cluster', group: 'fleet', value: 'join token valid 23h · dns on · CA from 2026-03' },
    { slug: 'plugins', icon: Puzzle, title: 'Plugins', group: 'fleet', value: '3 loaded · 0 failed' },
  ] },
]
const ALL = SETTINGS_GROUPS.flatMap((g) => g.pages)
/** What each page is about, for the title meta: it places the page, it is not a crumb trail. */
const ABOUT: Record<string, string> = {
  domain: 'external URL, preview domain and certificates',
  updates: 'version, channel and the restart window',
  builds: 'build limits and the image registry',
  timeouts: 'how long the proxy waits',
  users: 'who can sign in',
  teams: 'which projects a group of people sees',
  signin: 'sign-in methods and the admin gate',
  keys: 'tokens that act for a user',
  headers: 'headers added to every response',
  traffic: 'rate limits and IP rules',
  routes: 'domains the proxy serves that are not a project',
  store: 'where telemetry is written',
  retention: 'how long telemetry is kept',
  alerts: 'where alerts are sent',
  nodes: 'the machines this instance runs on',
  cluster: 'joining, cluster DNS and trust',
  plugins: 'what this binary loads',
}
const attention = ALL.filter((p) => p.state === 'error' || p.state === 'warn')

// ── Hub ──────────────────────────────────────────────────────────────
export function SettingsHub({ go }: { go: (v: string) => void }) {
  const errors = attention.filter((p) => p.state === 'error')
  const status = (
    <StatusLine state={errors.length ? 'error' : 'warn'} more={{ label: `+${attention.length - 1}`, items: attention.slice(1).map((p) => ({ state: p.state as State, children: <><Phrase onClick={() => go(`settings:${p.slug}`)}>{p.title}</Phrase>: {p.note}.</> })) }}>
      <Phrase onClick={() => go(`settings:${attention[0].slug}`)}>{attention[0].title}</Phrase>: {attention[0].note}. {attention.length - 1} more need a look.
    </StatusLine>
  )
  return (
    <div className="space-y-4">
      <PageTitle title="Settings" meta={`this instance · v0.1.0 · hetzner-1 · ${ALL.length} pages`} />
      {status}
      <div className="op-grid grid gap-6 md:grid-cols-2">
        {SETTINGS_GROUPS.map((g) => (
          <Section key={g.group} title={g.group} meta={g.why}>
            <ol className="op-rows border bg-background text-xs">
              {g.pages.map((p) => (
                <li key={p.slug}>
                  <button type="button" onClick={() => go(`settings:${p.slug}`)} className="flex w-full items-baseline gap-3 px-3 py-2 text-left hover:bg-muted/40">
                    {/* Mark first (the eye scans the left edge by kind), then the name, then the value; the state glyph opens the value it qualifies, so no row carries an empty slot for a problem it does not have. */}
                    <p.icon aria-hidden className="h-3.5 w-3.5 shrink-0 translate-y-0.5 text-muted-foreground" />
                    <span className="w-32 shrink-0 font-medium">{p.title}</span>
                    <span className={`min-w-0 flex-1 truncate font-mono text-[11px] ${p.state === 'error' ? 'text-destructive' : p.state === 'warn' ? 'text-warning' : 'text-muted-foreground'}`}>{p.state && p.state !== 'ok' && <span aria-hidden className={`mr-1.5 ${GLYPH_CLASS[p.state]}`}>{GLYPH[p.state]}</span>}{p.note ?? p.value}</span>
                  </button>
                </li>
              ))}
            </ol>
          </Section>
        ))}
      </div>
      <p className="font-mono text-[11px] text-muted-foreground">× broken · ◐ needs a look · each row shows the current value, not a description · ⌘K finds any setting by name</p>
    </div>
  )
}

// ── Pages ────────────────────────────────────────────────────────────
function Eff({ e }: { e: Effect }) { return <span className="ml-2 font-mono text-[11px] text-muted-foreground"><span aria-hidden className={GLYPH_CLASS[EFFECT[e]]}>{GLYPH[EFFECT[e]]}</span> {e}</span> }

/** The legend every form page ends with, so "takes effect: restart" reads as one of three known answers. */
export function EffectLegend() {
  return <p className="font-mono text-[11px] text-muted-foreground">takes effect: <Eff e="now" /> <Eff e="next request" /> <Eff e="restart" /></p>
}

export function SettingsPage({ slug, dense, notify, go }: { slug: string; dense: boolean; notify: Notify; go: (v: string) => void }) {
  const page = ALL.find((p) => p.slug === slug) ?? ALL[0]
  const [dirty, setDirty] = useState(false)
  const [q, setQ] = useState('')
  const touch = () => setDirty(true)
  const save = () => { setDirty(false); notify('ok', `${page.title} saved`) }
  // The trail is the shell's job; the meta places the page (group · what it is about), and is never a link.
  const meta = `${page.group} · ${ABOUT[page.slug] ?? page.title.toLowerCase()}`
  const restartDialog = <EchoDialog trigger={<Button size="sm" className="op-primary h-7 text-xs"><RefreshCw /> restart now</Button>} title="Restart temps" description="The console and proxy are unavailable for a few seconds; running deployments continue. Type restart to confirm." confirmWord="restart" steps={['drain in-flight requests', 'restart process', 'wait for readyz']} onDone={() => notify('ok', 'restarted', 'build concurrency is now 4')} />

  // Ledger pages: users · teams · keys · routes · nodes · plugins. One ledger each.
  if (slug === 'users') {
    const U = [['maya', 'maya@acme.sh', 'owner', 'now', 'Google'], ['jules', 'jules@acme.sh', 'admin', '2h ago', 'Google'], ['sam', 'sam@acme.sh', 'member', '3d ago', 'password'], ['ci-bot', 'ci@acme.sh', 'deployer', '4m ago', 'api key'], ['mara', 'mara@acme.sh', 'viewer', 'never', 'invited 2d ago'], ['tom', 'tom@acme.sh', 'member', '9d ago', 'password']]
    const rows: LedgerRow[] = U.filter((u) => u[0].toLowerCase().includes(q.trim().toLowerCase()) || u[1].toLowerCase().includes(q.trim().toLowerCase())).map((u) => ({ id: u[0], state: u[3] === 'never' ? 'idle' : 'ok', onOpen: () => notify('ok', `open ${u[0]}`, 'role, teams, sessions, keys'),
      mobile: <><span className="block font-medium">{u[0]} <span className="text-muted-foreground">· {u[2]}</span></span><span className="block text-[11px] text-muted-foreground">{u[1]} · {u[3]}</span></>,
      cells: [<span className="font-medium">{u[0]}</span>, <span className="font-mono text-muted-foreground">{u[1]}</span>, <span>{u[2]}</span>, <span className="text-muted-foreground">{u[4]}</span>, <span className="text-muted-foreground">{u[3]}</span>] }))
    return <Ledger title="Users" meta={meta} dense={dense} status={<StatusLine state="idle">mara was invited 2d ago and has not signed in. The invite expires in 5d.</StatusLine>} columns={['user', 'email', 'role', 'signs in with', 'last seen']} grid="minmax(8rem,1fr) minmax(12rem,1.5fr) minmax(6rem,max-content) minmax(8rem,1fr) minmax(70px,max-content)" rows={rows} total={U.length} filter={q} onFilter={setQ} placeholder="filter users" hint="○ never signed in" action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'invite', 'email + role; the link is valid 7 days')}>invite</Button>} footer={<span>roles: owner · admin · member · deployer · viewer · a user in no team sees every project</span>} />
  }
  if (slug === 'teams') {
    const T = [['platform', '3 members', 'all projects', 'admin'], ['storefront', '4 members', 'acme-storefront, acme-crm', 'member']]
    const rows: LedgerRow[] = T.map((t) => ({ id: t[0], state: 'ok', onOpen: () => notify('ok', `open team ${t[0]}`), mobile: <><span className="block font-medium">{t[0]}</span><span className="block text-[11px] text-muted-foreground">{t[1]} · {t[2]}</span></>, cells: [<span className="font-medium">{t[0]}</span>, <span className="text-muted-foreground">{t[1]}</span>, <span className="truncate text-muted-foreground">{t[2]}</span>, <span>{t[3]}</span>] }))
    return <Ledger title="Teams" meta={meta} dense={dense} status={<StatusLine state="ok">Two teams. Members outside a team see every project; a team narrows that to its projects.</StatusLine>} columns={['team', 'members', 'projects', 'role in them']} grid="minmax(8rem,1fr) minmax(6rem,max-content) minmax(12rem,2fr) minmax(6rem,max-content)" rows={rows} total={T.length} filter={q} onFilter={setQ} placeholder="filter teams" action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'new team')}>new team</Button>} />
  }
  if (slug === 'keys') {
    const K = [['ci-deploy', 'tk_HJfH…', 'deploy, projects:read', 'ci-bot', '4m ago', '6d', 'warn'], ['cli-maya', 'tk_5heD…', 'all', 'maya', '1h ago', '82d', 'ok'], ['status-reader', 'tk_P2nm…', 'monitors:read', 'jules', '3d ago', 'never', 'ok'], ['old-import', 'tk_EVvj…', 'imports', 'sam', '41d ago', 'expired', 'idle']]
    const rows: LedgerRow[] = K.filter((k) => k[0].toLowerCase().includes(q.trim().toLowerCase())).map((k) => ({ id: k[0], state: k[6] as State, onOpen: () => notify('ok', `open key ${k[0]}`, 'scopes, last uses, rotate'), mobile: <><span className="block font-mono">{k[0]}</span><span className="block text-[11px] text-muted-foreground">{k[2]} · expires {k[5]}</span></>, cells: [<span className="font-mono">{k[0]}</span>, <span className="font-mono text-muted-foreground">{k[1]}</span>, <span className="truncate text-muted-foreground">{k[2]}</span>, <span>{k[3]}</span>, <span className="text-muted-foreground">{k[4]}</span>, k[6] === 'ok' ? <span>{k[5]}</span> : <Status state={k[6] as State} label={k[5]} />] }))
    return <Ledger title="API keys" meta={meta} dense={dense} status={<StatusLine state="warn"><Phrase onClick={() => notify('ok', 'rotate ci-deploy', 'new secret shown once; old one valid 24h')}>ci-deploy</Phrase> expires in 6 days. CI deploys stop when it does; rotate it now and the old key keeps working for 24h.</StatusLine>} columns={['key', 'prefix', 'scopes', 'owner', 'last used', 'expires']} grid="minmax(8rem,1fr) minmax(6rem,max-content) minmax(10rem,1.5fr) minmax(5rem,max-content) minmax(70px,max-content) minmax(60px,max-content)" rows={rows} total={K.length} filter={q} onFilter={setQ} placeholder="filter keys" hint="◐ expires within 7d · ○ expired" action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'new key', 'name · scopes · expiry; the secret is shown once')}>new key</Button>} footer={<span>a key acts as its owner, narrowed to its scopes · secrets are shown once, at creation</span>} />
  }
  if (slug === 'routes') {
    const R = [['legacy.acme.sh', 'http://10.0.3.9:8080', 'no', 'warn', 'no certificate · served over http'], ['grafana.acme.sh', 'http://10.0.3.4:3000', 'yes', 'ok', ''], ['s3.acme.sh', 'http://10.0.3.7:9000', 'yes', 'ok', 'websocket on']]
    const rows: LedgerRow[] = R.map((r) => ({ id: r[0], state: r[3] as State, onOpen: () => notify('ok', `open route ${r[0]}`), mobile: <><span className="block font-mono">{r[0]}</span><span className="block text-[11px] text-muted-foreground">{r[4] || `→ ${r[1]}`}</span></>, cells: [<span className="font-mono">{r[0]}</span>, <span className="font-mono text-muted-foreground">{r[1]}</span>, <span>{r[2]}</span>, <Status state={r[3] as State} label={r[4]} />] }))
    return <Ledger title="Custom routes" meta={meta} dense={dense} status={<StatusLine state="warn"><span className="font-mono">legacy.acme.sh</span> is served over plain http: no certificate was requested because TLS is off for the route.</StatusLine>} columns={['domain', 'target', 'tls', 'state']} grid="minmax(10rem,1fr) minmax(12rem,1.5fr) minmax(4rem,max-content) minmax(12rem,2fr)" rows={rows} total={R.length} filter={q} onFilter={setQ} placeholder="filter routes" action={<Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', 'add route', 'domain → target · tls · websocket')}>add route</Button>} footer={<span>routes the proxy serves that are not a project: internal tools, other machines · project domains live on the project</span>} />
  }
  if (slug === 'nodes') return <NodesLedger dense={dense} go={go} meta={meta} />
  if (slug === 'cluster') return <ClusterPage meta={meta} notify={notify} />
  if (slug === 'plugins') {
    const P = [['agents', 'built-in', 'loaded', 'ok', ''], ['compliance-pack', 'external · /opt/temps/plugins', 'loaded', 'ok', 'license valid to 2026-11-28'], ['hello-world', 'example', 'not installed', 'idle', 'copy the install snippet']]
    const rows: LedgerRow[] = P.map((p) => ({ id: p[0], state: p[3] as State, mobile: <><span className="block font-mono">{p[0]}</span><span className="block text-[11px] text-muted-foreground">{p[1]} · {p[2]}</span></>, cells: [<span className="font-mono">{p[0]}</span>, <span className="text-muted-foreground">{p[1]}</span>, <Status state={p[3] as State} label={p[2]} />, <span className="text-muted-foreground">{p[4]}</span>] }))
    return <Ledger title="Plugins" meta={meta} dense={dense} status={<StatusLine state="ok">Three plugins loaded, none failed. Reload after installing one; a failed plugin stops the console, not the proxy.</StatusLine>} columns={['plugin', 'source', 'state', '']} grid="minmax(8rem,1fr) minmax(10rem,1.5fr) minmax(8rem,max-content) minmax(10rem,1.5fr)" rows={rows} total={P.length} filter={q} onFilter={setQ} placeholder="filter plugins" action={<Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', 'plugins reloaded', '3 loaded · 0 failed')}><RefreshCw /> reload</Button>} />
  }

  // Form pages on the Settings template. Each field's help says when it takes effect.
  const pages: Record<string, { status: ReactNode; sections: { title: string; body: ReactNode }[]; danger: ReactNode; before?: ReactNode }> = {
    domain: {
      status: <StatusLine state="error">Certificate renewals will fail: Let's Encrypt has no contact email. Renewal for <span className="font-mono">cdn.acme.sh</span> is due in 6 days.</StatusLine>,
      sections: [
        { title: 'addresses', body: <>
          <Field label="external URL" help={eff('now', 'what links, webhooks and OAuth callbacks use')}><Input defaultValue="https://temps.acme.sh" onChange={touch} className="h-8 font-mono text-xs" /></Field>
          <Field label="internal URL" help={eff('now', 'how containers reach the console')}><Input defaultValue="http://host.docker.internal:8080" onChange={touch} className="h-8 font-mono text-xs" /></Field>
          <Field label="preview domain" help={eff('next request', 'previews are served as <branch>.<project>.<this>')}><Input defaultValue="preview.acme.sh" onChange={touch} className="h-8 font-mono text-xs" /></Field>
        </> },
        { title: 'certificates', body: <>
          <Field label="ACME contact email" help={eff('now', "required · Let's Encrypt sends expiry warnings here")}><Input placeholder="ops@acme.sh" onChange={touch} className="h-8 border-destructive font-mono text-xs" /></Field>
          <Field label="ACME environment" help={eff('now', 'staging issues untrusted certificates for testing')}><Picker value="production" onChange={touch} options={[{ value: 'production', label: 'production' }, { value: 'staging', label: 'staging' }]} className="h-8 text-xs" width="200px" /></Field>
          <Field label="edge target" help={eff('now', 'the A record every project domain should point at')}><Input defaultValue="91.107.201.10" onChange={touch} className="h-8 font-mono text-xs" /></Field>
        </> },
      ],
      danger: <div className="flex flex-wrap items-center justify-between gap-3 text-xs"><div><p className="font-medium">Refresh the route table</p><p className="text-[11px] text-muted-foreground">Rebuilds every proxy route from the database. Requests are held for about a second.</p></div><EchoDialog trigger={<Button size="sm" variant="outline" className="h-8 text-xs">refresh routes</Button>} title="Refresh routes" description="Every route is rebuilt; in-flight requests wait." confirmWord="refresh" steps={['read routes', 'swap route table']} onDone={() => notify('ok', 'routes refreshed', '41 routes')} /></div>,
    },
    updates: {
      status: <StatusLine state="ok">v0.1.0 is the latest stable. Self-update checks nightly and restarts at 04:00 when there is a release.</StatusLine>,
      sections: [
        { title: 'running', body: <div className="grid gap-2 text-xs sm:grid-cols-3"><div><p className="op-label">version</p><p className="font-mono">v0.1.0</p></div><div><p className="op-label">built</p><p className="font-mono">2026-09-01 · 3f9a2c</p></div><div><p className="op-label">last check</p><p className="font-mono">2h ago · nothing newer</p></div></div> },
        { title: 'self-update', body: <>
          <Field label="self-update" help={eff('now', 'downloads the release and restarts at the window')}><Toggle checked onChange={touch} /></Field>
          <Field label="channel" help={eff('now', 'stable · beta gets releases two weeks earlier')}><Picker value="stable" onChange={touch} options={[{ value: 'stable', label: 'stable' }, { value: 'beta', label: 'beta' }]} className="h-8 text-xs" width="200px" /></Field>
          <Field label="restart window" help={eff('now', 'local time on the node')}><Input defaultValue="04:00" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
        </> },
      ],
      danger: <div className="flex flex-wrap items-center justify-between gap-3 text-xs"><div><p className="font-medium">Update now</p><p className="text-[11px] text-muted-foreground">Nothing newer than v0.1.0 on stable. Switch to beta to see v0.2.0-beta.3.</p></div><Button size="sm" variant="outline" className="h-8 text-xs" disabled>update now</Button></div>,
    },
    builds: {
      before: <Callout state="warn" title="Restart pending: build concurrency was set to 4 two hours ago" action={restartDialog}>Builds still run 2 at a time until temps restarts. Everything else on this page applies immediately.</Callout>,
      status: <StatusLine state="warn">Build concurrency 4 is waiting for a restart; builds still run 2 at a time.</StatusLine>,
      sections: [
        { title: 'limits', body: <>
          <Field label="concurrent builds" help={eff('restart', 'more than the node has cores slows every build')}><Input defaultValue="4" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
          <Field label="cpu per build" help={eff('now', '0 = unlimited · cores')}><Input defaultValue="0" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
          <Field label="memory per build" help={eff('now', '0 = unlimited · MB')}><Input defaultValue="0" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
        </> },
        { title: 'registry', body: <>
          <Field label="push images to a registry" help={eff('now', 'off: images stay on the node that built them')}><Toggle checked={false} onChange={touch} /></Field>
          <Field label="registry URL" help={eff('now', 'e.g. ghcr.io/acme')}><Input placeholder="ghcr.io/acme" onChange={touch} className="h-8 font-mono text-xs" disabled /></Field>
        </> },
      ],
      danger: <p className="text-xs text-muted-foreground">Nothing destructive here. Turning the registry off leaves images where they are.</p>,
    },
    timeouts: {
      status: <StatusLine state="ok">No default timeouts: a request may take up to the 600s ceiling. Projects can set their own under deploy settings.</StatusLine>,
      sections: [{ title: 'defaults', body: <>
        <Field label="ceiling" help={eff('next request', 'no project can exceed this · seconds')}><Input defaultValue="600" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
        <Field label="http request" help={eff('next request', '0 = none · seconds')}><Input defaultValue="0" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
        <Field label="sse idle" help={eff('next request', '0 = none · seconds without an event')}><Input defaultValue="0" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
        <Field label="websocket idle" help={eff('next request', '0 = none · seconds without a frame')}><Input defaultValue="0" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
      </> }],
      danger: <p className="text-xs text-muted-foreground">Nothing destructive here.</p>,
    },
    signin: {
      status: <StatusLine state="warn">The console answers to any IP. Set the admin gate to your office range or a VPN; the API keys and webhooks are unaffected.</StatusLine>,
      sections: [
        { title: 'providers', body: <ol className="op-rows border bg-background text-xs"><li className="flex items-center justify-between gap-3 px-3 py-2"><span><Status state="ok" label="Google" /> <span className="text-muted-foreground">· OIDC · 5 users · default role member</span></span><a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'open Google provider', 'issuer, client, role mappings') }} className="text-muted-foreground hover:text-foreground">edit</a></li><li className="flex items-center justify-between gap-3 px-3 py-2"><span><Status state="ok" label="password" /> <span className="text-muted-foreground">· 2 users · 2FA optional</span></span><a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'add provider', 'Google · Okta · Auth0 · Keycloak · any OIDC') }} className="text-muted-foreground hover:text-foreground">add provider</a></li></ol> },
        { title: 'policy', body: <>
          <Field label="require 2FA" help={eff('now', 'password users must enrol at next sign-in')}><Toggle checked={false} onChange={touch} /></Field>
          <Field label="auto-provision SSO users" help={eff('now', 'first sign-in creates the user with the default role')}><Toggle checked onChange={touch} /></Field>
        </> },
        { title: 'admin gate', body: <>
          <Field label="allowed IPs" help={eff('now', 'CIDRs allowed to reach the console · empty = any')}><Input placeholder="10.0.0.0/8, 203.0.113.4/32" onChange={touch} className="h-8 font-mono text-xs" /></Field>
          <Field label="allowed hosts" help={eff('now', 'Host headers the console answers to')}><Input defaultValue="temps.acme.sh" onChange={touch} className="h-8 font-mono text-xs" /></Field>
          <Field label="trust X-Forwarded-For" help={eff('now', 'only behind Cloudflare or your own proxy')}><Toggle checked={false} onChange={touch} /></Field>
        </> },
      ],
      danger: <div className="flex flex-wrap items-center justify-between gap-3 text-xs"><div><p className="font-medium">Sign everyone out</p><p className="text-[11px] text-muted-foreground">Every session ends now, including yours. API keys keep working.</p></div><EchoDialog trigger={<Button size="sm" variant="outline" className="h-8 border-destructive text-xs text-destructive">sign everyone out</Button>} destructive title="Sign everyone out" description="All 6 sessions end. Type the instance host to confirm." confirmWord="temps.acme.sh" steps={['revoke sessions', 'rotate cookie secret']} onDone={() => notify('warn', 'all sessions revoked')} /></div>,
    },
    headers: {
      status: <StatusLine state="ok">Strict preset on every response: HSTS for a year, frames denied, no sniffing. Projects that embed themselves elsewhere set their own frame policy.</StatusLine>,
      sections: [{ title: 'headers', body: <>
        <Field label="preset" help={eff('next request', 'strict · balanced · off · custom edits below')}><Picker value="strict" onChange={touch} options={[{ value: 'strict', label: 'strict' }, { value: 'balanced', label: 'balanced' }, { value: 'off', label: 'off' }]} className="h-8 text-xs" width="200px" /></Field>
        <Field label="Strict-Transport-Security" help={eff('next request')}><Input defaultValue="max-age=31536000; includeSubDomains" onChange={touch} className="h-8 font-mono text-xs" /></Field>
        <Field label="X-Frame-Options" help={eff('next request')}><Input defaultValue="DENY" onChange={touch} className="h-8 w-40 font-mono text-xs" /></Field>
        <Field label="Content-Security-Policy" help={eff('next request', 'empty = not sent')}><Input placeholder="default-src 'self'" onChange={touch} className="h-8 font-mono text-xs" /></Field>
        <Field label="Referrer-Policy" help={eff('next request')}><Input defaultValue="strict-origin-when-cross-origin" onChange={touch} className="h-8 font-mono text-xs" /></Field>
      </> }],
      danger: <p className="text-xs text-muted-foreground">Nothing destructive here. "off" sends no security headers at all; the verdict will say so.</p>,
    },
    traffic: {
      status: <StatusLine state="ok">Rate limiting is off. Two IPs are blocked and one range is always allowed; 0 requests were refused in 24h.</StatusLine>,
      sections: [
        { title: 'rate limit', body: <>
          <Field label="rate limiting" help={eff('next request', 'per client IP, across every project')}><Toggle checked={false} onChange={touch} /></Field>
          <Field label="per minute" help={eff('next request', 'requests · 429 above it')}><Input defaultValue="60" onChange={touch} className="h-8 w-24 font-mono text-xs" disabled /></Field>
          <Field label="per hour" help={eff('next request')}><Input defaultValue="1000" onChange={touch} className="h-8 w-24 font-mono text-xs" disabled /></Field>
        </> },
        { title: 'ip rules', body: <ol className="op-rows border bg-background text-xs">{[['203.0.113.0/24', 'allow', 'office · never rate limited'], ['198.51.100.7', 'block', 'credential stuffing · 2026-08-30'], ['198.51.100.9', 'block', 'scraper · 2026-09-02']].map((r) => <li key={r[0]} className="flex items-center gap-3 px-3 py-2"><span className="w-36 shrink-0 font-mono">{r[0]}</span><Status state={r[1] === 'allow' ? 'ok' : 'error'} label={r[1]} /><span className="min-w-0 flex-1 truncate text-muted-foreground">{r[2]}</span><a href="#" onClick={(e) => { e.preventDefault(); notify('ok', `remove ${r[0]}`) }} className="text-muted-foreground hover:text-foreground">remove</a></li>)}<li className="px-3 py-2"><a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'add rule', 'ip or cidr · allow / block · reason') }} className="text-muted-foreground hover:text-foreground">add a rule</a></li></ol> },
      ],
      danger: <p className="text-xs text-muted-foreground">Blocking your own IP locks you out of the console too; the admin gate on Sign-in is checked first.</p>,
    },
    store: {
      status: <StatusLine state="ok">Metrics, spans and logs go to TimescaleDB on this node: 3.1 GB, scraped every 15s from 12 services. ClickHouse is the option above ~50 GB/day.</StatusLine>,
      sections: [
        { title: 'backend', body: <>
          <Field label="store" help={eff('restart', 'switching keeps old data where it is')}><Picker value="timescale" onChange={touch} options={[{ value: 'timescale', label: 'TimescaleDB (this node)' }, { value: 'clickhouse', label: 'ClickHouse' }]} className="h-8 text-xs" width="260px" /></Field>
          <Field label="ClickHouse URL" help={eff('restart', 'only when the store is ClickHouse')}><SecretValue value="https://ch.internal:8443" secret /></Field>
        </> },
        { title: 'collection', body: <>
          <Field label="scrape interval" help={eff('now', 'seconds · 12 services scraped')}><Input defaultValue="15" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
          <Field label="compress proxy logs after" help={eff('now', 'hours · compressed rows are read-only')}><Input defaultValue="24" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
        </> },
      ],
      danger: <p className="text-xs text-muted-foreground">Nothing destructive here. Deleting data is on <Phrase onClick={() => go('settings:retention')}>Retention</Phrase>, on purpose.</p>,
    },
    retention: {
      status: <StatusLine state="ok">Raw metrics 7 days, hourly 90 days, daily 2 years; logs and spans 14 days. Shortening a value deletes older rows on the next hourly pass.</StatusLine>,
      sections: [
        { title: 'metrics', body: <>
          <Field label="raw" help={eff('now', 'days · 1.9 GB now')}><Input defaultValue="7" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
          <Field label="hourly" help={eff('now', 'days · 0.4 GB')}><Input defaultValue="90" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
          <Field label="daily" help={eff('now', 'years · 0.1 GB')}><Input defaultValue="2" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
        </> },
        { title: 'logs and traces', body: <>
          <Field label="proxy logs" help={eff('now', 'days · 0.5 GB')}><Input defaultValue="14" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
          <Field label="spans" help={eff('now', 'days · 0.2 GB')}><Input defaultValue="14" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
          <Field label="application logs" help={eff('now', 'days')}><Input defaultValue="14" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
        </> },
      ],
      danger: <div className="text-xs"><p className="font-medium">Saving a shorter value deletes data.</p><p className="mt-1 text-[11px] text-muted-foreground">The save bar asks you to type "delete" when any value got shorter, and says how many days of which signal go away.</p></div>,
    },
    alerts: {
      status: <StatusLine state="ok">Alerts go to ops@acme.sh and #incidents. Disk warns at 80% (now 41%); 2 metric rules, none firing.</StatusLine>,
      sections: [
        { title: 'channels', body: <ol className="op-rows border bg-background text-xs">{[['email', 'ops@acme.sh', 'ok', 'tested 2d ago'], ['slack', '#incidents', 'ok', 'tested 2d ago'], ['webhook', '—', 'idle', 'not set']].map((c) => <li key={c[0]} className="flex items-center gap-3 px-3 py-2"><span className="w-20 shrink-0 font-medium">{c[0]}</span><span className="min-w-0 flex-1 truncate font-mono text-muted-foreground">{c[1]}</span><Status state={c[2] as State} label={c[3]} /><a href="#" onClick={(e) => { e.preventDefault(); notify('ok', `test ${c[0]}`, 'sends a real message') }} className="text-muted-foreground hover:text-foreground">test</a></li>)}</ol> },
        { title: 'host', body: <>
          <Field label="disk warning" help={eff('now', '% of any data disk · now 41%')}><Input defaultValue="80" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
          <Field label="check every" help={eff('now', 'seconds')}><Input defaultValue="300" onChange={touch} className="h-8 w-24 font-mono text-xs" /></Field>
        </> },
        { title: 'rules', body: <p className="text-xs text-muted-foreground">Metric and uptime rules live with what they watch: <Phrase onClick={() => go('metrics')}>metrics</Phrase>, <Phrase onClick={() => go('uptime')}>uptime</Phrase>, each database. This page is only where they are sent.</p> },
      ],
      danger: <p className="text-xs text-muted-foreground">Removing the last channel silences every alert; the hub says so.</p>,
    },
  }
  const pg = pages[slug] ?? pages.domain
  return (
    <div className="space-y-4">
      {/* `before` goes inside the template's status slot: a restart Callout above the page title reads as belonging to the shell, not to this page. */}
      <Settings title={page.title} meta={meta} status={pg.before ? <div className="space-y-4">{pg.status}{pg.before}</div> : pg.status} sections={pg.sections} onSave={save} dirty={dirty} danger={pg.danger} />
      <EffectLegend />
    </div>
  )
}
export function settingsSlugTitle(slug: string) { return ALL.find((p) => p.slug === slug)?.title ?? slug }
