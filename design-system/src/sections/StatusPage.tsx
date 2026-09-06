// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState } from 'react'
import { Link, useSearchParams } from 'react-router'
import { Maximize2, Minimize2, Rss } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { GLYPH, GLYPH_CLASS, StatusStrip, type State, type StatusBucket } from '@/components/op'
import { ProjectMark } from '@/components/op'
import { PROJECT_ICONS } from './console-projects'
import { PAGE_BLEED } from '@/components/shell-context'

/**
 * The public status page, one per project, published at status.<domain>.
 * It is read by customers during an incident on a phone, so it is one column,
 * the verdict first in words, then each component with 90 days of checks,
 * then incidents with their updates, then how to be told next time. No
 * console chrome, no numbers the reader cannot act on, ink and the state
 * tones only. The same vocabulary as the console (● operational · ◐ degraded
 * · × outage · ○ maintenance), but the legend under the components only lists
 * the states this page can actually show: a key to a mark nothing carries
 * makes the reader hunt for it.
 */
type Component = { name: string; group: string; state: State; uptime90: number; buckets: StatusBucket[] }
const days = (seed: number, bad: number[] = [], slow: number[] = []): StatusBucket[] => Array.from({ length: 90 }, (_, i) => {
  const d = new Date(2026, 5, 9 + i)
  const state: State = bad.includes(i) ? 'error' : slow.includes(i) ? 'warn' : 'ok'
  return { start: d.toLocaleDateString('en', { month: 'short', day: 'numeric' }), state, checks: 2880, down: state === 'error' ? 60 : 0, p50_ms: 80 + ((i * seed) % 40) }
})
const COMPONENTS: Component[] = [
  { name: 'Website', group: 'Platform', state: 'ok', uptime90: 100, buckets: days(3) },
  { name: 'API', group: 'Platform', state: 'error', uptime90: 99.94, buckets: days(5, [89], [86, 80]) },
  { name: 'Checkout', group: 'Platform', state: 'warn', uptime90: 99.97, buckets: days(7, [], [89, 77]) },
  { name: 'Dashboard', group: 'Platform', state: 'ok', uptime90: 99.99, buckets: days(2, [], [41]) },
  { name: 'Email delivery', group: 'Services', state: 'ok', uptime90: 100, buckets: days(9) },
  { name: 'Webhooks', group: 'Services', state: 'ok', uptime90: 99.98, buckets: days(4, [12]) },
]
type Update = { t: string; state: 'investigating' | 'identified' | 'monitoring' | 'resolved'; text: string }
type Incident = { id: string; title: string; state: State; affects: string[]; started: string; updates: Update[] }
const INCIDENTS: Incident[] = [
  { id: 'inc_31', title: 'API returning connection errors', state: 'error', affects: ['API', 'Checkout'], started: 'Sep 6, 20:30 UTC', updates: [
    { t: '21:02', state: 'monitoring', text: 'A fix has been deployed. Error rates are back to normal; we are watching for the next 30 minutes.' },
    { t: '20:47', state: 'identified', text: 'The deploy at 20:28 changed how the API accepts connections. We are rolling it back.' },
    { t: '20:33', state: 'investigating', text: 'We are seeing elevated connection errors on the API. Checkout is affected.' },
  ] },
  { id: 'inc_30', title: 'Slow responses on Checkout', state: 'warn', affects: ['Checkout'], started: 'Sep 3, 06:00 UTC', updates: [
    { t: '06:14', state: 'resolved', text: 'Response times recovered. A database index has been added to prevent a repeat.' },
    { t: '06:02', state: 'investigating', text: 'Checkout is responding slowly for some customers. Orders are still going through.' },
  ] },
  { id: 'inc_29', title: 'Webhook deliveries delayed', state: 'error', affects: ['Webhooks'], started: 'Jun 21, 14:10 UTC', updates: [
    { t: '15:00', state: 'resolved', text: 'All queued deliveries have been sent. No deliveries were lost.' },
    { t: '14:15', state: 'identified', text: 'The webhook worker stopped after a certificate expired. Deliveries are queued, not dropped.' },
  ] },
]
const WORD: Record<State, string> = { ok: 'operational', warn: 'degraded', error: 'outage', idle: 'maintenance', sampled: 'unknown' }
const UPDATE_STATE: Record<Update['state'], State> = { investigating: 'error', identified: 'warn', monitoring: 'warn', resolved: 'ok' }

