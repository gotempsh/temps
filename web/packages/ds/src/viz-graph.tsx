// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, useState, type ReactNode } from 'react'
import { ChevronRight } from 'lucide-react'
import { cn } from './lib/cn'
import { fmtNum, fmtPct } from './fmt'
import { GLYPH, GLYPH_CLASS, type State } from './status'

/* ────────────────────────────────────────────────────────────────────────
   Things with edges: where visitors went next, and what talks to what. Both
   are drawn as a list first — the list carries the keyboard and the numbers
   — with the picture as the second view, the way `GeoMap` sits beside a
   ranked list. See `design-system/docs/data-viz.md`.
   ──────────────────────────────────────────────────────────────────────── */

// ── PathTree ───────────────────────────────────────────────────────────

/** One step on a journey and everything that happened after it. */
export type PathNode = {
  /** The page, event or screen ("/", "signup", "checkout"). */
  label: string
  /** How many sessions reached this step. */
  count: number
  /** How many stopped here (closed the tab, no next event). Drawn as the branch's drop-off. */
  exits?: number
  /** Where the rest went next, ranked by the caller. */
  children?: PathNode[]
  note?: string
}

function PathBranch({ node, parentCount, depth, dropAlert, onOpen }: { node: PathNode; parentCount: number; depth: number; dropAlert: number; onOpen?: (n: PathNode) => void }) {
  const [open, setOpen] = useState(depth < 1)
  const share = parentCount ? (node.count / parentCount) * 100 : 0
  const exits = node.exits ?? 0
  const drop = node.count ? (exits / node.count) * 100 : 0
  const kids = node.children ?? []
  return (
    <li>
      <div className="flex min-w-0 items-center gap-2 px-3 py-1.5" style={{ paddingLeft: `${0.75 + depth * 1.25}rem` }}>
        <button type="button" disabled={!kids.length} aria-expanded={kids.length ? open : undefined} onClick={() => setOpen((o) => !o)}
          className={cn('flex min-w-0 flex-1 items-center gap-1.5 text-left', kids.length && 'hover:text-foreground')}>
          {kids.length
            ? <ChevronRight aria-hidden className={cn('h-3 w-3 shrink-0 text-muted-foreground transition-transform', open && 'rotate-90')} />
            : <span aria-hidden className="w-3 shrink-0 text-center text-muted-foreground">·</span>}
          <span className="min-w-0 truncate font-mono">{node.label}</span>
          {node.note && <span className="shrink-0 text-[11px] text-muted-foreground">{node.note}</span>}
        </button>
        <span className="shrink-0 font-mono tabular-nums">{fmtNum(node.count)}</span>
        <span className="w-12 shrink-0 text-right font-mono tabular-nums text-muted-foreground">{fmtPct(share, { digits: share < 10 ? 1 : 0 })}</span>
        {/* Drop-off is the number the reader came for, so it is the only thing on the row that can turn red. */}
        <span className={cn('w-20 shrink-0 text-right font-mono tabular-nums', drop >= dropAlert ? 'text-destructive' : 'text-muted-foreground')}>
          {exits ? <>{drop >= dropAlert && <span aria-hidden className="mr-1">×</span>}{fmtPct(drop, { digits: 0 })} left</> : ''}
        </span>
        {onOpen && <button type="button" onClick={() => onOpen(node)} className="shrink-0 font-mono text-[10px] text-muted-foreground underline underline-offset-4 hover:text-foreground">open</button>}
      </div>
      {open && kids.length > 0 && <ul className="op-rows border-t border-[var(--op-rule-soft)]">{kids.map((k) => <PathBranch key={k.label} node={k} parentCount={node.count} depth={depth + 1} dropAlert={dropAlert} onOpen={onOpen} />)}</ul>}
    </li>
  )
}

/**
 * Where visitors went next, as an indented tree: entry at the root, each step
 * with its count, its share of the step above and how many left there. It
 * replaces the Sankey, which is banned — a Sankey's ribbons cannot be compared
 * (their thickness is not read from a baseline), cannot be labelled without
 * overprinting, and cannot be reached by a keyboard at all.
 *
 * Branches collapse; every toggle is a real button, so Tab and Enter walk the
 * journey. Drop-off above `dropAlert` is the one thing that takes a tone.
 *
 * ```tsx
 * <PathTree root={{ label: '/', count: 12418, exits: 4820, children: [...] }}
 *   label="journeys from the landing page" verdict="Half of the sessions that
 *   reach /pricing leave without opening /signup." />
 * ```
 */
