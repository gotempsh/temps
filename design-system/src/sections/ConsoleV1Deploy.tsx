// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useState } from 'react'
import { ExternalLink, RefreshCw, RotateCcw, Square } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  Callout, Columns, Detail, EchoDialog, KeyValue, Ledger, Lede, LogLines, Metric, MetricGrid, Phrase, Section, Stages, Status, StatusLine,
  type KV, type LedgerRow, type LogLine, type Stage, type State,
} from '@/components/op'
import type { Notify } from './ConsoleV1Observe'
import { DEPS, ENVS } from './ConsoleV1Env'

/**
 * A deployment is the record people open most, and usually because something
 * is wrong. The recipe holds: the verdict says whether traffic is on it and
 * what changed since; the lede holds the facts (commit, branch, who, trigger,
 * took, replicas); the content column is the pipeline, one row per step,
 * each row saying what the step produced rather than what it is for, with
 * the failed or running step open on its log; the aside is what is serving
 * and where it came from. The full build log, the runtime log since this
 * deploy, and the post-deploy checks are facets with their own tab.
 *
 * What it refuses from the old page: eleven equal cards, each with a
 * description nobody reads ("Download source code from git repository") and
 * a gear; a preview that pushes the pipeline below the fold; and post-deploy
 * housekeeping (cron, alerts, agents, screenshot, scan, source maps) drawn
 * with the same weight as the steps that decide whether the site is up.
 */

// ── Data ─────────────────────────────────────────────────────────────
type Status_ = 'live' | 'superseded' | 'failed' | 'building' | 'cancelled'
type Deployment = {
  tag: string; project: string; env: string; status: Status_
  commit: string; message: string; author: string; branch: string; trigger: string
  at: string; took?: string; usual: string
  url: string; replicas?: string; resources: string; image?: string; node: string
  stages: Stage[]
  /** The build tool's own words on the failed step. */
  fault?: { step: string; quote: string; hint: string }
  /** Traffic since it went live, against the deployment before it. */
  since?: { prev: string; minutes: number; requests: string; errorRate: string; errorDelta: string; p95: string; p95Delta: string; state: State; issue?: string }
  supersededBy?: string
}

const L = (t: string, level: LogLine['level'], source: string, msg: string): LogLine => ({ t, level, source, msg })

