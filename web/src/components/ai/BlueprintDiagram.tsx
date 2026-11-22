// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  BlueprintDocument,
  ComponentLive,
  Flow,
} from '@/api/client/types.gen'
import { cn } from '@/lib/utils'
import { useId, type KeyboardEvent } from 'react'
import {
  NODE_HEIGHT,
  layoutBlueprint,
  type BlueprintKind,
} from './blueprint-layout'
import { LIVE_LABELS, LIVE_TONES, componentDetail } from './chat-blueprint'

export interface DiagramChanges {
  added: ReadonlySet<string>
  changed: ReadonlySet<string>
  addedFlows: ReadonlySet<number>
}

const KIND_LABELS: Record<BlueprintKind, string> = {
  client: 'Client',
  app: 'App',
  data: 'Data',
  external: 'Outside',
}

const KIND_FILL: Record<BlueprintKind, string> = {
  client: 'fill-sky-500/10 stroke-sky-500/50',
  app: 'fill-violet-500/10 stroke-violet-500/50',
  data: 'fill-emerald-500/10 stroke-emerald-500/50',
  external: 'fill-muted stroke-border',
}

const LIVE_DOT: Record<string, string> = {
  created: 'fill-muted-foreground/50',
  building: 'fill-amber-500',
  live: 'fill-emerald-500',
  failed: 'fill-destructive',
  stopped: 'fill-muted-foreground/50',
  missing: 'fill-destructive',
}

function clip(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text
}

/**
 * The architecture as a picture: who uses it, the code Temps runs, the state
 * it keeps and what it calls outside, with every flow between them. A wide
 * container gets the diagram; a narrow one the same content as a list.
 */
export function BlueprintDiagram({
  document,
  live,
  changes,
  selected,
  onSelect,
  className,
}: {
  document: BlueprintDocument
  live?: Record<string, ComponentLive>
  changes?: DiagramChanges
  selected?: string | null
  onSelect?: (componentId: string) => void
  className?: string
}) {
  const flows = document.flows ?? []
  return (
    // The picture needs room for its columns; a narrow panel gets the list.
    <div className={cn('@container', className)}>
      <DiagramSvg
        document={document}
        flows={flows}
        live={live}
        changes={changes}
        selected={selected}
        onSelect={onSelect}
      />
      <DiagramList
        document={document}
        flows={flows}
        live={live}
        changes={changes}
        selected={selected}
        onSelect={onSelect}
      />
    </div>
  )
}

interface ViewProps {
  document: BlueprintDocument
  flows: Flow[]
  live?: Record<string, ComponentLive>
  changes?: DiagramChanges
  selected?: string | null
  onSelect?: (componentId: string) => void
}