export function PathTree({ root, label, verdict, dropAlert = 50, onOpen, meta, className }: {
  root: PathNode
  label: string
  verdict: string
  /** Share of a branch that leaves before a drop-off turns red. */
  dropAlert?: number
  onOpen?: (n: PathNode) => void
  meta?: ReactNode
  className?: string
}) {
  const total = root.count
  const steps = useMemo(() => {
    let n = 0
    const walk = (x: PathNode) => { n += 1; (x.children ?? []).forEach(walk) }
    walk(root)
    return n
  }, [root])
  return (
    <div className={cn('min-w-0 border bg-background text-xs', className)}>
      <div className="flex items-center gap-3 border-b px-3 py-1.5">
        <span className="op-label min-w-0 flex-1 truncate">{label}</span>
        <span className="op-label shrink-0 text-[9px]">sessions</span>
        <span className="op-label w-12 shrink-0 text-right text-[9px]">of above</span>
        <span className="op-label w-20 shrink-0 text-right text-[9px]">drop-off</span>
      </div>
      <ul className="op-rows" aria-label={`${label}. ${verdict.replace(/\.\s*$/, '')}. ${fmtNum(total)} sessions across ${steps} steps.`}>
        <PathBranch node={root} parentCount={total} depth={0} dropAlert={dropAlert} onOpen={onOpen} />
      </ul>
      <p className="border-t px-3 py-1.5 font-mono text-[10px] text-muted-foreground">{meta ?? <>{fmtNum(total)} sessions · share is of the step above · × drop-off at or above {dropAlert}%</>}</p>
    </div>
  )
}

// ── Topology ───────────────────────────────────────────────────────────

/** A machine, a service or a database in the graph. */
export type TopoNode = {
  id: string
  /** What it is called. Printed in mono. */
  label: string
  /** What kind of thing it is ("control plane", "worker", "service", "database"). One word. */
  kind: string
  state: State
  /**
   * Which row it sits on. Layers are drawn top to bottom in ascending order,
   * so the layout is deterministic: no force simulation, no reflow on reload.
   */
  layer: number
  /** Two or three facts under the name ("10.0.3.2 · 4 vCPU"). */
  facts?: string
}
/** An edge. `relay` is drawn dashed because it is a different kind of reach, not a worse one. */
export type TopoLink = { from: string; to: string; kind?: 'direct' | 'relay' | 'call'; label?: string; state?: State }

/**
 * Nodes and the links between them: a cluster (control plane, workers, their
 * WireGuard links) or a service map (services and their call edges). The
 * layout is layered and deterministic — `layer` decides the row, order in the
 * array decides the column — because a graph that moves between reloads
 * cannot be compared with the one the operator saw yesterday.
 *
 * The list under the graph is the primary view and carries the keyboard, the
 * numbers and the state words; the graph is the second view of the same rows,
 * exactly as `GeoMap` sits under a ranked list. The graph itself is one
 * `role="img"` with a sentence and no focusable children — a control inside a
 * picture is a nested-interactive violation, and the list already has a button
 * for every node.
 *
 * ```tsx
 * <Topology nodes={nodes} links={links} label="cluster"
 *   verdict="hetzner-3 has not sent a heartbeat for 4 minutes." />
 * ```
 */