const PIPELINE_OK: Stage[] = [
  { phase: 'build', name: 'download repository', state: 'ok', duration: '4s', result: 'main@9bc61c0 · 1,204 files · 38 MB', lines: [L('20:33:20', 'info', 'git', 'clone github.com/acme/api-gateway --depth 1 --branch main'), L('20:33:24', 'info', 'git', 'checked out 9bc61c0 · 1,204 files')] },
  { phase: 'build', name: 'build container image', state: 'ok', duration: '2m 01s', result: 'image 212 MB · 14 layers · 9 cached', lines: [
    L('20:33:24', 'info', 'docker', 'FROM oven/bun:1.2-slim AS deps'), L('20:33:25', 'debug', 'docker', 'layer sha256:4f2a… cached'), L('20:33:25', 'info', 'bun', 'bun install --frozen-lockfile'),
    L('20:34:06', 'info', 'bun', '412 packages installed [41.2s]'), L('20:34:06', 'info', 'next', 'next build'), L('20:35:19', 'warn', 'next', 'app/(marketing)/compare/page.tsx: image is missing "sizes"; layout shift possible'),
    L('20:35:23', 'info', 'next', '✓ compiled · 48 routes · first load JS 92 kB'), L('20:35:25', 'info', 'docker', 'exported sha256:9e21c7… · 212 MB · 14 layers'),
  ] },
  { phase: 'release', name: 'start containers', state: 'ok', duration: '16s', result: '2 of 2 replicas healthy · GET / 200 in 0.8s', lines: [
    L('20:35:25', 'info', 'deploy', 'starting api-gateway-dep_91a-1 on hetzner-1'), L('20:35:25', 'info', 'deploy', 'starting api-gateway-dep_91a-2 on hetzner-1'), L('20:35:38', 'info', 'health', 'GET / → 200 in 0.8s (replica 1)'),
    L('20:35:40', 'info', 'health', 'GET / → 200 in 0.7s (replica 2)'), L('20:35:41', 'info', 'deploy', '2 of 2 replicas healthy'),
  ] },
  { phase: 'release', name: 'persist static assets', state: 'ok', duration: '16s', result: '798 assets · 18.8 MB · stale chunks keep serving', lines: [L('20:35:41', 'info', 'assets', 'extracting from /app/.next/static'), L('20:35:56', 'info', 'assets', 'persisted 798 assets · 18,781,506 bytes')] },
  { phase: 'release', name: 'switch traffic', state: 'ok', duration: '1s', result: 'api.acme.sh → dep_91a · dep_90e drained', lines: [L('20:35:57', 'info', 'proxy', 'route api.acme.sh → dep_91a (2 upstreams)'), L('20:35:57', 'info', 'proxy', 'dep_90e drained in 0.4s · containers kept for rollback')] },
  { phase: 'after going live', name: 'cron jobs', state: 'ok', duration: '1s', result: '3 from .temps.yaml · next 02:00', lines: [L('20:35:58', 'info', 'cron', 'read .temps.yaml · 3 jobs'), L('20:35:58', 'info', 'cron', 'nightly-report  0 2 * * *   unchanged'), L('20:35:58', 'info', 'cron', 'sitemap         0 */6 * * * unchanged'), L('20:35:58', 'info', 'cron', 'cleanup-carts   30 3 * * *  updated: schedule was 0 3 * * *'), L('20:35:59', 'info', 'cron', 'next run nightly-report at 02:00')] },
  { phase: 'after going live', name: 'metric alerts', state: 'ok', duration: '1s', result: '2 rules reconciled · none changed', lines: [L('20:35:59', 'info', 'alerts', 'read .temps.yaml · 2 rules'), L('20:35:59', 'info', 'alerts', 'error-rate > 0.5% for 5m → #ops    unchanged'), L('20:35:59', 'info', 'alerts', 'p95 > 400ms for 10m → #ops         unchanged')] },
  { phase: 'after going live', name: 'agents', state: 'ok', duration: '2s', result: '1 definition synced', lines: [L('20:36:00', 'info', 'agents', 'read .temps/agents/*.yaml · 1 file'), L('20:36:01', 'info', 'agents', 'support-triage: model, tools and prompt unchanged · synced')] },
  { phase: 'after going live', name: 'screenshot', state: 'ok', duration: '9s', result: '1440 × 900 · below', lines: [L('20:36:01', 'info', 'shot', 'GET https://api.acme.sh · 1440 × 900 · chromium'), L('20:36:09', 'info', 'shot', 'load event after 1.9s · networkidle after 7.4s'), L('20:36:10', 'info', 'shot', 'saved 214 kB png')] },
  { phase: 'after going live', name: 'vulnerability scan', state: 'warn', duration: '38s', result: '2 medium · 0 high or critical', lines: [L('20:36:10', 'info', 'trivy', 'scanning sha256:9e21c7… · 14 layers'), L('20:36:31', 'info', 'trivy', 'os packages: 0 findings (debian 12.7)'), L('20:36:47', 'warn', 'trivy', 'CVE-2025-30204 medium · golang.org/x/crypto 0.31.0 → fixed in 0.35.0'), L('20:36:47', 'warn', 'trivy', 'CVE-2025-27789 medium · @babel/runtime 7.26.0 → fixed in 7.26.10'), L('20:36:48', 'info', 'trivy', '2 medium · 0 high · 0 critical · report scan_9a1')] },
  { phase: 'after going live', name: 'source maps', state: 'ok', duration: '3s', result: '14 maps · errors symbolicate', lines: [L('20:36:48', 'info', 'maps', 'found 14 .map files under /app/.next/static/chunks'), L('20:36:51', 'info', 'maps', 'stored for release 9bc61c0 · 3.1 MB · maps are not served to browsers')] },
]

