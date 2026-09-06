// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { ArrowRight, Check, Eye, EyeOff, Minus, Plus, Search, Trash2, Upload } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { CalendarHeatmap, Columns, EchoDialog, Kbd, Ledger, PageState, Phrase, Picker, Section, SecretValue, Stages, Status, StatusLine, type LedgerRow, type Stage, type State } from '@/components/op'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   Redesign of three existing console surfaces, on v1, using the real API
   shapes (EnvironmentResponse, DeploymentResponse,
   EnvironmentVariableResponse from web/src/api/client/types.gen.ts).

   Feedback this answers:
   · "Promote to" was hidden in a per-row dropdown on deployments. Here it is
     the primary action of the environments tab, drawn as the path between
     environments, and a first-class button on every deploy that is ahead of
     production. The status line says when something is promotable.
   · The variables page had one global list with a pill per environment and a
     header dropdown ("Preview values for") that only changed preview values,
     so choosing staging still showed production-only variables. Here each
     environment is its own view that shows exactly what that environment
     receives. "matrix" is the cross-environment view with one column per
     environment for bulk association.
   · Search on `/`, selection with `x`, bulk add-to / remove-from environment
     with a command echo, import from .env.
   ──────────────────────────────────────────────────────────────────────── */

type Notify = (level: 'ok' | 'warn' | 'err', msg: string, detail?: string, undo?: () => void) => void

/** Case-insensitive substring match over the fields a filter box searches. */
const matches = (q: string, ...fields: (string | undefined | null)[]) => { const n = q.trim().toLowerCase(); return !n || fields.some((f) => (f ?? '').toLowerCase().includes(n)) }

// ── Data, in API shape ──────────────────────────────────────────────────

export type Env = { id: number; name: string; slug: string; branch: string | null; main_url: string; current_deployment_id: number | null; is_preview: boolean; sleeping: boolean; protected: boolean; last_activity_at: string }
export const ENVS: Env[] = [
  { id: 1, name: 'production', slug: 'production', branch: 'main', main_url: 'https://api.acme.sh', current_deployment_id: 91, is_preview: false, sleeping: false, protected: true, last_activity_at: '2m ago' },
  { id: 2, name: 'staging', slug: 'staging', branch: 'develop', main_url: 'https://staging-api.acme.sh', current_deployment_id: 93, is_preview: false, sleeping: false, protected: false, last_activity_at: '18m ago' },
  { id: 3, name: 'pr-212', slug: 'pr-212', branch: 'feat/rate-limits', main_url: 'https://pr-212-api.acme.sh', current_deployment_id: 94, is_preview: true, sleeping: true, protected: false, last_activity_at: '3h ago' },
]

type Dep = { id: number; tag: string; status: 'success' | 'building' | 'failed' | 'cancelled'; environment_id: number; commit_hash: string; commit_message: string; commit_author: string; branch: string; created_at: string; duration: string; is_current: boolean }
export const DEPS: Dep[] = [
  { id: 94, tag: 'dep_94b', status: 'success', environment_id: 3, commit_hash: '7e1c2aa', commit_message: 'feat(api): per-key rate limits', commit_author: 'jules', branch: 'feat/rate-limits', created_at: '3h ago', duration: '51s', is_current: true },
  { id: 93, tag: 'dep_93c', status: 'success', environment_id: 2, commit_hash: 'd41f9e0', commit_message: 'fix(checkout): address form null id', commit_author: 'maya', branch: 'develop', created_at: '18m ago', duration: '47s', is_current: true },
  { id: 92, tag: 'dep_92e', status: 'failed', environment_id: 2, commit_hash: 'b09a771', commit_message: 'fix(checkout): address form null id (wip)', commit_author: 'maya', branch: 'develop', created_at: '32m ago', duration: '12s', is_current: false },
  { id: 91, tag: 'dep_91a', status: 'success', environment_id: 1, commit_hash: '9bc61c0', commit_message: 'feat(checkout): new address form', commit_author: 'maya', branch: 'main', created_at: '41m ago', duration: '48s', is_current: true },
  { id: 90, tag: 'dep_90e', status: 'success', environment_id: 1, commit_hash: '4f21a8d', commit_message: 'perf(router): cache edge lookups', commit_author: 'maya', branch: 'main', created_at: '10h ago', duration: '41s', is_current: false },
  { id: 89, tag: 'dep_89f', status: 'success', environment_id: 2, commit_hash: '4f21a8d', commit_message: 'perf(router): cache edge lookups', commit_author: 'maya', branch: 'develop', created_at: '11h ago', duration: '43s', is_current: false },
  { id: 88, tag: 'dep_88c', status: 'cancelled', environment_id: 1, commit_hash: 'c0ffee1', commit_message: 'chore: bump deps', commit_author: 'jules', branch: 'main', created_at: 'yesterday', duration: '—', is_current: false },
]
const DEP_STATE: Record<Dep['status'], State> = { success: 'ok', building: 'warn', failed: 'error', cancelled: 'idle' }