export function Topology({ nodes, links, label, verdict, onOpen, height = 260, meta, className }: {
  nodes: TopoNode[]
  links: TopoLink[]
  label: string
  verdict: string
  onOpen?: (n: TopoNode) => void
  height?: number
  meta?: ReactNode
  className?: string
}) {
  const [hot, setHot] = useState<string | null>(null)
  const layers = useMemo(() => [...new Set(nodes.map((n) => n.layer))].sort((a, b) => a - b), [nodes])
  const at = useMemo(() => {
    const m = new Map<string, { x: number; y: number }>()
    layers.forEach((l, li) => {
      const row = nodes.filter((n) => n.layer === l)
      row.forEach((n, i) => m.set(n.id, { x: ((i + 0.5) / row.length) * 100, y: ((li + 0.5) / layers.length) * 100 }))
    })
    return m
  }, [nodes, layers])
  const byId = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes])
  const linksOf = (id: string) => links.filter((l) => l.from === id || l.to === id)
  const sentence = `${label}: ${nodes.length} nodes on ${layers.length} rows, ${links.length} links. ${verdict.replace(/\.\s*$/, '')}. The list below has every node and its links.`
  return (
    <div className={cn('min-w-0 space-y-2', className)}>
      {/* The graph clips rather than pushing the page sideways on a phone: it is
          the second view, and the list under it carries every fact in full. */}
      <div role="img" aria-label={sentence} className="relative min-w-0 overflow-hidden border bg-background" style={{ height }}>
        <svg aria-hidden viewBox="0 0 100 100" preserveAspectRatio="none" className="absolute inset-0 h-full w-full">
          {links.map((l, i) => {
            const a = at.get(l.from), b = at.get(l.to)
            if (!a || !b) return null
            const on = hot === l.from || hot === l.to
            return (
              <line key={i} x1={a.x} y1={a.y} x2={b.x} y2={b.y} vectorEffect="non-scaling-stroke"
                stroke={l.state === 'error' ? 'var(--destructive)' : l.state === 'warn' ? 'var(--warning)' : 'var(--foreground)'}
                strokeOpacity={on ? 1 : 0.35} strokeWidth={on ? 1.5 : 1}
                strokeDasharray={l.kind === 'relay' ? '4 3' : l.kind === 'call' ? '1 3' : undefined} />
            )
          })}
        </svg>
        {/* The cards are part of the picture, not controls: a focusable child
            inside a `role="img"` is a nested-interactive axe violation, and the
            same nodes are real buttons in the list beneath. A pointer can still
            click one; a keyboard uses the list, exactly as with `GeoMap`. */}
        {nodes.map((n) => {
          const p = at.get(n.id)!
          return (
            <span key={n.id} aria-hidden onMouseEnter={() => setHot(n.id)} onMouseLeave={() => setHot(null)}
              onClick={() => onOpen?.(n)}
              className={cn('absolute block max-w-[min(18rem,46%)] -translate-x-1/2 -translate-y-1/2 border bg-background px-2 py-1 text-left text-xs shadow-[2px_2px_0_0_var(--foreground)]', hot === n.id && 'bg-muted', onOpen && 'cursor-pointer hover:bg-muted')}
              style={{ left: `${p.x}%`, top: `${p.y}%` }}>
              <span className="flex min-w-0 items-center gap-1.5 font-mono">
                <span className={cn('shrink-0', GLYPH_CLASS[n.state])}>{GLYPH[n.state]}</span>
                <span className="truncate">{n.label}</span>
              </span>
              <span className="block truncate font-mono text-[10px] text-muted-foreground">{n.kind}{n.facts ? ` · ${n.facts}` : ''}</span>
            </span>
          )
        })}
      </div>
      <ul className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[10px] text-muted-foreground">
        {(['direct', 'relay', 'call'] as const).filter((k) => links.some((l) => (l.kind ?? 'direct') === k)).map((k) => (
          <li key={k} className="flex items-center gap-1.5">
            <svg aria-hidden width={18} height={6} viewBox="0 0 18 6" className="shrink-0"><line x1={0} y1={3} x2={18} y2={3} stroke="var(--foreground)" strokeWidth={1} strokeDasharray={k === 'relay' ? '4 3' : k === 'call' ? '1 3' : undefined} /></svg>{k}
          </li>
        ))}
      </ul>
      {/* The list is the view that carries the keyboard, the numbers and the state words. */}
      <ol className="op-rows border bg-background text-xs">
        {nodes.map((n) => (
          <li key={n.id}>
            <button type="button" disabled={!onOpen} onMouseEnter={() => setHot(n.id)} onMouseLeave={() => setHot(null)} onFocus={() => setHot(n.id)} onBlur={() => setHot(null)} onClick={() => onOpen?.(n)}
              className={cn('grid w-full grid-cols-1 items-center gap-x-3 px-3 py-1.5 text-left sm:grid-cols-[minmax(0,1fr)_minmax(0,auto)]', onOpen && 'hover:bg-muted/60', hot === n.id && 'bg-muted/60')}>
              <span className="min-w-0">
                <span className="flex min-w-0 items-center gap-1.5 font-mono">
                  <span aria-hidden className={cn('w-3 shrink-0 text-center', GLYPH_CLASS[n.state])}>{GLYPH[n.state]}</span>
                  <span className="truncate">{n.label}</span>
                  <span className="shrink-0 text-[11px] text-muted-foreground">{n.kind}</span>
                </span>
                {n.facts && <span className="block truncate pl-[1.125rem] font-mono text-[11px] text-muted-foreground">{n.facts}</span>}
              </span>
              {/* The links wrap rather than squeezing the name out of existence. */}
              <span className="min-w-0 font-mono text-[11px] text-muted-foreground sm:text-right">
                {linksOf(n.id).map((l) => `${l.kind ?? 'direct'} ${byId.get(l.from === n.id ? l.to : l.from)?.label ?? '?'}`).join(' · ') || 'no links'}
              </span>
            </button>
          </li>
        ))}
      </ol>
      <p className="font-mono text-[10px] text-muted-foreground">{meta ?? <>{nodes.length} nodes · {links.length} links · the graph and the list are the same rows</>}</p>
    </div>
  )
}