const PIPELINE_FAILED: Stage[] = [
  { phase: 'build', name: 'download repository', state: 'ok', duration: '3s', result: 'develop@b09a771 · 1,204 files · 38 MB', lines: [L('20:01:02', 'info', 'git', 'clone github.com/acme/api-gateway --depth 1 --branch develop'), L('20:01:05', 'info', 'git', 'checked out b09a771')] },
  { phase: 'build', name: 'build container image', state: 'error', duration: '9s', result: 'next build exited 1 · type error in AddressForm.tsx', lines: [
    L('20:01:05', 'info', 'docker', 'FROM oven/bun:1.2-slim AS deps'), L('20:01:05', 'debug', 'docker', 'layer sha256:4f2a… cached'), L('20:01:06', 'info', 'bun', 'bun install --frozen-lockfile'), L('20:01:07', 'info', 'bun', '412 packages installed (cached)'),
    L('20:01:07', 'info', 'next', 'next build'), L('20:01:13', 'error', 'next', "src/checkout/AddressForm.tsx:88:31\n  Type error: Property 'id' does not exist on type 'Address | undefined'.\n    88 |   const key = address.id"), L('20:01:14', 'error', 'next', 'Command failed with exit code 1'),
    L('20:01:14', 'error', 'docker', 'The command \'/bin/sh -c bun run build\' returned a non-zero code: 1'),
  ] },
  { phase: 'release', name: 'start containers', state: 'idle', result: 'not reached' },
  { phase: 'release', name: 'persist static assets', state: 'idle', result: 'not reached' },
  { phase: 'release', name: 'switch traffic', state: 'idle', result: 'not reached' },
]

const PIPELINE_BUILDING: Stage[] = [
  { phase: 'build', name: 'download repository', state: 'ok', duration: '3s', result: 'develop@b7c9d21 · 1,206 files · 38 MB', lines: [L('20:29:00', 'info', 'git', 'clone github.com/acme/api-gateway --depth 1 --branch develop'), L('20:29:03', 'info', 'git', 'checked out b7c9d21')] },
  { phase: 'build', name: 'build container image', state: 'idle', duration: '48s', result: 'next build · 31 of 48 routes', lines: [
    L('20:29:03', 'info', 'docker', 'FROM oven/bun:1.2-slim AS deps'), L('20:29:04', 'info', 'bun', 'bun install --frozen-lockfile'), L('20:29:44', 'info', 'bun', '412 packages installed [40.1s]'), L('20:29:44', 'info', 'next', 'next build'),
    L('20:29:47', 'warn', 'next', 'src/legacy/stripe.ts: import.meta.env is undefined at build time'), L('20:29:51', 'info', 'next', '  compiling 31/48 routes'),
  ] },
  { phase: 'release', name: 'start containers', state: 'idle' },
  { phase: 'release', name: 'persist static assets', state: 'idle' },
  { phase: 'release', name: 'switch traffic', state: 'idle' },
  { phase: 'after going live', name: 'cron jobs', state: 'idle' },
  { phase: 'after going live', name: 'metric alerts', state: 'idle' },
  { phase: 'after going live', name: 'screenshot', state: 'idle' },
  { phase: 'after going live', name: 'vulnerability scan', state: 'idle' },
  { phase: 'after going live', name: 'source maps', state: 'idle' },
]