type Var = { id: number; key: string; value: string; is_secret: boolean; include_in_preview: boolean; environments: number[]; updated_at: string }
const VARS0: Var[] = [
  { id: 1, key: 'DATABASE_URL', value: 'postgres://acme:••••@acme-pg:5432/acme', is_secret: true, include_in_preview: true, environments: [1, 2], updated_at: '3d ago' },
  { id: 2, key: 'REDIS_URL', value: 'redis://sessions-redis:6379/0', is_secret: false, include_in_preview: true, environments: [1, 2], updated_at: '3d ago' },
  { id: 3, key: 'STRIPE_SECRET_KEY', value: 'sk_live_51H••••••••••••', is_secret: true, include_in_preview: false, environments: [1], updated_at: '12d ago' },
  { id: 4, key: 'STRIPE_TEST_KEY', value: 'sk_test_51H••••••••••••', is_secret: true, include_in_preview: true, environments: [2], updated_at: '12d ago' },
  { id: 5, key: 'SENTRY_DSN', value: 'https://temps.acme.sh/errors/ingest/7', is_secret: false, include_in_preview: true, environments: [1, 2], updated_at: '20d ago' },
  { id: 6, key: 'LOG_LEVEL', value: 'info', is_secret: false, include_in_preview: true, environments: [1], updated_at: '41d ago' },
  { id: 7, key: 'FEATURE_RATE_LIMITS', value: 'true', is_secret: false, include_in_preview: true, environments: [2], updated_at: '3h ago' },
  { id: 8, key: 'RATE_LIMIT_PER_KEY', value: '600', is_secret: false, include_in_preview: true, environments: [2], updated_at: '3h ago' },
  { id: 9, key: 'SMTP_HOST', value: 'smtp.resend.com', is_secret: false, include_in_preview: false, environments: [1, 2], updated_at: '60d ago' },
  { id: 10, key: 'SMTP_PASSWORD', value: 're_••••••••••••', is_secret: true, include_in_preview: false, environments: [1, 2], updated_at: '60d ago' },
  { id: 11, key: 'PUBLIC_URL', value: 'https://api.acme.sh', is_secret: false, include_in_preview: false, environments: [1], updated_at: '90d ago' },
  { id: 12, key: 'OTEL_EXPORTER_OTLP_ENDPOINT', value: 'http://temps:4318', is_secret: false, include_in_preview: true, environments: [1, 2], updated_at: '90d ago' },
  { id: 13, key: 'CHECKOUT_ADDRESS_V2', value: 'true', is_secret: false, include_in_preview: true, environments: [2], updated_at: '18m ago' },
  { id: 14, key: 'MAX_UPLOAD_MB', value: '25', is_secret: false, include_in_preview: true, environments: [1, 2], updated_at: '120d ago' },
]

const envName = (id: number) => ENVS.find((e) => e.id === id)?.name ?? String(id)

// ── Promote: one dialog, used from three places ─────────────────────────

function PromoteDialog({ dep, to, trigger, notify, onDone }: { dep: Dep; to: Env; trigger: ReactNode; notify: Notify; onDone?: () => void }) {
  return (
    <EchoDialog
      trigger={trigger}
      echo={`$ temps deploy promote ${dep.tag} --to ${to.slug}`}
      title={`Promote to ${to.name}`}
      description={`Reuses the image built for ${dep.tag} (${dep.commit_hash} · ${dep.commit_message}). No rebuild. ${to.name} gets ${to.name === 'production' ? 'production' : to.name} variables, health check /healthz, then routes switch. About 20 seconds, no downtime.`}
      confirmWord={to.slug}
      steps={[`verify ${dep.tag} image present`, `render ${to.name} variables`, `start containers in ${to.name}`, 'health check /healthz', 'switch proxy routes', 'mark as current']}
      onDone={() => { notify('ok', `${dep.tag} promoted to ${to.name}`, `${dep.commit_hash} · ${dep.commit_message}`); onDone?.() }}
    />
  )
}

/** Which deploy is ahead of production and promotable. */
function promotable(deps: Dep[]) {
  const prod = ENVS[0]
  const stg = ENVS[1]
  const prodCur = deps.find((d) => d.environment_id === prod.id && d.is_current)
  const stgCur = deps.find((d) => d.environment_id === stg.id && d.is_current)
  if (!stgCur || !prodCur || stgCur.commit_hash === prodCur.commit_hash) return null
  const ahead = deps.filter((d) => d.environment_id === stg.id && d.status === 'success' && d.id > prodCur.id).length
  return { from: stg, to: prod, dep: stgCur, prodCur, ahead }
}

// ── Environments tab ───────────────────────────────────────────────────