function DiagramSvg({
  document,
  flows,
  live,
  changes,
  selected,
  onSelect,
}: ViewProps) {
  const markerId = useId().replace(/:/g, '')
  const layout = layoutBlueprint(document.components, flows)
  const byId = new Map(document.components.map((c) => [c.id, c]))
  if (layout.width === 0) return null
  const onKey = (id: string) => (event: KeyboardEvent<SVGGElement>) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault()
      onSelect?.(id)
    }
  }
  return (
    <div className="hidden overflow-x-auto @[600px]:block">
      <svg
        role="img"
        aria-label={`Architecture: ${document.summary}`}
        viewBox={`0 0 ${layout.width} ${layout.height}`}
        width={layout.width}
        height={layout.height}
        className="max-w-none text-foreground"
      >
        <defs>
          <marker
            id={markerId}
            viewBox="0 0 10 10"
            refX="9"
            refY="5"
            markerWidth="7"
            markerHeight="7"
            orient="auto-start-reverse"
          >
            <path d="M 0 0 L 10 5 L 0 10 z" className="fill-muted-foreground" />
          </marker>
        </defs>
        {layout.columns.map((column) => (
          <text
            key={column.kind}
            x={column.x}
            y={18}
            className="fill-muted-foreground text-[10px] font-medium uppercase tracking-wide"
          >
            {column.title}
          </text>
        ))}
        {layout.edges.map((edge) => {
          const flow = flows[edge.index]
          const added = changes?.addedFlows.has(edge.index)
          return (
            <g key={edge.index}>
              <title>{`${flow.from} → ${flow.to}: ${flow.kind}, ${flow.label}`}</title>
              <path
                d={edge.path}
                fill="none"
                markerEnd={`url(#${markerId})`}
                strokeDasharray={
                  flow.kind === 'publish' || flow.kind === 'subscribe'
                    ? '4 3'
                    : undefined
                }
                className={cn(
                  'stroke-muted-foreground/60',
                  added && 'stroke-primary'
                )}
                strokeWidth={added ? 2 : 1.25}
              />
              <text
                x={edge.labelX}
                y={edge.labelY}
                textAnchor={edge.labelAnchor}
                // A halo in the background colour keeps a label readable where
                // another flow passes behind it.
                paintOrder="stroke"
                strokeWidth={3}
                strokeLinejoin="round"
                className="fill-muted-foreground stroke-background text-[9px]"
              >
                {clip(flow.kind, 12)}
              </text>
            </g>
          )
        })}
        {layout.nodes.map((node) => {
          const component = byId.get(node.id)
          if (!component) return null
          const state = live?.[node.id]
          const isSelected = selected === node.id
          const isNew = changes?.added.has(node.id)
          const isChanged = changes?.changed.has(node.id)
          return (
            <g
              key={node.id}
              transform={`translate(${node.x} ${node.y})`}
              role={onSelect ? 'button' : undefined}
              tabIndex={onSelect ? 0 : undefined}
              aria-label={`${component.name}, ${KIND_LABELS[component.kind]}${
                state ? `, ${LIVE_LABELS[state.status]}` : ''
              }`}
              aria-pressed={onSelect ? isSelected : undefined}
              onClick={onSelect ? () => onSelect(node.id) : undefined}
              onKeyDown={onSelect ? onKey(node.id) : undefined}
              className={cn(
                onSelect && 'cursor-pointer focus:outline-none',
                'group'
              )}
            >
              <rect
                width={node.width}
                height={node.height}
                rx={8}
                className={cn(
                  KIND_FILL[component.kind],
                  isNew && 'stroke-primary',
                  isChanged && 'stroke-amber-500',
                  isSelected && 'stroke-foreground',
                  'group-focus-visible:stroke-ring'
                )}
                strokeWidth={isSelected || isNew || isChanged ? 2 : 1}
                strokeDasharray={isNew ? '5 3' : undefined}
              />
              <text
                x={12}
                y={22}
                className="fill-foreground text-[12px] font-medium"
              >
                {clip(component.name, 22)}
              </text>
              <text x={12} y={40} className="fill-muted-foreground text-[10px]">
                {clip(componentDetail(component), 28)}
              </text>
              {state && (
                <circle
                  cx={node.width - 12}
                  cy={NODE_HEIGHT / 2 - 12}
                  r={4}
                  className={LIVE_DOT[state.status]}
                >
                  <title>{LIVE_LABELS[state.status]}</title>
                </circle>
              )}
            </g>
          )
        })}
      </svg>
    </div>
  )
}

function DiagramList({
  document,
  flows,
  live,
  changes,
  selected,
  onSelect,
}: ViewProps) {
  const byId = new Map(document.components.map((c) => [c.id, c]))
  const name = (id: string) => byId.get(id)?.name ?? id
  return (
    <div className="space-y-3 @[600px]:hidden">
      <ul className="space-y-1.5" aria-label="Parts">
        {document.components.map((component) => {
          const state = live?.[component.id]
          const body = (
            <>
              <span className="min-w-0 flex-1">
                <span className="block truncate text-sm font-medium">
                  {component.name}
                  {changes?.added.has(component.id) && (
                    <span className="ml-1.5 text-[10px] font-medium text-primary">
                      new
                    </span>
                  )}
                  {changes?.changed.has(component.id) && (
                    <span className="ml-1.5 text-[10px] font-medium text-amber-700 dark:text-amber-400">
                      changed
                    </span>
                  )}
                </span>
                <span className="block truncate text-xs text-muted-foreground">
                  {KIND_LABELS[component.kind]} · {componentDetail(component)}
                </span>
              </span>
              {state && (
                <span
                  className={cn(
                    'shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium',
                    LIVE_TONES[state.status]
                  )}
                >
                  {LIVE_LABELS[state.status]}
                </span>
              )}
            </>
          )
          return (
            <li key={component.id}>
              {onSelect ? (
                <button
                  type="button"
                  aria-pressed={selected === component.id}
                  onClick={() => onSelect(component.id)}
                  className={cn(
                    'flex w-full items-center gap-2 rounded-md border px-3 py-2 text-left',
                    selected === component.id && 'border-foreground'
                  )}
                >
                  {body}
                </button>
              ) : (
                <div className="flex items-center gap-2 rounded-md border px-3 py-2">
                  {body}
                </div>
              )}
            </li>
          )
        })}
      </ul>
      {flows.length > 0 && (
        <ul className="space-y-1 text-xs" aria-label="Flows">
          {flows.map((flow, index) => (
            <li
              key={`${flow.from}-${flow.to}-${index}`}
              className={cn(
                'text-muted-foreground',
                changes?.addedFlows.has(index) && 'text-primary'
              )}
            >
              <span className="font-medium text-foreground">
                {name(flow.from)}
              </span>{' '}
              →{' '}
              <span className="font-medium text-foreground">
                {name(flow.to)}
              </span>{' '}
              · {flow.kind} · {flow.label}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
