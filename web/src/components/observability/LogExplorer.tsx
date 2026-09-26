// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { HighlightedCode } from '@/components/ui/code-block'
import { AnsiLogMessage } from './AnsiLogMessage'

import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from 'react'
import { useQuery } from '@tanstack/react-query'
import { useVirtualizer } from '@tanstack/react-virtual'
import { getLogContext } from '@/api/client/sdk.gen'
import type {
  FacetValue,
  GlobalLogFacetsResponse,
  GlobalLogLine,
} from '@/api/client/types.gen'
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
import {
  Download,
  WrapText,
  X,
  Columns3,
  ListFilter,
  Loader2,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { logEnvironmentLabel } from '@/lib/log-environment'
import { LogLevelBadge } from '@temps-sdk/ds'

import { Input } from '@/components/ui/input'
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuCheckboxItem,
} from '@/components/ui/dropdown-menu'
import { useSearchParams } from 'react-router'
import { groupLogLines, logLineKey } from '@/lib/log-explorer'
import { facetValues } from '@/hooks/useGlobalLogs'

type Patch = Record<string, string | undefined>

const EXTRA_COLUMNS = ['deployment', 'node', 'environment']

// Keep the default panel visibility aligned with the two-column layout.
const desktopQuery = '(min-width: 1280px)'
const mobileQuery = '(max-width: 1023px)'
function subscribeDesktop(onChange: () => void) {
  const query = window.matchMedia(desktopQuery)
  query.addEventListener('change', onChange)
  return () => query.removeEventListener('change', onChange)
}
function subscribeMobile(onChange: () => void) {
  const query = window.matchMedia(mobileQuery)
  query.addEventListener('change', onChange)
  return () => query.removeEventListener('change', onChange)
}
const desktopSnapshot = () =>
  typeof window.matchMedia === 'function'
    ? window.matchMedia(desktopQuery).matches
    : true
const serverDesktopSnapshot = () => true
const mobileSnapshot = () =>
  typeof window.matchMedia === 'function'
    ? window.matchMedia(mobileQuery).matches
    : false
const serverMobileSnapshot = () => false
/** Row height guess for the virtualizer; real heights are measured on mount. */
const ROW_ESTIMATE = 29

type FacetSection = {
  title: string
  /** URL param this facet writes when a value is chosen. */
  key: string
  values: { value: string; label: string; count: number }[]
}

/**
 * Dense cross-project log list with store-backed facets and an adjacent record
 * inspector. Rows are virtualized: infinite scroll over the keyset cursor can
 * accumulate thousands of lines, and none of them may reach the DOM unrendered.
 */