export function EnvironmentsTab({ notify, dense }: { notify: Notify; dense: boolean }) {
  const [deps, setDeps] = useState(DEPS)
  const [q, setQ] = useState('')
  const p = promotable(deps)
  const promote = () => {
    if (!p) return
    setDeps((prev) => prev.map((d) => d.environment_id === 1 ? { ...d, is_current: false } : d).concat([{ ...p.dep, id: 95, tag: 'dep_95a', environment_id: 1, branch: 'main', created_at: 'now', is_current: true }]))
  }
  const envList = ENVS.filter((e) => matches(q, e.name, e.slug, e.branch, e.main_url))
  const rows: LedgerRow[] = envList.map((e) => {
    const cur = deps.find((d) => d.environment_id === e.id && d.is_current)
    const state: State = e.sleeping ? 'idle' : 'ok'
    return {
      id: e.slug,
      state,
      // The state cell says the state (running, sleeping). The branch and the protection are facts of
      // their own and get their own cells: a glyph coloured "protected" says nothing about whether it runs.
      mobile: <><span className="block truncate font-medium">{e.name}{e.is_preview && <span className="ml-2 border px-1 font-mono text-[10px] font-normal text-muted-foreground">preview</span>}</span><span className="block truncate text-[11px] text-muted-foreground">{cur?.tag} · {cur?.commit_hash} · {e.branch}</span></>,
      cells: [
        <span className="min-w-0 truncate font-medium">{e.name}{e.is_preview && <span className="ml-2 border px-1 font-mono text-[10px] font-normal text-muted-foreground">preview</span>}</span>,
        <span className="truncate font-mono text-muted-foreground">{e.branch ?? '—'}</span>,
        <span className="truncate"><span className="font-mono">{cur?.tag ?? '—'}</span> <span className="text-muted-foreground">{cur?.commit_hash} · {cur?.commit_message}</span></span>,
        <a href={e.main_url} target="_blank" rel="noreferrer" className="truncate font-mono text-muted-foreground underline underline-offset-4 hover:text-foreground">{e.main_url.replace('https://', '')}</a>,
        <span className="text-muted-foreground">{e.protected ? 'protected' : '—'}</span>,
        <span className="text-muted-foreground">{e.last_activity_at}</span>,
        <Status state={state} label={e.sleeping ? 'sleeping' : 'running'} />,
      ],
    }
  })
  return (
    <div className="space-y-6">
      <StatusLine state={p ? 'warn' : 'ok'}>
        {p ? <>{p.dep.tag} is ready to <PromoteDialog dep={p.dep} to={p.to} notify={notify} onDone={promote} trigger={<button type="button" className="underline underline-offset-4 hover:text-foreground">promote to production</button>} />.</> : <>Production and staging are in sync.</>}
      </StatusLine>

      {/* The promotion path, drawn. This is the main action of the page. */}
      <Section title="Promotion path" meta="staging → production" >
      <div className="grid gap-px border bg-border md:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)]">
        {[ENVS[1], null, ENVS[0]].map((e) => e ? (
          <div key={e.id} className="min-w-0 bg-background p-4">
            <div className="flex items-center justify-between gap-2">
              <span className="op-label min-w-0 truncate">{e.name}{e.protected && <span className="ml-2 border px-1 font-mono text-[10px] normal-case tracking-normal">protected</span>}</span>
              <Status state={e.sleeping ? 'idle' : 'ok'} label={e.sleeping ? 'sleeping' : 'running'} />
            </div>
            {(() => { const cur = deps.find((d) => d.environment_id === e.id && d.is_current); return cur ? (
              <div className="mt-2 text-xs">
                <p className="truncate font-mono text-base">{cur.tag} <span className="text-muted-foreground">{cur.commit_hash}</span></p>
                <p className="truncate">{cur.commit_message}</p>
                <p className="truncate text-muted-foreground">{cur.commit_author} · {cur.created_at} · auto-deploys from <span className="font-mono">{e.branch}</span></p>
                <a href={e.main_url} target="_blank" rel="noreferrer" className="mt-1 block truncate font-mono text-[11px] underline underline-offset-4 hover:text-foreground">{e.main_url.replace('https://', '')}</a>
              </div>
            ) : null })()}
          </div>
        ) : (
          <div key="arrow" className="flex flex-col items-center justify-center gap-2 bg-background px-4 py-4">
            {p ? (
              <PromoteDialog dep={p.dep} to={p.to} notify={notify} onDone={promote} trigger={<Button size="sm" className="op-primary h-8 text-xs">promote <ArrowRight /></Button>} />
            ) : (
              <span className="inline-flex items-center gap-1 text-xs text-muted-foreground"><Check className="h-3.5 w-3.5" /> in sync</span>
            )}
            <span className="text-center font-mono text-[10px] text-muted-foreground">{p ? `${p.dep.commit_hash} → production` : 'nothing to promote'}</span>
          </div>
        ))}
      </div>
      </Section>

      {/* Every environment, preview ones included, on the ledger like any other list of records. */}
      <Section title="Environments" meta={`${ENVS.length} · ${ENVS.filter((e) => e.is_preview).length} preview · promotion reuses the built image, only variables change`}>
        <Ledger
          status={null}
          columns={['environment', 'branch', 'current deploy', 'url', 'protection', 'activity', 'state']}
          grid="minmax(7rem,max-content) minmax(5rem,max-content) minmax(0,1.6fr) minmax(0,1fr) minmax(5rem,max-content) minmax(4.5rem,max-content) minmax(6rem,max-content)"
          rows={rows} total={ENVS.length} filter={q} onFilter={setQ} placeholder="filter environments" dense={dense}
          footer={<>{rows.length} of {ENVS.length} · <Kbd keys="j" className="mx-1" /> down · <Kbd keys="k" className="mx-1" /> up · <Kbd keys="/" className="mx-1" /> filter</>}
        />
      </Section>
    </div>
  )
}

