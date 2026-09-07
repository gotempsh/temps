// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState } from 'react'
import { Status } from '@/components/op'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

/* ────────────────────────────────────────────────────────────────────────
   The URL is the state — the live example beside `docs/requirements.md`.

   A small ledger whose filter, sort and page are written into an address
   line above it, defaults omitted. `reload` throws the view away and
   rebuilds it from that string alone: if the two screens differ, the
   address was not carrying the state.

   The address here is a string in a box, not `window.location`: this block
   sits on a documentation page, and a demo that rewrote the guide's own URL
   would move the reader instead of showing them the rule. The console's real
   screens use `src/sections/console-url.ts`.

   The skin (`operator ink v1`) belongs to the parent page.
   ──────────────────────────────────────────────────────────────────────── */

type Row = { id: string; project: string; env: string; state: 'ok' | 'warn' | 'error'; word: string; started: number }

const ROWS: Row[] = [
  { id: 'dep_93a', project: 'api-gateway', env: 'production', state: 'ok', word: 'deployed', started: 41 },
  { id: 'dep_92e', project: 'api-gateway', env: 'staging', state: 'error', word: 'failed', started: 96 },
  { id: 'dep_92b', project: 'acme-storefront', env: 'production', state: 'ok', word: 'deployed', started: 180 },
  { id: 'dep_91a', project: 'billing-worker', env: 'production', state: 'warn', word: 'degraded', started: 260 },
  { id: 'dep_90e', project: 'acme-storefront', env: 'preview', state: 'ok', word: 'deployed', started: 420 },
  { id: 'dep_89f', project: 'billing-worker', env: 'staging', state: 'error', word: 'failed', started: 610 },
]

type Sort = 'started' | 'project'
type View = { f: string; sort: Sort; page: number }

const DEFAULT: View = { f: '', sort: 'started', page: 1 }
const PAGE_SIZE = 3

/** The address the view would have. Defaults are absent, not written as `=default`. */
function writeUrl(v: View): string {
  const p = new URLSearchParams()
  if (v.f) p.set('f', v.f)
  if (v.sort !== DEFAULT.sort) p.set('sort', v.sort)
  if (v.page !== DEFAULT.page) p.set('page', String(v.page))
  const q = p.toString()
  return `/v1?p=deploys${q ? `&${q}` : ''}`
}

/** And back: the whole point is that this is loss-free. */
function readUrl(url: string): View {
  const p = new URLSearchParams(url.slice(url.indexOf('?') + 1))
  const sort = p.get('sort')
  const page = Number(p.get('page'))
  return {
    f: p.get('f') ?? DEFAULT.f,
    sort: sort === 'project' ? 'project' : DEFAULT.sort,
    page: Number.isFinite(page) && page >= 1 ? Math.floor(page) : DEFAULT.page,
  }
}

export function UrlStateDemo() {
  const [view, setView] = useState<View>(DEFAULT)
  const [rebuilt, setRebuilt] = useState(false)
  const url = writeUrl(view)

  const list = useMemo(() => {
    const needle = view.f.trim().toLowerCase()
    const matched = ROWS.filter((r) => !needle || `${r.id} ${r.project} ${r.env} ${r.word}`.toLowerCase().includes(needle))
    return [...matched].sort((a, b) => (view.sort === 'project' ? a.project.localeCompare(b.project) || a.started - b.started : a.started - b.started))
  }, [view.f, view.sort])
  const pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE))
  const page = Math.min(view.page, pages)
  const rows = list.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE)

  const change = (next: Partial<View>) => { setRebuilt(false); setView((v) => ({ ...v, ...next })) }
  // A reload is not a re-render: the view is dropped first, then read back out
  // of the address. Nothing else is allowed to survive the round trip.
  const reload = () => { const address = url; setView(DEFAULT); setView(readUrl(address)); setRebuilt(true) }

  return (
    <div className="space-y-3">
      <div className="border">
        <div className="flex items-center gap-2 border-b px-3 py-2">
          <span className="op-label shrink-0 text-muted-foreground">address</span>
          <span className="min-w-0 flex-1 truncate font-mono text-[11px]" data-url-state-demo>{url}</span>
          <Button size="sm" variant="outline" className="h-6 shrink-0 px-2 text-[11px]" onClick={reload}>reload</Button>
        </div>
        <div className="flex flex-wrap items-center gap-2 border-b px-3 py-2">
          <input
            value={view.f}
            onChange={(e) => change({ f: e.target.value, page: 1 })}
            placeholder="filter deployments"
            aria-label="Filter deployments"
            className="h-7 min-w-0 flex-1 border bg-background px-2 text-xs"
          />
          <span className="inline-flex h-7 shrink-0 border" role="group" aria-label="Sort">
            {(['started', 'project'] as const).map((s, i) => (
              <button
                key={s}
                type="button"
                aria-pressed={view.sort === s}
                onClick={() => change({ sort: s, page: 1 })}
                className={cn('h-full px-2 text-[11px]', i > 0 && 'border-s', view.sort === s ? 'bg-foreground text-background' : 'hover:bg-muted')}
              >
                {s}
              </button>
            ))}
          </span>
        </div>
        <ol className="op-rows text-xs">
          {rows.map((r) => (
            <li key={r.id} className="op-row grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-3">
              <span className="font-mono text-muted-foreground">{r.id}</span>
              <span className="min-w-0 truncate">{r.project} <span className="text-muted-foreground">· {r.env}</span></span>
              <Status state={r.state} label={r.word} />
            </li>
          ))}
          {rows.length === 0 && <li className="op-row text-muted-foreground">Nothing matches “{view.f}”. Widen the filter.</li>}
        </ol>
        <div className="flex items-center justify-between gap-2 border-t px-3 py-2 text-[11px] text-muted-foreground">
          <span className="font-mono">{list.length ? `${(page - 1) * PAGE_SIZE + 1}–${(page - 1) * PAGE_SIZE + rows.length} of ${list.length}` : '0 of 0'}</span>
          <span className="flex items-center gap-1">
            <Button size="sm" variant="outline" className="h-6 px-2 text-[11px]" disabled={page <= 1} onClick={() => change({ page: page - 1 })}>prev</Button>
            <Button size="sm" variant="outline" className="h-6 px-2 text-[11px]" disabled={page >= pages} onClick={() => change({ page: page + 1 })}>next</Button>
          </span>
        </div>
      </div>
      <p className="text-[11px] text-muted-foreground">
        {rebuilt
          ? <><span className="font-medium text-foreground">rebuilt from the address</span> · the filter, the sort and the page came back out of the string above, and nothing else was kept.</>
          : <>Change the filter, the sort or the page and watch the address. Then press <span className="font-mono">reload</span>: the view is thrown away and rebuilt from that string alone.</>}
      </p>
    </div>
  )
}
