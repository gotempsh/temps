// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState, type ReactNode } from 'react'
import type { GlobalLogLine } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from '@/components/ui/table'
import { Download, WrapText, X, Columns3 } from 'lucide-react'
import { cn } from '@/lib/utils'

import { LogVolume } from './LogVolume'
import { Input } from '@/components/ui/input'
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuCheckboxItem,
} from '@/components/ui/dropdown-menu'
import { useSearchParams } from 'react-router'
import { groupLogLines } from '@/lib/log-explorer'

type Patch = Record<string, string | undefined>
const identity = (line: GlobalLogLine) => `${line.chunk_id}:${line.line_offset}`
const tone = (level: string) =>
  level === 'ERROR'
    ? 'text-destructive'
    : level === 'WARN'
      ? 'text-amber-600 dark:text-amber-400'
      : 'text-muted-foreground'

/** Dense cross-project log list with page-scoped facets and an adjacent record inspector. */
export function LogExplorer({
  lines,
  onFilter,
  toolbar,
  footer,
  status,
  onRange,
  onInspect,
}: {
  lines: GlobalLogLine[]
  onFilter: (patch: Patch) => void
  toolbar?: ReactNode
  footer?: ReactNode
  status?: ReactNode
  onRange: (from: string, to: string) => void
  onInspect?: () => void
}) {
  const [params, setParams] = useSearchParams()
  const mode =
    params.get('lv') === 'patterns'
      ? 'patterns'
      : params.get('lv') === 'service'
        ? 'service'
        : 'list'
  const columns = (params.get('cols') ?? 'deployment').split(',')
  const [facetSearch, setFacetSearch] = useState('')
  const presentation = (key: string, value: string) =>
    setParams(
      (previous) => {
        const next = new URLSearchParams(previous)
        next.set(key, value)
        return next
      },
      { replace: true }
    )
  const groups = groupLogLines(
    lines,
    mode === 'service' ? 'service' : 'message'
  )
  const inspector = useRef<HTMLHeadingElement>(null)
  const opener = useRef<HTMLButtonElement | null>(null)
  const [selected, setSelected] = useState<string>()
  const [wrap, setWrap] = useState(false)
  useEffect(() => {
    if (selected) inspector.current?.focus()
  }, [selected])
  const line = lines.find((entry) => identity(entry) === selected)
  const facets = [
    {
      title: 'Level',
      key: 'level',
      values: lines.map((entry) => ({
        value: entry.level,
        label: entry.level,
      })),
    },
    {
      title: 'Project',
      key: 'project_id',
      values: lines
        .filter((entry) => entry.project_id != null)
        .map((entry) => ({
          value: String(entry.project_id),
          label: entry.owner,
        })),
    },
    {
      title: 'Environment',
      key: 'env',
      values: lines
        .filter((entry) => entry.env)
        .map((entry) => ({ value: entry.env, label: entry.env })),
    },
    {
      title: 'Node',
      key: 'node_id',
      values: lines
        .filter((entry) => entry.node_id != null)
        .map((entry) => ({
          value: String(entry.node_id),
          label: entry.node_name || `Node ${entry.node_id}`,
        })),
    },
  ]
  facets.push({
    title: 'Source',
    key: 'source',
    values: lines
      .filter(
        (entry) => entry.project_id != null || entry.external_service_id != null
      )
      .map((entry) => ({
        value: entry.project_id != null ? 'application' : 'service',
        label: entry.project_id != null ? 'Applications' : 'Databases',
      })),
  })
  facets.push({
    title: 'Deployment',
    key: 'deploy_id',
    values: lines
      .filter((entry) => entry.deploy_id != null)
      .map((entry) => ({
        value: String(entry.deploy_id),
        label: `Deployment ${entry.deploy_id}`,
      })),
  })
  const exportPage = () => {
    const url = URL.createObjectURL(
      new Blob([lines.map((entry) => JSON.stringify(entry)).join('\n')], {
        type: 'application/x-ndjson',
      })
    )
    const link = document.createElement('a')
    link.href = url
    link.download = 'logs-current-page.ndjson'
    link.click()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  }
  return (
    <div className="grid min-w-0 items-start gap-5 xl:grid-cols-[minmax(0,1fr)_264px]">
      <section aria-label="Log explorer" className="min-w-0">
        {toolbar}
        <LogVolume lines={lines} onRange={onRange} />
        <div className="flex flex-wrap items-center justify-between gap-2 border-y py-2">
          <span className="text-xs text-muted-foreground">
            {lines.length} loaded {lines.length === 1 ? 'line' : 'lines'} ·
            newest first
          </span>
          <div className="flex flex-wrap items-center gap-1">
            <div
              role="group"
              aria-label="Log presentation"
              className="inline-flex rounded-md border p-0.5"
            >
              {(
                [
                  ['list', 'List'],
                  ['patterns', 'Patterns'],
                  ['service', 'By service'],
                ] as const
              ).map(([value, label]) => (
                <Button
                  key={value}
                  variant={mode === value ? 'secondary' : 'ghost'}
                  size="sm"
                  className="h-7 px-2 text-xs"
                  aria-pressed={mode === value}
                  onClick={() => presentation('lv', value)}
                >
                  {label}
                </Button>
              ))}
            </div>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="sm" className="h-7 px-2 text-xs">
                  <Columns3 className="mr-1 size-3.5" />
                  Columns
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                {['deployment', 'node', 'environment'].map((column) => (
                  <DropdownMenuCheckboxItem
                    key={column}
                    checked={columns.includes(column)}
                    onCheckedChange={(checked) =>
                      presentation(
                        'cols',
                        (checked
                          ? [...columns, column]
                          : columns.filter((value) => value !== column)
                        ).join(',')
                      )
                    }
                  >
                    {column}
                  </DropdownMenuCheckboxItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>

            <Button
              variant="ghost"
              size="sm"
              aria-pressed={wrap}
              onClick={() => setWrap((value) => !value)}
            >
              <WrapText className="mr-1.5 size-3.5" />
              Wrap
            </Button>
            <CopyButton
              value={window.location.href}
              label="Copy search link"
              size="sm"
              variant="ghost"
            />
            <Button variant="ghost" size="sm" onClick={exportPage}>
              <Download className="mr-1.5 size-3.5" />
              Export page
            </Button>
          </div>
        </div>
        {status ||
          (mode !== 'list' ? (
            <div className="border-x border-b">
              <p className="border-b px-3 py-2 text-xs text-muted-foreground">
                {mode === 'patterns' ? 'Exact repeated messages' : 'Services'}{' '}
                on this loaded page. Select a row to inspect an example.
              </p>
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>
                      {mode === 'patterns' ? 'Message pattern' : 'Service'}
                    </TableHead>
                    <TableHead className="w-20 text-right">Lines</TableHead>
                    <TableHead className="w-20 text-right">Errors</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {groups.map((group) => (
                    <TableRow key={group.id}>
                      <TableCell className="max-w-sm">
                        <button
                          type="button"
                          className="w-full truncate text-left font-mono text-[11px] hover:underline"
                          onClick={(event) => {
                            opener.current = event.currentTarget
                            onInspect?.()
                            setSelected(identity(group.example))
                          }}
                        >
                          {group.label}
                        </button>
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        {group.count}
                      </TableCell>
                      <TableCell className="text-right font-mono text-xs">
                        {group.errors}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          ) : (
            <div className="border-x border-b [&>div]:max-h-[62vh]">
              <Table className="table-fixed">
                <TableHeader className="sticky top-0 z-10 bg-muted/70 [&_th]:h-8 [&_th]:text-[10px] [&_th]:uppercase [&_th]:tracking-wide">
                  <TableRow>
                    <TableHead className="hidden w-24 md:table-cell">
                      Time
                    </TableHead>
                    <TableHead className="w-16">Level</TableHead>
                    <TableHead className="hidden w-32 md:table-cell">
                      Project / service
                    </TableHead>
                    <TableHead>Message</TableHead>
                    {columns
                      .filter((column) =>
                        ['deployment', 'node', 'environment'].includes(column)
                      )
                      .map((column) => (
                        <TableHead
                          key={column}
                          className="hidden w-24 xl:table-cell"
                        >
                          {column}
                        </TableHead>
                      ))}
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {lines.map((entry) => (
                    <TableRow
                      key={identity(entry)}
                      data-state={
                        selected === identity(entry) ? 'selected' : undefined
                      }
                    >
                      <TableCell
                        className="hidden py-1.5 font-mono text-[11px] tabular-nums md:table-cell"
                        title={new Date(entry.timestamp).toLocaleString()}
                      >
                        {new Date(entry.timestamp).toLocaleTimeString(
                          undefined,
                          { hour12: false, timeZone: 'UTC' }
                        )}
                      </TableCell>
                      <TableCell
                        className={cn(
                          'py-1.5 font-mono text-[11px]',
                          tone(entry.level)
                        )}
                      >
                        {entry.level}
                      </TableCell>
                      <TableCell className="hidden py-1.5 md:table-cell">
                        <p
                          className="truncate text-xs"
                          title={`${entry.owner} / ${entry.service}`}
                        >
                          {entry.owner} / {entry.service}
                        </p>
                      </TableCell>
                      <TableCell className="py-0.5">
                        <p className="truncate text-xs text-muted-foreground md:hidden">
                          {entry.owner} ·{' '}
                          {new Date(entry.timestamp).toLocaleTimeString()}
                        </p>
                        <button
                          type="button"
                          aria-label={`Inspect log: ${entry.message}`}
                          aria-pressed={selected === identity(entry)}
                          onClick={(event) => {
                            opener.current = event.currentTarget
                            onInspect?.()
                            setSelected(identity(entry))
                          }}
                          className={cn(
                            'block w-full rounded py-1 text-left font-mono text-[11px] hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring',
                            wrap ? 'whitespace-pre-wrap break-all' : 'truncate'
                          )}
                        >
                          {entry.message}
                        </button>
                      </TableCell>
                      {columns
                        .filter((column) =>
                          ['deployment', 'node', 'environment'].includes(column)
                        )
                        .map((column) => (
                          <TableCell
                            key={column}
                            className="hidden truncate py-1.5 font-mono text-[11px] text-muted-foreground xl:table-cell"
                          >
                            {(column === 'deployment'
                              ? entry.deploy_id
                              : column === 'node'
                                ? entry.node_name
                                : entry.env) ?? '—'}
                          </TableCell>
                        ))}
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          ))}
        {footer}
      </section>
      {line ? (
        <aside
          aria-label="Log record"
          className="min-w-0 border-l pl-4 xl:sticky xl:top-4"
        >
          <div className="mb-3 flex items-center justify-between">
            <h2
              ref={inspector}
              tabIndex={-1}
              className="text-sm font-semibold outline-none"
            >
              Log record
            </h2>
            <Button
              variant="ghost"
              size="icon"
              aria-label="Close log record"
              onClick={() => {
                setSelected(undefined)
                opener.current?.focus()
              }}
            >
              <X className="size-4" />
            </Button>
          </div>
          <p className={cn('mb-1 font-mono text-xs', tone(line.level))}>
            {line.level}
          </p>
          <time
            className="text-xs text-muted-foreground"
            dateTime={line.timestamp}
          >
            {new Date(line.timestamp).toLocaleString()}
          </time>
          <pre className="my-3 max-h-64 overflow-auto whitespace-pre-wrap break-all rounded bg-muted p-3 text-xs">
            {line.message}
          </pre>
          <CopyButton
            value={line.message}
            label="Copy log message"
            size="sm"
            variant="outline"
          />
          <dl className="my-4 space-y-3 text-xs">
            {[
              ['Source', line.owner],
              ['Environment', line.env],
              ['Service', line.service],
              ['Node', line.node_name],
              ['Container', line.container_id],
              ['Deployment', line.deploy_id],
              ['Chunk', line.chunk_id],
              ['Line offset', line.line_offset],
            ].map(([label, value]) => (
              <div key={String(label)}>
                <dt className="text-muted-foreground">{label}</dt>
                <dd className="break-all font-mono">{value ?? '—'}</dd>
              </div>
            ))}
          </dl>
          {line.fields != null && (
            <>
              <h3 className="mb-2 text-xs font-semibold">Structured fields</h3>
              <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded bg-muted p-3 text-xs">
                {JSON.stringify(line.fields, null, 2)}
              </pre>
            </>
          )}
        </aside>
      ) : (
        <aside aria-label="Log facets" className="space-y-5 xl:sticky xl:top-4">
          <div>
            <h2 className="text-sm font-semibold">Facets</h2>
            <p className="mt-1 text-xs text-muted-foreground">
              Counts from this page only. Select a value to search the full time
              range.
            </p>
          </div>
          <Input
            aria-label="Filter facets"
            placeholder="Filter facets"
            value={facetSearch}
            onChange={(event) => setFacetSearch(event.target.value)}
            className="h-8 text-xs"
          />
          {facets.map((facet) => {
            const counts = new Map<string, { label: string; count: number }>()
            for (const value of facet.values) {
              const prior = counts.get(value.value)
              counts.set(value.value, {
                label: value.label,
                count: (prior?.count ?? 0) + 1,
              })
            }
            const visible = [...counts].filter(([, item]) =>
              `${facet.title} ${item.label}`
                .toLowerCase()
                .includes(facetSearch.toLowerCase())
            )
            return (
              visible.length > 0 && (
                <section
                  key={facet.key}
                  aria-label={`${facet.title} facets`}
                  className="border-t pt-3"
                >
                  <h3 className="mb-1 text-xs font-medium text-muted-foreground">
                    {facet.title}
                  </h3>
                  {visible
                    .sort((a, b) => b[1].count - a[1].count)
                    .map(([value, item]) => (
                      <Button
                        key={value}
                        variant="ghost"
                        size="sm"
                        className="relative flex h-7 w-full justify-between gap-2 overflow-hidden rounded-none border-b px-2 text-[11px]"
                        onClick={() =>
                          onFilter({
                            [facet.key]: value,
                            ...(facet.key === 'source' && value === 'service'
                              ? { project_id: undefined }
                              : {}),
                          })
                        }
                      >
                        <span
                          aria-hidden="true"
                          className="pointer-events-none absolute inset-y-1 left-0 bg-muted/70"
                          style={{
                            width: `${(item.count / Math.max(1, lines.length)) * 100}%`,
                          }}
                        />
                        <span className="relative truncate">{item.label}</span>
                        <span className="relative flex gap-3 tabular-nums text-muted-foreground">
                          {item.count}
                          <span className="w-8 text-right">
                            {Math.round(
                              (item.count / Math.max(1, lines.length)) * 100
                            )}
                            %
                          </span>
                        </span>
                      </Button>
                    ))}
                </section>
              )
            )
          })}
        </aside>
      )}
    </div>
  )
}