// ── Deploys tab with promote as a first-class action ───────────────────


/** The build that is running now (DeploymentJobResponse per stage) and 12 weeks of deploys per day. */
const BUILD_STAGES: Stage[] = [
  { name: 'clone', state: 'ok', duration: '3s' },
  { name: 'install', state: 'ok', duration: '41s', lines: [{ t: '20:29:02', level: 'info', source: 'bun', msg: 'bun install --frozen-lockfile' }, { t: '20:29:43', level: 'info', source: 'bun', msg: '412 packages installed [41.2s]' }] },
  { name: 'build', state: 'idle', lines: [{ t: '20:29:44', level: 'info', source: 'bun', msg: 'bun build ./src/index.ts --target node' }, { t: '20:29:47', level: 'warn', source: 'bun', msg: 'src/legacy/stripe.ts: import.meta.env is undefined at build time' }, { t: '20:29:48', level: 'info', source: 'bun', msg: '  312 modules · 1.9 MB' }] },
  { name: 'image', state: 'idle' },
  { name: 'deploy', state: 'idle' },
]
const ACTIVITY = Array.from({ length: 12 * 7 }, (_, i) => ({ date: `2026-0${6 + Math.floor(i / 30)}-${String(1 + (i % 30)).padStart(2, '0')}`, count: (i % 7 === 5 || i % 7 === 6) ? (i % 4 === 0 ? 1 : 0) : Math.floor(Math.abs(Math.sin(i / 2.7)) * 7) }))