const BASE = { project: 'api-gateway', url: 'api.acme.sh', resources: '0.5–2 cores · 128 MB–2 GB', node: 'hetzner-1', usual: '2m 20s' }
const DEPLOYMENTS: Deployment[] = [
  { ...BASE, tag: 'dep_91a', env: 'production', status: 'live', commit: '9bc61c0', message: 'feat(checkout): new address form', author: 'maya', branch: 'main', trigger: 'push to main', at: '20:33 today', took: '2m 25s', replicas: '2 of 2', image: 'sha256:9e21c7 · 212 MB', stages: PIPELINE_OK,
    since: { prev: 'dep_90e', minutes: 41, requests: '30.8k', errorRate: '0.61', errorDelta: '+0.49pt', p95: '184', p95Delta: '−9ms', state: 'warn', issue: 'i_4821' } },
  { ...BASE, tag: 'dep_90e', env: 'production', status: 'superseded', supersededBy: 'dep_91a', commit: '4f21a8d', message: 'perf(router): cache edge lookups', author: 'maya', branch: 'main', trigger: 'push to main', at: '10:00 today', took: '2m 12s', image: 'sha256:51ac09 · 210 MB', stages: PIPELINE_OK.map((s) => ({ ...s, lines: undefined, result: s.name === 'switch traffic' ? 'api.acme.sh → dep_90e · dep_88c drained' : s.name === 'vulnerability scan' ? '2 medium · 0 high or critical' : s.result, state: s.name === 'vulnerability scan' ? 'warn' : 'ok' })) },
  { ...BASE, tag: 'dep_92e', env: 'staging', url: 'staging.api.acme.sh', status: 'failed', commit: 'b09a771', message: 'fix(checkout): address form null id (wip)', author: 'maya', branch: 'develop', trigger: 'push to develop', at: '20:01 today', took: '12s', stages: PIPELINE_FAILED,
    fault: { step: 'build container image', quote: "src/checkout/AddressForm.tsx:88:31 · Type error: Property 'id' does not exist on type 'Address | undefined'.", hint: 'Staging stayed on dep_89f. The same fix built as dep_93c fourteen minutes later.' } },
  { ...BASE, tag: 'dep_92b', env: 'staging', url: 'staging.api.acme.sh', status: 'building', commit: 'b7c9d21', message: 'fix(checkout): guard address before reading id', author: 'maya', branch: 'develop', trigger: 'push to develop', at: '20:29 today', stages: PIPELINE_BUILDING },
  { ...BASE, tag: 'dep_88c', env: 'production', status: 'cancelled', commit: 'c0ffee1', message: 'chore: bump deps', author: 'jules', branch: 'main', trigger: 'redeploy by jules', at: 'yesterday 17:12', took: '6s', stages: PIPELINE_FAILED.slice(0, 1).concat([{ phase: 'build', name: 'build container image', state: 'idle', result: 'cancelled by jules before it started' }]) },
]

const RUNTIME: LogLine[] = [
  L('20:35:38', 'info', 'replica 1', 'ready on :3000 · next 15.3 · node 22'), L('20:35:40', 'info', 'replica 2', 'ready on :3000'), L('20:36:02', 'info', 'replica 1', 'GET /checkout 200 · 142 ms'),
  L('20:38:41', 'error', 'replica 2', "TypeError: Cannot read properties of undefined (reading 'id')\n    at AddressForm (src/checkout/AddressForm.tsx:88:31)"), L('20:38:41', 'warn', 'replica 2', 'POST /api/checkout 500 · 12 ms'),
  L('20:39:05', 'error', 'replica 1', "TypeError: Cannot read properties of undefined (reading 'id')\n    at AddressForm (src/checkout/AddressForm.tsx:88:31)"), L('20:41:10', 'info', 'replica 1', 'GET / 200 · 38 ms'), L('20:41:12', 'debug', 'replica 2', 'revalidated /pricing (ISR, 60s)'),
]

const WORD: Record<Status_, string> = { live: 'live', superseded: 'superseded', failed: 'failed', building: 'building', cancelled: 'cancelled' }
const WORD_STATE: Record<Status_, State> = { live: 'ok', superseded: 'idle', failed: 'error', building: 'idle', cancelled: 'idle' }