export function LogExplorer({
  lines,
  environmentLabels = {},
  projectLabels = {},
  facets,
  facetsLoading,
  facetsError,
  onRetryFacets,
  onFilter,
  toolbar,
  footer,
  status,
  onInspect,
  onLoadMore,
  autoLoadMore = true,
  hasMore,
  isLoadingMore,
  histogram,
  attributesPanel,
}: {
  environmentLabels?: Record<string, string>
  /**
   * Names for project ids that appear in the facets, resolved across the whole
   * time range. Loaded lines alone can't name a project whose lines all fall
   * outside the current page.
   */
  projectLabels?: Record<string, string>
  lines: GlobalLogLine[]
  facets?: GlobalLogFacetsResponse
  facetsLoading?: boolean
  facetsError?: unknown
  onRetryFacets?: () => void
  onFilter: (patch: Patch) => void
  toolbar?: ReactNode
  footer?: ReactNode
  status?: ReactNode
  onInspect?: () => void
  onLoadMore?: () => void
  /** Partial searches pause at their budget boundary until the user resumes. */
  autoLoadMore?: boolean
  hasMore?: boolean
  isLoadingMore?: boolean
  /** Line-count histogram (ADR-047 §5), rendered above the toolbar. */
  histogram?: ReactNode
  /** Attribute facet sidebar (ADR-047 §5), rendered under the label facets. */
  attributesPanel?: ReactNode
}) {
  const [params, setParams] = useSearchParams()
  const mode =
    params.get('lv') === 'patterns'
      ? 'patterns'
      : params.get('lv') === 'service'
        ? 'service'
        : 'list'
  const columns = (params.get('cols') ?? 'deployment').split(',')
  const visibleColumns = columns.filter((column) =>
    EXTRA_COLUMNS.includes(column)
  )
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
  const isDesktop = useSyncExternalStore(
    subscribeDesktop,
    desktopSnapshot,
    serverDesktopSnapshot
  )
  const isMobile = useSyncExternalStore(
    subscribeMobile,
    mobileSnapshot,
    serverMobileSnapshot
  )
  const wrapPreference = params.get('wrap')
  const wrap = wrapPreference === '1' || (wrapPreference !== '0' && isMobile)
  const facetPreference = params.get('facets')
  const showFacets =
    facetPreference === '1' || (facetPreference !== '0' && isDesktop)
  useEffect(() => {
    if (selected) inspector.current?.focus()
  }, [selected])
  const line = lines.find((entry) => logLineKey(entry) === selected)

  // Facet values carry only an id and a count. Loaded lines are used purely to
  // put a human name on an id we already have — never to decide which values
  // exist or how many there are; that is the store's answer now. Ids with no
  // line on this page are named by `projectLabels`.
  const names = useMemo(() => {
    const projects = new Map<string, string>()
    const nodes = new Map<string, string>()
    const services = new Map<string, string>()
    for (const entry of lines) {
      if (entry.project_id != null)
        projects.set(String(entry.project_id), entry.owner)
      if (entry.external_service_id != null)
        services.set(String(entry.external_service_id), entry.owner)
      if (entry.node_id != null && entry.node_name)
        nodes.set(String(entry.node_id), entry.node_name)
    }
    return { projects, nodes, services }
  }, [lines])

  // Standalone database services store project_id = 0 because they have no
  // owning project. It is a storage sentinel, not a selectable project.
  const projectFacet = facetValues(facets, 'project_id').filter(
    (item) => item.value !== '0'
  )
  const serviceFacet = facetValues(facets, 'external_service_id').filter(
    (item) => item.value !== '0'
  )
  const total = (values: FacetValue[]) =>
    values.reduce((sum, item) => sum + item.count, 0)
  const map = (
    values: FacetValue[],
    label: (value: string) => string
  ): FacetSection['values'] =>
    values.map((item) => ({
      value: item.value,
      label: label(item.value),
      count: item.count,
    }))

  const facetSections: FacetSection[] = [
    {
      title: 'Level',
      key: 'level',
      values: map(facetValues(facets, 'level'), (value) => value),
    },
    {
      title: 'Project',
      key: 'project_id',
      values: map(
        projectFacet,
        (value) =>
          names.projects.get(value) ??
          projectLabels[value] ??
          `Project ${value}`
      ),
    },
    {
      title: 'Environment',
      key: 'env',
      values: map(facetValues(facets, 'env'), (value) =>
        logEnvironmentLabel(value, environmentLabels)
      ),
    },
    {
      title: 'Node',
      key: 'node_id',
      values: map(
        facetValues(facets, 'node_id'),
        (value) => names.nodes.get(value) ?? `Node ${value}`
      ),
    },
    {
      // Synthesized from the two ownership facets rather than a separate query:
      // "applications vs databases" is exactly project_id-set vs
      // external_service_id-set, and the counts are the store's, not the page's.
      title: 'Source',
      key: 'source',
      values: [
        {
          value: 'application',
          label: 'Applications',
          count: total(projectFacet),
        },
        { value: 'service', label: 'Databases', count: total(serviceFacet) },
      ].filter((item) => item.count > 0),
    },
    {
      title: 'Deployment',
      key: 'deploy_id',
      values: map(
        facetValues(facets, 'deploy_id'),
        (value) => `Deployment ${value}`
      ),
    },
  ]

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

  const scroller = useRef<HTMLDivElement>(null)
  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => scroller.current,
    estimateSize: () => ROW_ESTIMATE,
    overscan: 24,
    // Refs never attach during `renderToStaticMarkup` (no commit phase), so
    // `getScrollElement()` stays null forever there. The default zero-height
    // `initialRect` would then range-clip to nothing, silently rendering an
    // empty table before hydration ever runs. A non-zero guess keeps the
    // first paint (SSR or client) showing real rows instead of a blank body.
    initialRect: { width: 0, height: 620 },
  })
  const virtualRows = virtualizer.getVirtualItems()
  const paddingTop = virtualRows.length ? virtualRows[0].start : 0
  const paddingBottom = virtualRows.length
    ? virtualizer.getTotalSize() - virtualRows[virtualRows.length - 1].end
    : 0
  // Reaching the last virtual row means the user scrolled to the bottom of the
  // rope — walk the keyset cursor one page older. The footer keeps an explicit
  // button so this is reachable without a scroll wheel too.
  const lastIndex = virtualRows.length
    ? virtualRows[virtualRows.length - 1].index
    : -1
  useEffect(() => {
    if (mode !== 'list' || !autoLoadMore || !hasMore || isLoadingMore) return
    if (lines.length > 0 && lastIndex >= lines.length - 1) onLoadMore?.()
  }, [
    mode,
    autoLoadMore,
    hasMore,
    isLoadingMore,
    lastIndex,
    lines.length,
    onLoadMore,
  ])

  return (
    <div className="space-y-4">
      {histogram}
      <div
        className={cn(
          'grid min-w-0 items-start gap-5',
          (line || showFacets) && 'xl:grid-cols-[minmax(0,1fr)_264px]'
        )}
      >
        <section aria-label="Log explorer" className="min-w-0">
          {toolbar}
          <div className="mt-4 flex flex-wrap items-center justify-between gap-2 pb-3">
            <span className="text-xs text-muted-foreground">
              {lines.length} loaded {lines.length === 1 ? 'line' : 'lines'} ·
              newest first
            </span>
            <div className="flex flex-wrap items-center gap-1">
              <Button
                variant="ghost"
                size="sm"
                aria-expanded={showFacets && !line}
                aria-controls="log-facets"
                onClick={() => {
                  setSelected(undefined)
                  presentation('facets', showFacets && !line ? '0' : '1')
                }}
              >
                <ListFilter className="mr-1.5 size-3.5" />
                Filters
              </Button>
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
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-7 px-2 text-xs"
                  >
                    <Columns3 className="mr-1 size-3.5" />
                    Columns
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  {EXTRA_COLUMNS.map((column) => (
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
                onClick={() => presentation('wrap', wrap ? '0' : '1')}
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
          {status && <div className="mb-3">{status}</div>}
          {(!status || lines.length > 0) &&
            (mode !== 'list' ? (
              <div className="overflow-hidden rounded-md border">
                <p className="px-4 py-3 text-xs text-muted-foreground">
                  {mode === 'patterns' ? 'Exact repeated messages' : 'Services'}{' '}
                  on this loaded page. Select a row to inspect an example.
                </p>
                <Table className="table-fixed">
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
                      <TableRow
                        key={group.id}
                        className="border-0 even:bg-muted/20"
                      >
                        <TableCell className="max-w-sm">
                          <button
                            type="button"
                            className={cn(
                              'block w-full text-left font-mono text-[11px] hover:underline',
                              wrap
                                ? 'whitespace-pre-wrap break-all'
                                : 'truncate'
                            )}
                            onClick={(event) => {
                              opener.current = event.currentTarget
                              onInspect?.()
                              setSelected(logLineKey(group.example))
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
              <div
                ref={scroller}
                // The scroll container has to be *this* element so the virtualizer
                // can own it; neutralize the overflow the Table primitive adds.
                className="max-h-[62vh] overflow-auto rounded-md border [&>div]:overflow-visible"
              >
                <Table className="block w-full lg:table lg:table-fixed">
                  <TableHeader className="hidden bg-muted lg:sticky lg:top-0 lg:z-10 lg:table-header-group [&_th]:h-8 [&_th]:text-[10px] [&_th]:uppercase [&_th]:tracking-wide">
                    <TableRow>
                      <TableHead className="hidden w-24 lg:table-cell">
                        Time
                      </TableHead>
                      <TableHead className="w-16">Level</TableHead>
                      <TableHead className="hidden w-32 lg:table-cell">
                        Project / service
                      </TableHead>
                      <TableHead>Message</TableHead>
                      {visibleColumns.map((column) => (
                        <TableHead
                          key={column}
                          className="hidden w-24 xl:table-cell"
                        >
                          {column}
                        </TableHead>
                      ))}
                    </TableRow>
                  </TableHeader>
                  <TableBody className="block lg:table-row-group">
                    {paddingTop > 0 && (
                      <tr aria-hidden="true" className="block lg:table-row">
                        <td
                          className="block lg:table-cell"
                          colSpan={4 + visibleColumns.length}
                          style={{ height: paddingTop, padding: 0 }}
                        />
                      </tr>
                    )}
                    {virtualRows.map((virtualRow) => {
                      const entry = lines[virtualRow.index]
                      if (!entry) return null
                      const key = logLineKey(entry)
                      return (
                        <TableRow
                          key={key}
                          className="block px-3 py-2 lg:table-row lg:px-0 lg:py-0"
                          data-index={virtualRow.index}
                          ref={virtualizer.measureElement}
                          data-state={selected === key ? 'selected' : undefined}
                        >
                          <TableCell
                            className="hidden py-1.5 font-mono text-[11px] tabular-nums lg:table-cell"
                            title={new Date(entry.timestamp).toLocaleString()}
                          >
                            {new Date(entry.timestamp).toLocaleTimeString(
                              undefined,
                              { hour12: false, timeZone: 'UTC' }
                            )}
                          </TableCell>
                          <TableCell className="flex min-w-0 items-center gap-2 p-0 font-mono text-xs lg:table-cell lg:px-4 lg:py-1.5 lg:text-[11px]">
                            <LogLevelBadge level={entry.level} />
                            <span className="min-w-0 truncate text-muted-foreground lg:hidden">
                              {entry.owner} ·{' '}
                              {new Date(entry.timestamp).toLocaleTimeString()}
                            </span>
                          </TableCell>
                          <TableCell className="hidden py-1.5 lg:table-cell">
                            <p
                              className="truncate text-xs"
                              title={`${entry.owner} / ${entry.service}`}
                            >
                              {entry.owner} / {entry.service}
                            </p>
                          </TableCell>
                          <TableCell className="block min-w-0 p-0 lg:table-cell lg:px-4 lg:py-0.5">
                            <button
                              type="button"
                              aria-label={`Inspect log: ${entry.message}`}
                              aria-pressed={selected === key}
                              onClick={(event) => {
                                opener.current = event.currentTarget
                                onInspect?.()
                                setSelected(key)
                              }}
                              className={cn(
                                'block w-full rounded py-1 text-left font-mono text-base leading-6 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring lg:text-[11px] lg:leading-normal',
                                wrap
                                  ? 'whitespace-pre-wrap break-words'
                                  : 'truncate'
                              )}
                            >
                              <AnsiLogMessage message={entry.message} />
                            </button>
                          </TableCell>
                          {visibleColumns.map((column) => (
                            <TableCell
                              key={column}
                              className="hidden truncate py-1.5 font-mono text-[11px] text-muted-foreground xl:table-cell"
                            >
                              {(column === 'deployment'
                                ? entry.deploy_id
                                : column === 'node'
                                  ? entry.node_name
                                  : logEnvironmentLabel(
                                      entry.env,
                                      environmentLabels
                                    )) ?? '—'}
                            </TableCell>
                          ))}
                        </TableRow>
                      )
                    })}
                    {paddingBottom > 0 && (
                      <tr aria-hidden="true" className="block lg:table-row">
                        <td
                          className="block lg:table-cell"
                          colSpan={4 + visibleColumns.length}
                          style={{ height: paddingBottom, padding: 0 }}
                        />
                      </tr>
                    )}
                  </TableBody>
                </Table>
                {isLoadingMore && (
                  <p
                    role="status"
                    className="flex items-center justify-center gap-2 py-2 text-xs text-muted-foreground"
                  >
                    <Loader2 className="size-3 animate-spin" />
                    Loading older lines…
                  </p>
                )}
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
            <div className="mb-1">
              <LogLevelBadge level={line.level} />
            </div>
            <time
              className="text-xs text-muted-foreground"
              dateTime={line.timestamp}
            >
              {new Date(line.timestamp).toLocaleString()}
            </time>
            <pre className="my-3 max-h-64 overflow-auto whitespace-pre-wrap break-all rounded bg-muted p-3 text-xs">
              <AnsiLogMessage message={line.message} />
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
                [
                  'Environment',
                  logEnvironmentLabel(line.env, environmentLabels),
                ],
                ['Service', line.service],
                ['Stream', line.stream],
                ['Node', line.node_name],
                ['Container', line.container_id],
                ['Deployment', line.deploy_id],
                ['Line ID', line.line_id],
              ].map(([label, value]) => (
                <div key={String(label)}>
                  <dt className="text-muted-foreground">{label}</dt>
                  <dd className="break-all font-mono">{value ?? '—'}</dd>
                </div>
              ))}
            </dl>
            {line.fields != null && (
              <>
                <h3 className="mb-2 text-xs font-semibold">
                  Structured fields
                </h3>
                <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded bg-muted p-3 text-xs">
                  <HighlightedCode
                    code={JSON.stringify(line.fields, null, 2)}
                    language={'json'}
                  />
                </pre>
              </>
            )}
            <SurroundingLines key={line.line_id} line={line} />
          </aside>
        ) : showFacets ? (
          <aside
            id="log-facets"
            aria-label="Log facets"
            className="space-y-5 xl:sticky xl:top-4"
          >
            <div>
              <h2 className="text-sm font-semibold">Facets</h2>
              <p className="mt-1 text-xs text-muted-foreground">
                Counts across the whole time range, not just the loaded lines.
                Select a value to filter.
              </p>
            </div>
            {facets?.partial && (
              <p className="rounded border border-amber-500/40 bg-amber-500/5 p-2 text-[11px] text-amber-700 dark:text-amber-400">
                Some value lists were capped, so they are a prefix of the most
                common values rather than the complete set. Narrow the time
                range or the filters for an exact list.
              </p>
            )}
            {facetsError ? (
              <div className="space-y-2 border-t pt-3">
                <p className="text-xs text-muted-foreground">
                  Facet counts could not be loaded.
                </p>
                <Button
                  variant="outline"
                  size="sm"
                  className="h-7 text-xs"
                  onClick={onRetryFacets}
                >
                  Retry facets
                </Button>
              </div>
            ) : (
              <>
                <Input
                  aria-label="Filter facets"
                  placeholder="Filter facets"
                  value={facetSearch}
                  onChange={(event) => setFacetSearch(event.target.value)}
                  className="h-8 text-xs"
                />
                {facetsLoading && (
                  <p className="text-xs text-muted-foreground">
                    Loading facet counts…
                  </p>
                )}
                {facetSections.map((facet) => {
                  const facetTotal = Math.max(
                    1,
                    facet.values.reduce((sum, item) => sum + item.count, 0)
                  )
                  const visible = facet.values.filter((item) =>
                    `${facet.title} ${item.label} ${item.value}`
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
                        {visible.map((item) => (
                          <Button
                            key={item.value}
                            variant="ghost"
                            size="sm"
                            className="relative flex h-7 w-full justify-between gap-2 overflow-hidden rounded-none border-b px-2 text-[11px]"
                            onClick={() =>
                              onFilter({
                                [facet.key]: item.value,
                                ...(facet.key === 'source' &&
                                item.value === 'service'
                                  ? { project_id: undefined }
                                  : {}),
                              })
                            }
                          >
                            <span
                              aria-hidden="true"
                              className="pointer-events-none absolute inset-y-1 left-0 bg-muted/70"
                              style={{
                                width: `${(item.count / facetTotal) * 100}%`,
                              }}
                            />
                            <span className="relative truncate">
                              {item.label}
                            </span>
                            <span className="relative flex gap-3 tabular-nums text-muted-foreground">
                              {item.count}
                              <span className="w-8 text-right">
                                {Math.round((item.count / facetTotal) * 100)}%
                              </span>
                            </span>
                          </Button>
                        ))}
                      </section>
                    )
                  )
                })}
              </>
            )}
            {attributesPanel}
          </aside>
        ) : null}
      </div>
    </div>
  )
}

/**
 * The lines immediately around a match, fetched on demand.
 *
 * Addressed by the line's keyset identity — `(timestamp, container_id,
 * line_id)` — rather than a chunk offset. A well-formed key that matches no row
 * is a 404, which means the line aged out of retention; that is a real answer
 * and gets said out loud instead of rendering an empty panel.
 */
function SurroundingLines({ line }: { line: GlobalLogLine }) {
  // Mounted with `key={line.line_id}`, so selecting another line remounts this
  // panel collapsed instead of leaving the previous line's context expanded.
  const [open, setOpen] = useState(false)
  const containerId = line.container_id
  const query = useQuery({
    queryKey: ['log-context', line.timestamp, containerId, line.line_id],
    enabled: open && !!containerId,
    retry: false,
    queryFn: async ({ signal }) => {
      const result = await getLogContext({
        query: {
          timestamp: line.timestamp,
          container_id: containerId!,
          line_id: line.line_id,
          lines: 25,
        },
        signal,
      })
      if (result.response?.status === 404)
        throw new Error(
          'This log line is no longer available — it has aged out of retention.'
        )
      if (!result.data)
        throw new Error(
          (result.error as { detail?: string } | undefined)?.detail ??
            'Surrounding lines could not be loaded.'
        )
      return result.data
    },
  })

  if (!containerId)
    return (
      <p className="mt-4 border-t pt-3 text-xs text-muted-foreground">
        Surrounding lines need a container ID, which this line does not carry.
      </p>
    )

  return (
    <section aria-label="Surrounding lines" className="mt-4 border-t pt-3">
      <div className="flex items-center justify-between">
        <h3 className="text-xs font-semibold">Surrounding lines</h3>
        <Button
          variant="outline"
          size="sm"
          className="h-7 text-xs"
          aria-expanded={open}
          onClick={() => setOpen((value) => !value)}
        >
          {open ? 'Hide' : 'Show ±25 lines'}
        </Button>
      </div>
      {open && (
        <div className="mt-2">
          {query.isPending ? (
            <p
              role="status"
              className="flex items-center gap-2 text-xs text-muted-foreground"
            >
              <Loader2 className="size-3 animate-spin" />
              Loading surrounding lines…
            </p>
          ) : query.error ? (
            <div className="space-y-2">
              <p className="text-xs text-destructive">
                {(query.error as Error).message}
              </p>
              <Button
                variant="outline"
                size="sm"
                className="h-7 text-xs"
                onClick={() => void query.refetch()}
              >
                Retry surrounding lines
              </Button>
            </div>
          ) : (
            <pre className="max-h-72 overflow-auto rounded bg-muted p-2 font-mono text-[11px]">
              {query.data.lines.map((context, index) => (
                <div
                  key={context.line_id}
                  className={cn(
                    'whitespace-pre-wrap break-all px-1',
                    index === query.data.target_index
                      ? 'rounded bg-primary/10 font-semibold'
                      : 'opacity-70'
                  )}
                >
                  <span className="mr-2 text-muted-foreground">
                    {new Date(context.timestamp).toLocaleTimeString(undefined, {
                      hour12: false,
                      timeZone: 'UTC',
                    })}
                  </span>
                  <AnsiLogMessage message={context.message} />
                </div>
              ))}
            </pre>
          )}
        </div>
      )}
    </section>
  )
}