export function DeploysTab({ notify, dense, go }: { notify: Notify; dense: boolean; go: (v: string) => void }) {
  const [q, setQ] = useState('')
  const [env, setEnv] = useState<number | 'all'>('all')
  const [deps, setDeps] = useState(DEPS)
  const p = promotable(deps)
  const list = deps.filter((d) => (env === 'all' || d.environment_id === env) && matches(q, d.tag, d.commit_message, d.commit_hash, d.commit_author))
  const prodCur = deps.find((d) => d.environment_id === 1 && d.is_current)
  const rows: LedgerRow[] = list.map((d) => {
    const canPromote = d.status === 'success' && d.environment_id !== 1 && prodCur && d.commit_hash !== prodCur.commit_hash
    return {
      id: d.tag, state: DEP_STATE[d.status], onOpen: () => go(`deploy:${d.tag}`),
      mobile: <><span className="block font-mono">{d.tag} · {envName(d.environment_id)}</span><span className="block truncate text-[11px] text-muted-foreground">{d.commit_message}</span>{canPromote && <PromoteDialog dep={d} to={ENVS[0]} notify={notify} onDone={() => setDeps((prev) => prev.map((x) => x.environment_id === 1 ? { ...x, is_current: false } : x).concat([{ ...d, id: 95, tag: 'dep_95a', environment_id: 1, branch: 'main', created_at: 'now', is_current: true }]))} trigger={<Button size="sm" variant="outline" className="mt-1 h-6 px-2 text-[11px]" onClick={(e) => e.stopPropagation()}>promote to production <ArrowRight /></Button>} />}</>,
      cells: [
        <span className="font-mono"><Status state={DEP_STATE[d.status]} label={d.tag} />{d.is_current && <span className="ml-2 border px-1 text-[10px] text-muted-foreground">current</span>}</span>,
        <span>{envName(d.environment_id)}</span>,
        <span className="truncate"><span className="font-mono text-muted-foreground">{d.commit_hash}</span> {d.commit_message} <span className="text-muted-foreground">· {d.commit_author}{d.duration !== '—' && ` · ${d.duration}`}</span></span>,
        <span className="text-muted-foreground">{d.created_at}</span>,
        canPromote ? (
          <PromoteDialog dep={d} to={ENVS[0]} notify={notify} onDone={() => setDeps((prev) => prev.map((x) => x.environment_id === 1 ? { ...x, is_current: false } : x).concat([{ ...d, id: 95, tag: 'dep_95a', environment_id: 1, branch: 'main', created_at: 'now', is_current: true }]))}
            trigger={<Button size="sm" variant="outline" className="h-6 px-2 text-[11px]">promote to production <ArrowRight /></Button>} />
        ) : d.environment_id === 1 && !d.is_current && d.status === 'success' ? (
          <Button size="sm" variant="outline" className="h-6 px-2 text-[11px]" onClick={() => notify('ok', `rolling back to ${d.tag}`)}>roll back to this</Button>
        ) : <span />,
      ],
    }
  })
  return (
    <div className="space-y-6">
    <Columns>
      <div><Section title="Building now" meta="dep_92b · staging · b7c9d21 · started 20:29" action={<a href="#" onClick={(e) => { e.preventDefault(); go('deploy:dep_92b') }} className="text-xs">open</a>}><Stages stages={BUILD_STAGES} /></Section></div>
      <div><Section title="Activity" meta="12 weeks · deploys per day"><div className="border bg-background p-3"><CalendarHeatmap days={ACTIVITY} /></div></Section></div>
    </Columns>
    <Ledger
      status={
        <StatusLine state={p ? 'warn' : 'ok'} more={deps.some((d) => d.status === 'failed') ? { label: '+1 failed', items: deps.filter((d) => d.status === 'failed').map((d) => ({ state: 'error' as State, children: <><Phrase onClick={() => setEnv(2)}>{d.tag}</Phrase> failed to build on {ENVS.find((e) => e.id === d.environment_id)?.slug} after {d.duration}, {d.created_at}.</> })) } : undefined}>
          {p ? <><Phrase onClick={() => setEnv(2)}>{p.dep.tag}</Phrase> is ready to promote to production.</> : <>Production is on {prodCur?.tag}, same as staging.</>}
        </StatusLine>
      }
      columns={['deploy', 'environment', 'commit', 'when', '']} grid="minmax(9rem,max-content) minmax(6rem,max-content) minmax(0,1.8fr) minmax(4.5rem,max-content) minmax(0,max-content)"
      rows={rows} total={deps.length} filter={q} onFilter={setQ} placeholder="filter by tag, commit or message" dense={dense}
      action={
        <label className="flex min-w-0 items-center gap-2 text-xs"><span className="shrink-0 text-muted-foreground">in</span>
          <Picker skin="operator ink v1" value={String(env)} onChange={(v) => setEnv(v === 'all' ? 'all' : (Number(v) as number))} placeholder="environment" searchPlaceholder="environment…"
            options={[
              { value: 'all', label: 'all environments', group: 'scope', meta: `${deps.length} deploys` },
              ...ENVS.map((e) => ({ value: String(e.id), label: e.name, group: 'environments', meta: `${deps.filter((d) => d.environment_id === e.id).length} deploys`, state: (e.sleeping ? 'idle' : 'ok') as State })),
            ]} />
        </label>
      }
    />
    </div>
  )
}

// ── Variables tab: one view per environment, plus a matrix ─────────────