// ── Screen ────────────────────────────────────────────────────────────
type Tab = 'overview' | 'build log' | 'runtime log' | 'checks'
const TABS = ['overview', 'build log', 'runtime log', 'checks'] as const

export function DeploymentScreen({ tag, dense, notify, go }: { tag: string; dense: boolean; notify: Notify; go: (v: string) => void }) {
  const d = DEPLOYMENTS.find((x) => x.tag === tag) ?? fromList(tag)
  const [tab, setTab] = useState<Tab>('overview')
  const [q, setQ] = useState('')
  const [live, setLive] = useState(d.status === 'building' || d.status === 'live')
  const [elapsed, setElapsed] = useState(51)
  useEffect(() => { if (d.status !== 'building') return; const id = window.setInterval(() => setElapsed((e) => e + 1), 1000); return () => window.clearInterval(id) }, [d.status])
  const running = d.stages.findIndex((s) => s.state === 'idle' && s.lines)
  const done = d.stages.filter((s) => s.state === 'ok').length

  const status = d.status === 'live'
    ? d.since && d.since.state !== 'ok'
      ? <StatusLine state="warn">Serving production since {d.at.replace(' today', '')}, but the error rate went from 0.12% to {d.since.errorRate}% after it: one new TypeError in AddressForm, 31 events. <Phrase onClick={() => go(`issue:${d.since!.issue}`)}>Open the issue</Phrase> or roll back to {d.since.prev} in about 5s.</StatusLine>
      : <StatusLine state="ok">Serving production since {d.at}. Error rate and p95 are level with {d.since?.prev}.</StatusLine>
    : d.status === 'superseded'
      ? <StatusLine state="ok">Nothing to do: replaced by <Phrase onClick={() => go(`deploy:${d.supersededBy}`)}>{d.supersededBy}</Phrase> 41 minutes ago. The image is kept; rolling back to it takes about 5s.</StatusLine>
      : d.status === 'failed'
        ? <StatusLine state="error">Failed at <Phrase onClick={() => setTab('build log')}>{d.fault?.step}</Phrase> after {d.took}: {d.fault?.quote.split(' · ')[1]} Nothing changed in {d.env}.</StatusLine>
        : d.status === 'building'
          ? <StatusLine state="idle">Building: step {running + 1} of {d.stages.length}, {d.stages[running]?.name}. {fmt(elapsed)} so far, usually {d.usual} end to end.</StatusLine>
          : <StatusLine state="ok">Nothing to do: cancelled by {d.author} before the build started. Nothing changed in {d.env}.</StatusLine>

  const facts: KV[] = [
    { k: 'commit', v: <><span className="font-mono">{d.commit}</span> {d.message}</>, copy: d.commit },
    { k: 'branch', v: d.branch, mono: true },
    { k: 'trigger', v: d.trigger },
    { k: 'by', v: d.author, mono: true },
    { k: 'took', v: d.status === 'building' ? `${fmt(elapsed)} so far` : d.took ?? '—', mono: true, state: d.status === 'failed' ? 'error' : undefined },
    ...(d.replicas ? [{ k: 'replicas', v: d.replicas, mono: true }] : []),
  ]
  const lede = (
    <Lede state={WORD_STATE[d.status]} word={WORD[d.status]} facts={facts}>
      {d.status === 'live' && <>{d.url} serves this build</>}
      {d.status === 'superseded' && <>served production from {d.at} until {d.supersededBy} took over · image kept for rollback</>}
      {d.status === 'failed' && <>never became an image · {d.url} still serves the previous deploy</>}
      {d.status === 'building' && <>started {d.at.replace(' today', '')} · {done} of {d.stages.length} steps done</>}
      {d.status === 'cancelled' && <>queued {d.at}, never built · {d.url} was untouched</>}
    </Lede>
  )

  const actions = (
    <>
      {d.status === 'building'
        ? <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs"><Square /> cancel</Button>} title={`Cancel ${d.tag}`} description="The build stops where it is. Nothing has changed in staging." confirmWord={d.tag} steps={['stop build', 'remove partial image']} onDone={() => notify('warn', `${d.tag} cancelled`)} />
        : <Button size="sm" variant="outline" className="h-7 text-xs" asChild><a href={`https://${d.url}`} target="_blank" rel="noreferrer"><ExternalLink /> visit</a></Button>}
      {d.status !== 'building' && <Button size="sm" className="op-primary h-7 text-xs" onClick={() => notify('ok', `redeploying ${d.commit}`, `${d.env} · dep_95a · same commit, fresh build`)}><RefreshCw /> redeploy</Button>}
    </>
  )

  const checkRows: LedgerRow[] = d.stages.filter((s) => s.phase === 'after going live' && s.state !== 'idle').filter((s) => s.name.toLowerCase().includes(q.trim().toLowerCase())).map((s) => ({
    id: s.name, state: s.state, onOpen: s.name === 'vulnerability scan' ? () => go('scan:scan_9a1') : undefined,
    mobile: <><span className="block">{s.name}</span><span className="block truncate text-[11px] text-muted-foreground">{s.result}</span></>,
    cells: [<Status state={s.state} label={s.name} />, <span className="truncate font-mono text-muted-foreground">{s.result}</span>, <span className="font-mono text-muted-foreground">{s.duration}</span>],
    sort: { name: s.name, duration: secs(s.duration) },
  }))

  return (
    <Detail title={d.tag} meta={`${d.project} · ${d.env}`} status={status} lede={lede} tabs={TABS} tab={tab} onTab={setTab} actions={actions}>
      {tab === 'overview' && (
        <Columns>
          <div>
            {d.fault && (
              <Callout state="error" title={`${d.fault.step} failed after ${d.took}`} quote={d.fault.quote} action={<Button size="sm" variant="outline" className="h-7 text-xs" onClick={() => notify('ok', `retrying ${d.tag}`, 'same commit, fresh build')}><RefreshCw /> retry</Button>}>
                {d.fault.hint}
              </Callout>
            )}
            <Section title="Pipeline" meta={d.status === 'building' ? `${done} of ${d.stages.length} steps · ${fmt(elapsed)} · live` : `${d.stages.length} steps · ${d.took} push to traffic`} action={<a href="#" onClick={(e) => { e.preventDefault(); setTab('build log') }} className="text-xs">full log</a>}>
              <Stages stages={d.stages} />
              <p className="mt-2 text-[11px] text-muted-foreground">Each step says what it produced. Steps after going live do not hold traffic back; a failure there is a warning on this page, not a failed deploy.</p>
            </Section>
            {d.since && (
              <Section title="Since it went live" meta={`${d.since.minutes} min · against ${d.since.prev}`}>
                <MetricGrid cols={3}>
                  <Metric label="requests" value={d.since.requests} baseline={`in ${d.since.minutes} min`} />
                  <Metric label="error rate" value={d.since.errorRate} unit="%" delta={d.since.errorDelta} baseline={`vs ${d.since.prev}`} state={d.since.state} />
                  <Metric label="p95 latency" value={d.since.p95} unit="ms" delta={d.since.p95Delta} baseline={`vs ${d.since.prev}`} />
                </MetricGrid>
              </Section>
            )}
            {(d.status === 'live' || d.status === 'superseded') && (
              <Section title="Screenshot" meta={`${d.url} · taken after going live`} action={<a href={`https://${d.url}`} target="_blank" rel="noreferrer" className="text-xs">open</a>}>
                <div className="border bg-background">
                  <p className="flex items-center gap-2 border-b px-3 py-1.5 font-mono text-[11px] text-muted-foreground"><span aria-hidden>○ ○ ○</span><span className="truncate">{d.url}</span></p>
                  {/* The sandbox has no screenshot service; the landing mock stands in so the frame has real proportions. */}
                  <div className="op-inset relative h-56 overflow-hidden sm:h-72"><iframe title={`screenshot of ${d.url}`} src="/landing" tabIndex={-1} aria-hidden className="pointer-events-none absolute left-0 top-0 h-[400%] w-[400%] origin-top-left scale-[0.25] border-0 sm:h-[200%] sm:w-[200%] sm:scale-50" /></div>
                </div>
              </Section>
            )}
          </div>
          <div>
            <Section title={d.status === 'live' ? 'Serving' : d.status === 'building' ? 'Will serve' : 'Served'} meta={d.env}>
              {/* Only what the lede does not already say: replicas, commit, branch, author and trigger live up there. */}
              <KeyValue compact rows={[
                { k: 'url', v: d.url, mono: true, copy: `https://${d.url}` },
                { k: 'resources', v: d.resources, mono: true },
                { k: 'image', v: d.image ?? (d.status === 'failed' ? 'none built' : d.status === 'building' ? 'building' : '—'), mono: true, copy: d.image?.split(' ')[0], state: d.status === 'failed' ? 'error' : undefined },
                { k: 'node', v: <Phrase onClick={() => go(`node:${d.node}`)}>{d.node}</Phrase> },
              ]} />
            </Section>
            <Section title="Source">
              <KeyValue compact rows={[
                { k: 'repository', v: <a href={`https://github.com/acme/api-gateway/commit/${d.commit}`} target="_blank" rel="noreferrer" className="underline underline-offset-4">github.com/acme/api-gateway at {d.commit}</a>, mono: true },
                { k: 'started', v: d.at, mono: true },
                ...(d.since ? [{ k: 'replaced', v: <Phrase onClick={() => go(`deploy:${d.since!.prev}`)}>{d.since.prev}</Phrase> }] : d.supersededBy ? [{ k: 'replaced by', v: <Phrase onClick={() => go(`deploy:${d.supersededBy}`)}>{d.supersededBy}</Phrase> }] : []),
              ]} />
            </Section>
            {d.status !== 'building' && (
              <Section title="Danger" meta="typed confirmation">
                <div className="flex flex-wrap gap-2">
                  {d.status === 'superseded' && <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs"><RotateCcw /> roll back to this</Button>} title={`Roll back production to ${d.tag}`} description={`${d.supersededBy} stops serving; ${d.tag}'s image starts again. About 5s. Variables are today's, not ${d.at}'s.`} confirmWord={d.tag} steps={['start dep_90e containers', 'wait for health', 'switch traffic', 'drain dep_91a']} onDone={() => { notify('ok', `production rolled back to ${d.tag}`); go('deploy:dep_90e') }} />}
                  {d.status === 'live' && d.since && <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs"><RotateCcw /> roll back to {d.since.prev}</Button>} title={`Roll back production to ${d.since.prev}`} description={`${d.tag} stops serving; ${d.since.prev}'s image starts again. About 5s. ${d.tag} is kept for inspection.`} confirmWord={d.since.prev} steps={[`start ${d.since.prev} containers`, 'wait for health', 'switch traffic', `drain ${d.tag}`]} onDone={() => { notify('ok', `production rolled back to ${d.since!.prev}`); go(`deploy:${d.since!.prev}`) }} />}
                  <EchoDialog trigger={<Button size="sm" variant="outline" className="h-7 text-xs text-destructive">delete</Button>} destructive title={`Delete ${d.tag}`} description={d.status === 'live' ? 'This deployment is serving production. Deleting it is not possible until another one takes over.' : 'The image and the build log are removed. The commit stays in git.'} confirmWord={d.tag} steps={['remove image', 'remove build log']} onDone={() => go('api-gateway')} />
                </div>
              </Section>
            )}
          </div>
        </Columns>
      )}

      {tab === 'build log' && (
        <Section title="Build log" meta={`${d.stages.filter((s) => s.lines).length} steps with output · ${d.stages.flatMap((s) => s.lines ?? []).length} lines`} action={<a href="#" onClick={(e) => { e.preventDefault(); notify('ok', 'download', `${d.tag}-build.log · ${d.stages.flatMap((s) => s.lines ?? []).length} lines`) }} className="text-xs">download</a>}>
          <LogLines lines={d.stages.flatMap((s) => (s.lines ?? []).map((l) => ({ ...l, source: s.name })))} live={d.status === 'building'} height={520} search />
        </Section>
      )}

      {tab === 'runtime log' && (
        d.status === 'live' || d.status === 'superseded'
          ? <Section title="Runtime log" meta={live ? 'both replicas · live · newest at the bottom' : 'both replicas · paused'} action={<button type="button" className="text-xs text-muted-foreground hover:text-foreground" onClick={() => setLive((l) => !l)}>{live ? 'pause' : 'resume'}</button>}>
              <LogLines lines={RUNTIME} live={live && d.status === 'live'} height={520} search />
            </Section>
          : <Section title="Runtime log" meta="nothing ran"><p className="border bg-background px-3 py-3 text-xs text-muted-foreground">{d.status === 'building' ? 'Containers start after the image is built; the log begins then.' : 'This deployment never started a container, so there is no runtime log.'}</p></Section>
      )}

      {tab === 'checks' && (
        <Ledger status={null} dense={dense}
          columns={[{ label: 'check', key: 'name' }, 'result', { label: 'took', key: 'duration', numeric: true }]} grid="minmax(10rem,1fr) minmax(0,2fr) minmax(60px,max-content)"
          rows={checkRows} total={checkRows.length} filter={q} onFilter={setQ} placeholder="filter checks"
          hint="after going live · none of these hold traffic back"
          footer={<span>{checkRows.length ? `${checkRows.filter((r) => r.state === 'warn').length} need a look` : 'run after the deploy goes live'}</span>} />
      )}
    </Detail>
  )
}

/** Tags that only exist in the deploys ledger get a record built from their row: a finished, superseded deploy with the shared pipeline. */
function fromList(tag: string): Deployment {
  const row = DEPS.find((x) => x.tag === tag)
  const env = ENVS.find((e) => e.id === row?.environment_id)
  const status: Status_ = row?.status === 'failed' ? 'failed' : row?.status === 'cancelled' ? 'cancelled' : row?.is_current ? 'live' : 'superseded'
  return {
    ...BASE, tag, env: env?.slug ?? 'production', url: env?.main_url.replace('https://', '') ?? BASE.url, status: status === 'failed' ? 'superseded' : status,
    commit: row?.commit_hash ?? '0000000', message: row?.commit_message ?? '', author: row?.commit_author ?? 'unknown', branch: row?.branch ?? 'main', trigger: `push to ${row?.branch ?? 'main'}`,
    at: row?.created_at ?? '', took: row?.duration ?? '—', replicas: status === 'live' ? '2 of 2' : undefined, image: 'sha256:a0b1c2 · 211 MB', supersededBy: status === 'superseded' ? DEPS.find((x) => x.environment_id === row?.environment_id && x.is_current)?.tag : undefined,
    stages: PIPELINE_OK.map((s) => ({ ...s, lines: undefined })),
  }
}

/** "2m 01s" / "38s" → seconds, so "2m 01s" does not sort as 2. */
function secs(d?: string) {
  if (!d) return null
  const m = /(?:(\d+)m\s*)?(?:(\d+)s)?/.exec(d.trim())
  if (!m || (!m[1] && !m[2])) return null
  return Number(m[1] ?? 0) * 60 + Number(m[2] ?? 0)
}

function fmt(s: number) { return s >= 60 ? `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s` : `${s}s` }