export function StatusPage({ full = false }: { full?: boolean }) {
  const [params] = useSearchParams()
  const project = params.get('project') ?? 'acme-storefront'
  const [email, setEmail] = useState('')
  const [subscribed, setSubscribed] = useState(false)
  const worst: State = COMPONENTS.some((c) => c.state === 'error') ? 'error' : COMPONENTS.some((c) => c.state === 'warn') ? 'warn' : 'ok'
  const affected = COMPONENTS.filter((c) => c.state !== 'ok')
  const active = INCIDENTS.filter((i) => i.updates[0].state !== 'resolved')
  const past = INCIDENTS.filter((i) => i.updates[0].state === 'resolved')
  const groups = useMemo(() => [...new Set(COMPONENTS.map((c) => c.group))], [])
  // The legend is built from the states actually on the page (component states and the
  // 90-day strip), never from the full vocabulary.
  const legend = useMemo(() => (['ok', 'warn', 'error', 'idle', 'sampled'] as State[]).filter((st) => COMPONENTS.some((c) => c.state === st || c.buckets.some((b) => b.state === st))), [])
  const verdict = worst === 'ok' ? 'All systems operational.' : worst === 'warn' ? `${affected.map((c) => c.name).join(' and ')} ${affected.length > 1 ? 'are' : 'is'} degraded.` : `${affected.filter((c) => c.state === 'error').map((c) => c.name).join(' and ')} ${affected.filter((c) => c.state === 'error').length > 1 ? 'are' : 'is'} down${affected.some((c) => c.state === 'warn') ? `; ${affected.filter((c) => c.state === 'warn').map((c) => c.name).join(', ')} degraded` : ''}.`
  return (
    <div className={full ? 'operator ink v1 min-h-screen' : `operator ink v1 min-h-[calc(100vh-3rem)] ${PAGE_BLEED}`}>
      <Link to={full ? '/status-page' : '/status'} aria-label={full ? 'Exit full screen' : 'Full screen'} className="fixed bottom-4 right-4 z-40 inline-flex h-8 w-8 items-center justify-center border bg-background text-foreground hover:bg-muted [&_svg]:h-3.5 [&_svg]:w-3.5">{full ? <Minimize2 /> : <Maximize2 />}</Link>
      <header className="border-b px-4 sm:px-8">
        <div className="mx-auto flex max-w-3xl items-center justify-between gap-4 py-4">
          <span className="flex items-center gap-2 text-sm font-semibold"><ProjectMark name={project} icon={PROJECT_ICONS[project]} size={24} />{project.replace('acme-storefront', 'Acme')} status</span>
          <nav className="flex items-center gap-4 text-xs text-muted-foreground"><a href="#incidents" className="hover:text-foreground">incidents</a><a href="#subscribe" className="hover:text-foreground">subscribe</a><a href="https://acme.sh" className="hover:text-foreground">acme.sh</a></nav>
        </div>
      </header>
      <main className="mx-auto max-w-3xl px-4 py-8 sm:px-8">
        {/* Verdict: the one raised block, the first thing on a phone. */}
        <section className="op-raise border bg-background px-5 py-4" aria-live="polite">
          <p className="flex items-baseline gap-3 text-xl font-semibold leading-7 tracking-[-0.01em]"><span aria-hidden className={GLYPH_CLASS[worst]}>{GLYPH[worst]}</span>{verdict}</p>
          <p className="mt-1 text-sm text-muted-foreground">{active.length ? <>An incident is open since {active[0].started}; last update {active[0].updates[0].t} UTC. <a href="#incidents" className="underline underline-offset-4">Read the updates</a>.</> : <>Checked every 30 seconds from three regions · updated 12 seconds ago.</>}</p>
        </section>

        {groups.map((g) => (
          <section key={g} className="mt-8">
            <h2 className="flex items-baseline gap-2 text-sm font-semibold leading-6"><span>{g}</span><span className="font-mono text-[11px] text-muted-foreground">90 days · one segment per day</span></h2>
            <ol className="mt-3 divide-y divide-[var(--op-rule-soft)] border bg-background">
              {COMPONENTS.filter((c) => c.group === g).map((c) => (
                <li key={c.name} className="px-4 py-3">
                  <div className="flex items-baseline justify-between gap-3 text-sm">
                    <span className="flex items-baseline gap-2"><span aria-hidden className={GLYPH_CLASS[c.state]}>{GLYPH[c.state]}</span><span className="font-medium">{c.name}</span><span className={`text-xs ${c.state === 'ok' ? 'text-muted-foreground' : GLYPH_CLASS[c.state]}`}>{WORD[c.state]}</span></span>
                    <span className="font-mono text-xs text-muted-foreground">{c.uptime90.toFixed(2)}%</span>
                  </div>
                  <StatusStrip buckets={c.buckets} height={14} className="mt-2" />
                </li>
              ))}
            </ol>
          </section>
        ))}
        <p className="mt-2 font-mono text-[11px] text-muted-foreground">{legend.map((st) => `${GLYPH[st]} ${WORD[st]}`).join(' · ')} · hover or focus a strip and use ← → to read a day</p>

        <section id="incidents" className="mt-10">
          <h2 className="flex items-baseline gap-2 text-sm font-semibold leading-6"><span>Incidents</span><span className="font-mono text-[11px] text-muted-foreground">{active.length} open · {past.length} in the last 90 days</span></h2>
          <ol className="mt-3 space-y-6">
            {[...active, ...past].map((i) => (
              <li key={i.id} className={`border-l-2 pl-4 ${i.updates[0].state === 'resolved' ? 'border-l-border' : i.state === 'error' ? 'border-l-destructive' : 'border-l-warning'}`}>
                <p className="flex flex-wrap items-baseline gap-x-2 text-sm font-semibold leading-5"><span aria-hidden className={GLYPH_CLASS[i.updates[0].state === 'resolved' ? 'ok' : i.state]}>{GLYPH[i.updates[0].state === 'resolved' ? 'ok' : i.state]}</span>{i.title}<span className="font-mono text-[11px] font-normal text-muted-foreground">{i.started} · {i.affects.join(', ')}</span></p>
                <ol className="mt-2 space-y-2">
                  {i.updates.map((u) => (
                    <li key={u.t} className="grid grid-cols-[3.5rem_6rem_minmax(0,1fr)] gap-x-3 text-xs">
                      <span className="font-mono text-muted-foreground">{u.t}</span>
                      {/* A phase word never carries the tone alone: glyph + word, like everywhere else. */}
                      <span className={`flex items-baseline gap-1.5 font-mono ${GLYPH_CLASS[UPDATE_STATE[u.state]]}`}><span aria-hidden>{GLYPH[UPDATE_STATE[u.state]]}</span>{u.state}</span>
                      <span className="text-foreground">{u.text}</span>
                    </li>
                  ))}
                </ol>
              </li>
            ))}
          </ol>
        </section>

        <section id="subscribe" className="mt-10 border bg-background px-5 py-4">
          <h2 className="text-sm font-semibold leading-6">Be told next time</h2>
          <p className="mt-1 text-xs text-muted-foreground">One email when an incident opens and one when it resolves. No marketing, unsubscribe in one click.</p>
          {subscribed ? (
            <p className="mt-3 text-xs"><span aria-hidden className={GLYPH_CLASS.ok}>●</span> Check {email} for a confirmation link.</p>
          ) : (
            <form className="mt-3 flex flex-wrap gap-2" onSubmit={(e) => { e.preventDefault(); if (email) setSubscribed(true) }}>
              <Input type="email" required value={email} onChange={(e) => setEmail(e.target.value)} placeholder="you@example.com" className="h-8 w-64 max-w-full text-xs" aria-label="email" />
              <Button size="sm" type="submit" className="op-primary h-8 text-xs">subscribe</Button>
              <a href="/status/feed.xml" className="inline-flex h-8 items-center gap-1 px-1 text-xs text-muted-foreground hover:text-foreground"><Rss className="h-3.5 w-3.5" /> rss</a>
              <a href="/status/webhooks" className="inline-flex h-8 items-center px-1 text-xs text-muted-foreground hover:text-foreground">webhook</a>
            </form>
          )}
        </section>
      </main>
      <footer className="border-t px-4 py-4 sm:px-8"><div className="mx-auto flex max-w-3xl flex-wrap items-center justify-between gap-2 font-mono text-[11px] text-muted-foreground"><span>times in UTC · checked from fra, iad, sin</span><span>powered by temps</span></div></footer>
    </div>
  )
}