type VarView = 'matrix' | number
export function VariablesTab({ notify, dense }: { notify: Notify; dense: boolean }) {
  const [vars, setVars] = useState(VARS0)
  const [view, setView] = useState<VarView>(1)
  const [q, setQ] = useState('')
  const [sel, setSel] = useState<Set<number>>(new Set())
  const [cursor, setCursor] = useState(0)
  const [reveal, setReveal] = useState<Set<number>>(new Set())
  const [showAll, setShowAll] = useState(false)
  const [loading, setLoading] = useState(false)

  const list = useMemo(() => vars.filter((v) => (view === 'matrix' || v.environments.includes(view) || (view === 3 && v.include_in_preview)) && v.key.toLowerCase().includes(q.toLowerCase())).sort((a, b) => a.key.localeCompare(b.key)), [vars, view, q])
  const missingInProd = vars.filter((v) => v.environments.includes(2) && !v.environments.includes(1) && !v.key.includes('TEST'))

  const listRef = useRef<HTMLDivElement>(null)
  const toggleSel = (id: number) => setSel((s) => { const n = new Set(s); if (n.has(id)) n.delete(id); else n.add(id); return n })
  /* The cursor IS the focus (handoff §9): j, k and the arrows move DOM focus to the row they mark, so
     ⏎ and x always act on the row the bar marks. Hover never moves it — the pointer is not a selection. */
  const focusRow = (i: number) => {
    setCursor(i)
    listRef.current?.querySelectorAll<HTMLElement>('.op-row[tabindex]')[i]?.focus()
  }
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName
      if (tag === 'INPUT' || tag === 'TEXTAREA' || e.metaKey || e.ctrlKey) return
      // vim/Gmail/GitHub convention: j is down, k is up. Arrow keys do the same for everyone else.
      if (e.key === 'j' || e.key === 'ArrowDown') { e.preventDefault(); focusRow(Math.min(list.length - 1, cursor + 1)) }
      else if (e.key === 'k' || e.key === 'ArrowUp') { e.preventDefault(); focusRow(Math.max(0, cursor - 1)) }
      else if (e.key === 'x' && list[cursor]) { e.preventDefault(); toggleSel(list[cursor].id) }
      else if (e.key === 'Enter' && e.target === document.body && list[cursor]) toggleSel(list[cursor].id)
      else if (e.key === 'a' && e.shiftKey) setSel(new Set(list.map((v) => v.id)))
      else if (e.key === 'Escape') setSel(new Set())
      else if (e.key === '/') { e.preventDefault(); document.getElementById('var-filter')?.focus() }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [list, cursor])
  // A shorter list must not leave the cursor past its end.
  useEffect(() => { setCursor((c) => Math.min(c, Math.max(0, list.length - 1))) }, [list.length])

  // A scope change is a new list: the cursor and the selection both belong to the old one. Otherwise the sticky bulk bar acts on rows that are no longer on screen.
  useEffect(() => { setLoading(true); const t = window.setTimeout(() => setLoading(false), 250); setCursor(0); setSel(new Set()); return () => window.clearTimeout(t) }, [view])

  /* Association is reversible, so it happens on the click and the toast carries the undo. The typed
     confirmation (EchoDialog) is kept for what cannot be undone: deleting the variables. */
  const bulk = (envId: number, add: boolean) => {
    const before = vars
    const n = sel.size
    setVars((prev) => prev.map((v) => sel.has(v.id) ? { ...v, environments: add ? Array.from(new Set([...v.environments, envId])) : v.environments.filter((e) => e !== envId), updated_at: 'now' } : v))
    notify('ok', `${n} variable${n > 1 ? 's' : ''} ${add ? 'added to' : 'removed from'} ${envName(envId)}`, `${Array.from(sel).map((id) => vars.find((v) => v.id === id)?.key).join(', ')} · takes effect on the next deploy`, () => setVars(before))
    setSel(new Set())
  }
  /** One matrix cell: the same association, one variable at a time, with the same undo. */
  const toggleCell = (v: Var, envId: number, on: boolean) => {
    const before = vars
    setVars((prev) => prev.map((x) => x.id === v.id ? { ...x, environments: on ? x.environments.filter((id) => id !== envId) : [...x.environments, envId], updated_at: 'now' } : x))
    notify('ok', `${v.key} ${on ? 'removed from' : 'added to'} ${envName(envId)}`, `takes effect on the next deploy of ${envName(envId)}`, () => setVars(before))
  }
  const selKeys = Array.from(sel).map((id) => vars.find((v) => v.id === id)?.key ?? '')
  const isSecretShown = (v: Var) => showAll || reveal.has(v.id) || !v.is_secret
  const cols = view === 'matrix' ? '24px 1.6fr 2fr 100px 100px 100px 90px' : '24px 1.6fr 2.4fr 110px 90px'

  return (
    <div className="space-y-6">
      <StatusLine state={missingInProd.length ? 'warn' : 'ok'}>
        {missingInProd.length ? <><Phrase onClick={() => { setView('matrix'); setSel(new Set(missingInProd.map((v) => v.id))) }}>{missingInProd.length} variables</Phrase> are in staging but not production.</> : <>Production and staging receive the same variables.</>}
      </StatusLine>

      {/* The environment is a scope, not a view, so it is a Picker ("in production"), not a second row of tabs under the page's tabs.
          One row of tabs per page; scopes are pickers; 2–4 views of one list are a Segmented in the toolbar (handoff §7, "one axis per control"). */}
      <div className="flex flex-wrap items-center gap-2">
        <label className="flex items-center gap-2 text-xs"><span className="text-muted-foreground">in</span>
          <Picker skin="operator ink v1" value={String(view)} onChange={(v) => setView(v === 'matrix' ? 'matrix' : (Number(v) as VarView))} placeholder="environment" searchPlaceholder="environment…"
            options={[
              ...ENVS.map((e) => ({ value: String(e.id), label: e.name, group: 'environments', meta: `${vars.filter((v) => v.environments.includes(e.id) || (e.is_preview && v.include_in_preview)).length} vars`, state: (e.is_preview ? 'idle' : 'ok') as State })),
              { value: 'matrix', label: 'all environments · matrix', group: 'compare', meta: `${vars.length} vars`, state: (missingInProd.length ? 'warn' : 'ok') as State },
            ]} />
        </label>
        <div className="relative w-full sm:w-56">
          <Search className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input id="var-filter" value={q} onChange={(e) => { setQ(e.target.value); setCursor(0) }} placeholder="search keys" aria-label="Search keys" className="h-8 pl-7 pr-8 text-xs" />
          <Kbd keys="/" className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 opacity-60" />
        </div>
        <div className="flex w-full flex-wrap gap-2 sm:ml-auto sm:w-auto">
          <Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => setShowAll((s) => !s)} aria-pressed={showAll}>{showAll ? <EyeOff /> : <Eye />} {showAll ? 'hide values' : 'show values'}</Button>
          <Button size="sm" variant="outline" className="h-8 text-xs" onClick={() => notify('ok', 'import from .env', 'paste or drop a file; keys that exist are updated')}><Upload /> import .env</Button>
          <Button size="sm" className="op-primary h-8 text-xs" onClick={() => notify('ok', 'add variable', `key and value, then the environments it belongs to · currently in ${view === 'matrix' ? 'the matrix' : envName(view)}`)}><Plus /> add variable</Button>
        </div>
      </div>

      {typeof view === 'number' && (
        <p className="text-xs text-muted-foreground">
          Exactly what <span className="font-medium text-foreground">{envName(view)}</span> receives{view === 3 && ': its own variables plus every variable marked "include in preview"'}. Pick <button type="button" className="underline underline-offset-4" onClick={() => setView('matrix')}>all environments</button> to compare environments or change associations in bulk.
        </p>
      )}

      {loading ? <PageState state="loading" rows={6} /> : list.length === 0 ? (
        <PageState state="empty" title={q ? `No key matches "${q}"` : `${envName(view as number)} has no variables`} reason={q ? 'Search matches the key only. Values are never searched.' : 'Add one, import a .env file, or associate existing variables from the matrix view.'} next={<Button size="sm" className="op-primary h-8 text-xs" onClick={() => setView('matrix')}>open matrix</Button>} />
      ) : (
        <div ref={listRef} className="op-rows border" role="listbox" aria-multiselectable="true">
          <div className="op-row op-cols hidden items-center md:grid" style={{ '--cols': cols } as React.CSSProperties}>
            <span />
            <span className="op-label">key</span>
            <span className="op-label">value</span>
            {view === 'matrix' ? ENVS.map((e) => <span key={e.id} className="op-label">{e.name}</span>) : <span className="op-label">{view === 3 ? 'source' : 'also in'}</span>}
            <span className="op-label">updated</span>
          </div>
          {list.map((v, i) => (
            <div key={v.id} role="option" aria-selected={sel.has(v.id)}
              tabIndex={i === cursor ? 0 : -1}
              onFocus={() => setCursor(i)}
              onKeyDown={(e) => { if (e.key === 'Enter' && e.target === e.currentTarget) { e.preventDefault(); toggleSel(v.id) } }}
              className={cn('op-row op-cols relative grid grid-cols-[24px_1fr] items-center gap-x-3 text-xs outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring', i === cursor && 'bg-muted', sel.has(v.id) && 'op-marker-hot', !dense && 'py-1 md:py-0')} style={{ '--cols': cols } as React.CSSProperties}>
              {i === cursor && <span aria-hidden className="absolute left-0 top-0 h-full w-0.5 bg-foreground" />}
              <button type="button" aria-label={sel.has(v.id) ? 'deselect' : 'select'} onClick={() => toggleSel(v.id)} className={cn('flex h-4 w-4 items-center justify-center border', sel.has(v.id) && 'bg-foreground text-background')}>{sel.has(v.id) && <Check className="h-3 w-3" />}</button>
              <span className="min-w-0 font-mono font-medium">{v.key}{v.is_secret && <span className="ml-2 border px-1 text-[10px] font-normal text-muted-foreground">secret</span>}
                <span className="mt-1 flex flex-wrap gap-1 font-sans text-[11px] font-normal text-muted-foreground md:hidden">
                  {view === 'matrix' ? ENVS.map((e) => { const on = v.environments.includes(e.id) || (e.is_preview && v.include_in_preview); return <button key={e.id} type="button" aria-pressed={on} onClick={(ev) => { ev.stopPropagation(); if (!(e.is_preview && !v.environments.includes(e.id))) toggleCell(v, e.id, on) }} className={cn('border px-1.5 font-mono', on ? 'bg-foreground text-background' : '')}>{on ? '✓ ' : '– '}{e.name}</button> })
                    : view === 3 ? (v.environments.includes(3) ? 'set on pr-212' : 'include in preview') : (v.environments.filter((id) => id !== view).map(envName).join(', ') || <Status state="warn" label="only here" />)}
                </span>
              </span>
              <SecretValue className="hidden md:flex" value={v.value} secret={v.is_secret} revealed={isSecretShown(v)} onToggle={() => setReveal((r) => { const n = new Set(r); if (n.has(v.id)) n.delete(v.id); else n.add(v.id); return n })} />
              {view === 'matrix' ? ENVS.map((e) => {
                const on = v.environments.includes(e.id) || (e.is_preview && v.include_in_preview)
                const inherited = e.is_preview && !v.environments.includes(e.id) && v.include_in_preview
                return (
                  <button key={e.id} type="button" aria-pressed={on} title={inherited ? 'via include in preview' : on ? `remove from ${e.name}` : `add to ${e.name}`}
                    onClick={() => !inherited && toggleCell(v, e.id, on)}
                    className={cn('hidden h-6 w-16 items-center justify-center border font-mono text-[11px] md:inline-flex', on ? (inherited ? 'text-muted-foreground' : 'bg-foreground text-background') : 'text-muted-foreground hover:bg-muted')}>
                    {on ? (inherited ? '✓ preview' : '✓') : '–'}
                  </button>
                )
              }) : (
                <span className="hidden truncate text-muted-foreground md:block">
                  {view === 3 ? (v.environments.includes(3) ? 'set on pr-212' : 'include in preview') : v.environments.filter((id) => id !== view).map(envName).join(', ') || <Status state="warn" label="only here" />}
                </span>
              )}
              <span className="hidden text-muted-foreground md:block">{v.updated_at}</span>
            </div>
          ))}
          <div className="op-row flex flex-wrap items-center gap-x-1 gap-y-1 text-[11px] text-muted-foreground">
            {list.length} of {vars.length} · <Kbd keys="j" className="mx-1" /> down · <Kbd keys="k" className="mx-1" /> up · <Kbd keys="x" className="mx-1" /> or <Kbd keys="⏎" className="mx-1" /> select · <Kbd keys={['⇧', 'A']} className="mx-1" /> all · <Kbd keys="/" className="mx-1" /> search
          </div>
        </div>
      )}

      {/* Bulk bar. Appears with a selection; the verbs are the associations. */}
      {sel.size > 0 && (
        <div className="op-sticky-bottom -mx-4 flex flex-wrap items-center gap-2 border-t bg-background px-4 py-2 text-xs sm:-mx-6 sm:px-6">
          <span className="font-medium">{sel.size} selected</span>
          {/* Inside an environment view the selection is by definition in this environment, so the only
              questions are "also somewhere else?" and "not here any more?". The matrix view, being the
              cross-environment view, shows one control per environment stating where the selection IS:
              all (checked), some (dash + count), none (empty); click completes the set. */}
          {view !== 'matrix' && view !== 3 && (
            <Button size="sm" variant="outline" className="h-7 text-xs" title={`${envName(view)} stops receiving them on its next deploy; the variables keep existing for other environments`} onClick={() => bulk(view, false)}>remove from {envName(view)}</Button>
          )}
          {view === 'matrix' && <span className="ml-2 text-muted-foreground">in</span>}
          {ENVS.filter((e) => !e.is_preview && (view === 'matrix' || e.id !== view)).map((e) => {
            const inEnv = Array.from(sel).filter((id) => vars.find((v) => v.id === id)?.environments.includes(e.id)).length
            const all = inEnv === sel.size, none = inEnv === 0
            const missing = sel.size - inEnv
            // Reversible either way, so it is one click and an undo in the toast — the same consequence
            // the matrix cells have. Nothing here needs a slug typed; only the delete does.
            return view === 'matrix' ? (
              <Button key={`m-${e.id}`} size="sm" variant="outline" className={cn('h-7 gap-1.5 text-xs', all && 'op-fill-ink')} title={all ? `all ${sel.size} are in ${e.name} · click to remove them` : none ? `none are in ${e.name} · click to add them` : `${inEnv} of ${sel.size} are in ${e.name} · click to add the other ${missing}`} aria-pressed={all} onClick={() => bulk(e.id, !all)}>
                <span aria-hidden className={cn('flex h-3.5 w-3.5 items-center justify-center border', all ? 'border-background' : 'border-current')}>{all ? <Check className="h-3 w-3" /> : !none && <Minus className="h-3 w-3" />}</span>
                {e.name}{!all && !none && <span className="font-mono text-[10px] opacity-70">{inEnv}/{sel.size}</span>}
              </Button>
            ) : (
              // In an environment view the verb is explicit and the target is the other environment.
              <Button key={`add-${e.id}`} size="sm" variant="outline" className="h-7 text-xs" title={all ? `all ${sel.size} are already in ${e.name}` : `${missing} of ${sel.size} not yet in ${e.name} · they are added on its next deploy`} disabled={all} onClick={() => bulk(e.id, true)}>{all ? `already in ${e.name}` : `also add to ${e.name}`}</Button>
            )
          })}
          <EchoDialog destructive trigger={<Button size="sm" variant="outline" className="ml-auto h-7 border-destructive text-xs text-destructive"><Trash2 /> delete {sel.size}</Button>} echo={`$ temps env unset ${selKeys.join(' ')}`} title="Delete variables" description={`Deletes ${sel.size} variable${sel.size > 1 ? 's' : ''} from every environment. Running deploys keep their rendered values until the next deploy.`} confirmWord="delete" steps={['delete variables', 'render environments', 'mark environments for redeploy']} onDone={() => { setVars((prev) => prev.filter((v) => !sel.has(v.id))); notify('warn', `${sel.size} variables deleted`); setSel(new Set()) }} />
          <button type="button" className="text-muted-foreground underline underline-offset-4" onClick={() => setSel(new Set())}>clear <Kbd keys="esc" className="ml-1" /></button>
        </div>
      )}
    </div>
  )
}
